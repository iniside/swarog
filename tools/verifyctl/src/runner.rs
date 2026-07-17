use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use processctl::{
    rollout_lock_path, EnvironmentSnapshot, OutputDestination, OwnedChild, OwnedLease,
    ProcessGroupPolicy, RolloutLock, ShutdownPolicy, SpawnSpec, WorkspaceLayout,
};
use rand::RngCore as _;

use crate::cli::{Action, Options};
use crate::model::{Outcome, StageResult, Summary};
use crate::stages;

const POLL: Duration = Duration::from_millis(25);
const SHUTDOWN: ShutdownPolicy = ShutdownPolicy {
    graceful_timeout: Duration::from_secs(1),
    force_timeout: Duration::from_secs(5),
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    Green = 0,
    Failed = 1,
    Orchestration = 2,
    Interrupted = 130,
}

pub fn execute(options: Options) -> Result<Exit> {
    if options.action == Action::Help {
        println!("{}", crate::cli::USAGE);
        return Ok(Exit::Green);
    }
    let snapshot = EnvironmentSnapshot::capture();
    let root = workspace_root()?;
    let run_id = run_id();
    std::fs::create_dir_all(root.join("run")).context("create shared rollout directory")?;

    if options.action != Action::Verify {
        let _lease = RolloutLock::acquire_exclusive(rollout_lock_path(&root), &run_id)
            .context("acquire shared rollout lease")?;
        return match options.action {
            Action::BlessPublicApi => stages::public_api::bless(&root),
            Action::BlessContractGolden => stages::contract_golden::bless(&root),
            Action::Verify | Action::Help => unreachable!("handled above"),
        };
    }

    let environment = FrozenEnvironment::from_snapshot(&snapshot);
    // One lease, lent to each of these roles in turn (one borrower alive at a
    // time — enforced by `BorrowedChild`'s borrow of the lease). splitproof is
    // the long-standing borrower; weles borrows the same rollout rather than
    // deadlocking on `run/rollout.lock` against this very lease.
    let mut lease = RolloutLock::acquire(rollout_lock_path(&root), &run_id, ["splitproof", "weles"])
        .context("acquire shared rollout lease")?;
    let log_dir = root.join("run").join("verify").join(&run_id);
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create run log directory {}", log_dir.display()))?;
    println!("[run-id] {run_id}");
    println!("[logs] {}", log_dir.display());

    install_interrupt_handler()?;
    let mut summary = Summary::default();
    let mut context = Context {
        root,
        log_dir,
        options,
        environment,
        lease: &mut lease,
        stage: crate::model::StageId::Build,
    };
    for stage in stages::manifest(options.level, options.strict) {
        context.stage = stage.id;
        println!("== {} ==", stage.id.name());
        let result = (stage.run)(&mut context);
        let outcome = stage_outcome(stage.id, result, |message| {
            eprintln!("verifyctl: {message}");
            if let Err(error) = context.note(message) {
                eprintln!(
                    "verifyctl: could not append {} stage error log: {error:#}",
                    stage.id.name()
                );
            }
        });
        println!("  {outcome}");
        summary.push(StageResult {
            id: stage.id,
            class: stage.class,
            outcome,
        });
        if interrupted() {
            summary.print();
            return Ok(Exit::Interrupted);
        }
    }
    summary.print();
    Ok(verdict(&summary, options.strict))
}

/// The run's exit, and the ONE place that decides it.
///
/// Extracted from [`execute`] because the fake-path harness can no longer reach
/// the GREEN branch. `weles-managed-gateway` boots the real split fleet, so in a
/// temp root whose `cargo` is a fixture that builds nothing it is honestly FAIL
/// (`tests/runner.rs`) — and one permanently-red BLOCKING row makes every fake
/// run exit 1 whatever else happened. The end-to-end pair that used to
/// discriminate the classes (an advisory FAIL is green; `--strict` turns it red)
/// collapsed with it.
///
/// So the discrimination lives HERE now, and is proven better than it was: the
/// matrix below fails every stage of the REAL manifest in turn, in both strict
/// modes, rather than the two stages the fake path happened to be able to drive.
pub(crate) fn verdict(summary: &Summary, strict: bool) -> Exit {
    if summary.failed(strict) {
        Exit::Failed
    } else {
        Exit::Green
    }
}

fn stage_outcome(
    id: crate::model::StageId,
    result: Result<Outcome>,
    report: impl FnOnce(&str),
) -> Outcome {
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let suffix = if interrupted() {
                " after interruption"
            } else {
                ""
            };
            let message = format!("stage {} errored{suffix}: {error:#}", id.name());
            report(&message);
            Outcome::Fail
        }
    }
}

pub struct Context<'a> {
    pub root: PathBuf,
    pub log_dir: PathBuf,
    pub options: Options,
    environment: FrozenEnvironment,
    lease: &'a mut OwnedLease,
    stage: crate::model::StageId,
}

impl Context<'_> {
    pub fn cargo(&mut self, label: &str, args: &[&str]) -> Result<Outcome> {
        self.cargo_os(label, &args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    pub fn cargo_os(&mut self, label: &str, args: &[OsString]) -> Result<Outcome> {
        let cargo = self
            .resolve("cargo")
            .context("cargo is not available on the captured PATH")?;
        self.command(label, cargo, args.to_vec())
    }

    pub fn resolve(&self, executable: &str) -> Option<PathBuf> {
        find_on_path(executable, &self.environment.build)
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment.build
    }

    pub fn database_url(&self) -> Option<&str> {
        self.environment
            .splitproof
            .get("DATABASE_URL")
            .map(String::as_str)
    }

    /// The frozen environment for a child that BOOTS A FLEET: the build
    /// environment plus the captured `DATABASE_URL`. Exactly what
    /// [`Context::splitproof`] hands its child — a fleet-booting stage needs the
    /// same values, and re-deriving them would be a second authority for what a
    /// rollout child's world is.
    pub fn rollout_environment(&self) -> &BTreeMap<String, String> {
        &self.environment.splitproof
    }

    /// Lends this run's ONE lease to a child claiming `role`.
    ///
    /// The roles are frozen at acquire above (`["splitproof", "weles"]`), and
    /// `spawn_borrower` refuses any other — so this is not a widening: it is
    /// `Context::splitproof`'s borrow, reachable by the stage that needs a
    /// DIFFERENT role. A stage may not acquire the lock itself: verifyctl holds
    /// it for the whole manifest, so an acquire would deadlock against this very
    /// lease.
    ///
    /// The returned child borrows `self` mutably for its life — one borrower
    /// alive at a time, enforced by the borrow checker rather than by review.
    /// A stage therefore resolves its paths and environment BEFORE calling this
    /// and reports its findings after the child is gone.
    pub fn borrow_rollout<'child>(
        &'child mut self,
        spec: SpawnSpec,
        role: &str,
    ) -> Result<processctl::BorrowedChild<'child>> {
        Ok(self.lease.spawn_borrower(spec, role)?)
    }

    pub fn stage_log(&self, label: &str, stream: &str) -> PathBuf {
        self.command_log(label, stream)
    }

    pub fn command(
        &mut self,
        label: &str,
        executable: PathBuf,
        args: Vec<OsString>,
    ) -> Result<Outcome> {
        self.command_at(label, executable, args, self.root.clone())
    }

    pub fn command_at(
        &mut self,
        label: &str,
        executable: PathBuf,
        args: Vec<OsString>,
        cwd: PathBuf,
    ) -> Result<Outcome> {
        let stdout = self.command_log(label, "out");
        let stderr = self.command_log(label, "err");
        let mut child = OwnedChild::spawn(SpawnSpec {
            label: format!("verify-{}-{label}", self.stage.name()),
            executable,
            args,
            env: os_environment(&self.environment.build),
            cwd,
            stdout: OutputDestination::File(stdout),
            stderr: OutputDestination::File(stderr),
            process_group: ProcessGroupPolicy::Owned,
        })?;
        wait_owned(&mut child, &self.command_log(label, "cleanup"))
    }

    pub fn command_code(
        &mut self,
        label: &str,
        executable: PathBuf,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<Option<i32>> {
        let mut child = OwnedChild::spawn(SpawnSpec {
            label: format!("verify-{}-{label}", self.stage.name()),
            executable,
            args,
            env: os_environment(&self.environment.build),
            cwd: self.root.clone(),
            stdout: OutputDestination::File(self.command_log(label, "out")),
            stderr: OutputDestination::File(self.command_log(label, "err")),
            process_group: ProcessGroupPolicy::Owned,
        })?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status.code());
            }
            if interrupted() || std::time::Instant::now() >= deadline {
                record_cleanup(
                    &self.command_log(label, "cleanup"),
                    child.shutdown(SHUTDOWN),
                );
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn splitproof(&mut self) -> Result<Outcome> {
        let executable = splitproof_executable(&self.root, &self.environment.build);
        if !executable.is_file() {
            self.note("splitproof executable was not produced by the build stage")?;
            return Ok(Outcome::Fail);
        }
        let stdout = self.command_log("splitproof", "out");
        let stderr = self.command_log("splitproof", "err");
        let cleanup = self.command_log("splitproof", "cleanup");
        let mut environment = self.environment.splitproof.clone();
        environment.insert("SPLITPROOF_SKIP_BUILD".into(), "1".into());
        let spec = SpawnSpec {
            label: "verify-splitproof".into(),
            executable,
            args: Vec::new(),
            env: os_environment(&environment),
            cwd: self.root.clone(),
            stdout: OutputDestination::File(stdout),
            stderr: OutputDestination::File(stderr),
            process_group: ProcessGroupPolicy::Owned,
        };
        let mut child = self.lease.spawn_borrower(spec, "splitproof")?;
        loop {
            if interrupted() {
                record_cleanup(&cleanup, child.shutdown(SHUTDOWN));
                return Ok(Outcome::Fail);
            }
            if let Some(status) = child.try_wait()? {
                return Ok(if status.success() {
                    Outcome::Pass
                } else {
                    Outcome::Fail
                });
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn note(&self, message: &str) -> Result<()> {
        let path = self.command_log("note", "log");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }

    fn command_log(&self, label: &str, stream: &str) -> PathBuf {
        self.log_dir
            .join(format!("{}-{label}.{stream}.log", self.stage.name()))
    }
}

fn wait_owned(child: &mut OwnedChild, cleanup_log: &Path) -> Result<Outcome> {
    loop {
        if interrupted() {
            record_cleanup(cleanup_log, child.shutdown(SHUTDOWN));
            return Ok(Outcome::Fail);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(if status.success() {
                Outcome::Pass
            } else {
                Outcome::Fail
            });
        }
        std::thread::sleep(POLL);
    }
}

fn workspace_root() -> Result<PathBuf> {
    let mut directory = std::env::current_dir()?;
    loop {
        if directory.join("Cargo.toml").is_file() && directory.join("tools/processctl").is_dir() {
            return Ok(directory);
        }
        if !directory.pop() {
            bail!("verifyctl must run inside the GameBackend workspace");
        }
    }
}

fn run_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut random = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut random);
    format!(
        "verify-{timestamp}-{}-{:08x}",
        std::process::id(),
        u32::from_le_bytes(random)
    )
}

#[derive(Clone)]
struct FrozenEnvironment {
    build: BTreeMap<String, String>,
    splitproof: BTreeMap<String, String>,
}

impl FrozenEnvironment {
    fn from_snapshot(snapshot: &EnvironmentSnapshot) -> Self {
        let build = snapshot.build_environment();
        let mut splitproof = build.clone();
        if let Some(database_url) = snapshot.value("DATABASE_URL") {
            splitproof.insert("DATABASE_URL".into(), database_url.into());
        }
        Self { build, splitproof }
    }
}

pub(crate) fn os_environment(environment: &BTreeMap<String, String>) -> BTreeMap<OsString, OsString> {
    environment
        .iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
}

fn splitproof_executable(root: &Path, environment: &BTreeMap<String, String>) -> PathBuf {
    WorkspaceLayout::from_root(root.to_path_buf(), environment).binary("debug", "splitproof")
}

fn find_on_path(name: &str, environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path = environment_value(environment, "PATH")?;
    let extensions: Vec<OsString> = if cfg!(windows) {
        environment_value(environment, "PATHEXT")
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .split(';')
            .map(OsString::from)
            .collect()
    } else {
        vec![OsString::new()]
    };
    for directory in std::env::split_paths(OsString::from(path).as_os_str()) {
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{}", extension.to_string_lossy()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn environment_value<'a>(environment: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    environment
        .iter()
        .find(|(candidate, _)| {
            if cfg!(windows) {
                candidate.eq_ignore_ascii_case(key)
            } else {
                candidate.as_str() == key
            }
        })
        .map(|(_, value)| value.as_str())
}

fn record_cleanup<E: std::fmt::Display>(
    path: &Path,
    result: std::result::Result<processctl::ShutdownOutcome, E>,
) {
    if let Err(error) = result {
        eprintln!("verifyctl: interrupted cleanup failed: {error}");
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "interrupted cleanup failed: {error}");
        }
    }
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(target_os = "linux")]
fn install_interrupt_handler() -> Result<()> {
    unsafe extern "C" fn handler(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    let result = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
    if result == libc::SIG_ERR {
        bail!(
            "install SIGINT handler: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn install_interrupt_handler() -> Result<()> {
    unsafe extern "system" fn handler(kind: u32) -> i32 {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
        if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
            INTERRUPTED.store(true, Ordering::SeqCst);
            1
        } else {
            0
        }
    }
    let ok =
        unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1) };
    if ok == 0 {
        bail!(
            "install console interrupt handler: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn install_interrupt_handler() -> Result<()> {
    bail!("verifyctl supports only Windows and Linux")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StageClass, StageId};

    /// A summary shaped like a real run of `level`/`strict`: every stage of the
    /// real manifest, carrying its REAL class, with the outcome `outcome` gives
    /// it. Classes are not restated here — a copy would drift from
    /// `stages::manifest`, and the class is the whole input to the verdict.
    fn manifest_summary(
        level: crate::cli::Level,
        strict: bool,
        outcome: impl Fn(StageId) -> Outcome,
    ) -> Summary {
        let mut summary = Summary::default();
        for stage in stages::manifest(level, strict) {
            summary.push(StageResult {
                id: stage.id,
                class: stage.class,
                outcome: outcome(stage.id),
            });
        }
        summary
    }

    /// The green/advisory/strict discrimination the fake path used to prove by
    /// exit code and cannot any more (`weles-managed-gateway` is honestly FAIL in
    /// a temp root, so every fake run exits 1 — see `verdict` and
    /// `tests/runner.rs`).
    ///
    /// Stronger than the pair it replaces: it fails EVERY stage of the real
    /// manifest in turn, so each stage's blocking-ness is proven individually,
    /// where the fake path could only ever drive the handful its fixture had
    /// controls for.
    #[test]
    fn the_verdict_is_green_exactly_when_no_blocking_stage_failed() {
        let all_green = manifest_summary(crate::cli::Level::Slow, true, |_| Outcome::Pass);
        assert_eq!(verdict(&all_green, false), Exit::Green);
        assert_eq!(verdict(&all_green, true), Exit::Green);

        for stage in stages::manifest(crate::cli::Level::Slow, true) {
            let one_red = manifest_summary(crate::cli::Level::Slow, true, |id| {
                if id == stage.id {
                    Outcome::Fail
                } else {
                    Outcome::Pass
                }
            });
            let advisory = stage.class == StageClass::Advisory;
            assert_eq!(
                verdict(&one_red, false),
                if advisory { Exit::Green } else { Exit::Failed },
                "{} FAIL without --strict",
                stage.id.name()
            );
            assert_eq!(
                verdict(&one_red, true),
                Exit::Failed,
                "{} FAIL under --strict",
                stage.id.name()
            );
        }
    }

    /// A skipped stage is not a failure — including the BLOCKING `audit` skip the
    /// fake path drives with `--no-install --strict`, whose exit-0 assertion the
    /// permanently-red live stage took with it.
    #[test]
    fn a_skip_never_makes_the_run_red_even_under_strict() {
        let skipped = manifest_summary(crate::cli::Level::All, true, |id| {
            if id == StageId::Audit {
                Outcome::Skip(crate::model::SkipReason::ExplicitNoInstallMissingTool)
            } else {
                Outcome::Pass
            }
        });
        assert_eq!(verdict(&skipped, true), Exit::Green);
    }

    #[test]
    fn stage_error_is_a_logged_failure_outcome() {
        let mut reported = String::new();
        let outcome = stage_outcome(
            crate::model::StageId::PublicApi,
            Err(anyhow::anyhow!("fixture stage error")),
            |message| reported.push_str(message),
        );
        assert_eq!(outcome, Outcome::Fail);
        assert!(reported.contains("stage public-api errored"));
        assert!(reported.contains("fixture stage error"));
    }

    #[test]
    fn path_lookup_uses_fake_executable() {
        let dir = std::env::temp_dir().join(format!("verifyctl-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("cargo-audit{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&fake, b"fake").unwrap();
        let environment = BTreeMap::from([("PATH".into(), dir.to_string_lossy().into_owned())]);
        assert_eq!(
            find_on_path("cargo-audit", &environment)
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            fake.file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frozen_snapshot_ignores_poison_ambient_and_resolves_relative_target() {
        let root = PathBuf::from("workspace-root");
        let snapshot = EnvironmentSnapshot::from_values([
            ("PATH".into(), "captured-path".into()),
            ("PATHEXT".into(), ".EXE".into()),
            ("CARGO_TARGET_DIR".into(), "frozen-target".into()),
            ("DATABASE_URL".into(), "postgres://typed".into()),
            ("VERIFYCTL_POISON".into(), "must-not-pass".into()),
        ]);
        let frozen = FrozenEnvironment::from_snapshot(&snapshot);
        assert!(
            environment_value(&frozen.build, "PATH").is_some_and(|path| std::env::split_paths(
                OsString::from(path).as_os_str()
            )
            .any(|entry| entry == Path::new("captured-path")))
        );
        assert_eq!(
            frozen.splitproof.get("DATABASE_URL").map(String::as_str),
            Some("postgres://typed")
        );
        assert!(!frozen.build.contains_key("VERIFYCTL_POISON"));
        assert_eq!(
            splitproof_executable(&root, &frozen.build),
            root.join("frozen-target/debug")
                .join(format!("splitproof{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn cleanup_failure_fixture_does_not_change_interrupted_exit() {
        let path =
            std::env::temp_dir().join(format!("verifyctl-cleanup-{}.log", std::process::id()));
        record_cleanup::<&str>(&path, Err("fixture cleanup failure"));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("fixture cleanup failure"));
        assert_eq!(Exit::Interrupted as u8, 130);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn interruption_maps_to_stable_exit() {
        assert_eq!(Exit::Interrupted as u8, 130);
        assert_eq!(Exit::Green as u8, 0);
        assert_eq!(Exit::Failed as u8, 1);
        assert_eq!(Exit::Orchestration as u8, 2);
    }
}

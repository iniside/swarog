use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use processctl::{
    rollout_lock_path, OutputDestination, OwnedChild, OwnedLease, ProcessGroupPolicy, RolloutLock,
    ShutdownPolicy, SpawnSpec,
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
    match options.action {
        Action::Help => {
            println!("{}", crate::cli::USAGE);
            return Ok(Exit::Green);
        }
        Action::BlessPublicApi | Action::BlessContractGolden => {
            bail!("bless action recognized but not implemented until verifyctl step 6")
        }
        Action::Verify => {}
    }
    let root = workspace_root()?;
    let run_id = run_id();
    let log_dir = root.join("run").join("verify").join(&run_id);
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create run log directory {}", log_dir.display()))?;
    println!("[run-id] {run_id}");
    println!("[logs] {}", log_dir.display());

    let mut lease = RolloutLock::acquire(rollout_lock_path(&root), &run_id, "splitproof")
        .context("acquire shared rollout lease")?;
    install_interrupt_handler()?;
    let mut summary = Summary::default();
    let mut context = Context {
        root,
        log_dir,
        options,
        lease: &mut lease,
        stage: crate::model::StageId::Build,
    };
    for stage in stages::INITIAL {
        context.stage = stage.id;
        println!("== {} ==", stage.id.name());
        let outcome = (stage.run)(&mut context)?;
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
    Ok(if summary.failed(options.strict) {
        Exit::Failed
    } else {
        Exit::Green
    })
}

pub struct Context<'a> {
    pub root: PathBuf,
    pub log_dir: PathBuf,
    pub options: Options,
    lease: &'a mut OwnedLease,
    stage: crate::model::StageId,
}

impl Context<'_> {
    pub fn cargo(&mut self, label: &str, args: &[&str]) -> Result<Outcome> {
        self.cargo_os(label, &args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    pub fn cargo_os(&mut self, label: &str, args: &[OsString]) -> Result<Outcome> {
        let cargo = find_on_path("cargo").context("cargo is not available on PATH")?;
        self.command(label, cargo, args.to_vec())
    }

    pub fn command(
        &mut self,
        label: &str,
        executable: PathBuf,
        args: Vec<OsString>,
    ) -> Result<Outcome> {
        let stdout = self.command_log(label, "out");
        let stderr = self.command_log(label, "err");
        let mut child = OwnedChild::spawn(SpawnSpec {
            label: format!("verify-{}-{label}", self.stage.name()),
            executable,
            args,
            env: current_environment(),
            cwd: self.root.clone(),
            stdout: OutputDestination::File(stdout),
            stderr: OutputDestination::File(stderr),
            process_group: ProcessGroupPolicy::Owned,
        })?;
        wait_owned(&mut child)
    }

    pub fn splitproof(&mut self) -> Result<Outcome> {
        let executable = splitproof_executable(&self.root);
        if !executable.is_file() {
            self.note("splitproof executable was not produced by the build stage")?;
            return Ok(Outcome::Fail);
        }
        let stdout = self.command_log("splitproof", "out");
        let stderr = self.command_log("splitproof", "err");
        let spec = SpawnSpec {
            label: "verify-splitproof".into(),
            executable,
            args: Vec::new(),
            env: current_environment(),
            cwd: self.root.clone(),
            stdout: OutputDestination::File(stdout),
            stderr: OutputDestination::File(stderr),
            process_group: ProcessGroupPolicy::Owned,
        };
        let mut child = self.lease.spawn_borrower(spec, "splitproof")?;
        loop {
            if interrupted() {
                child.shutdown(SHUTDOWN)?;
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

    pub fn on_path(&self, executable: &str) -> bool {
        find_on_path(executable).is_some()
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

fn wait_owned(child: &mut OwnedChild) -> Result<Outcome> {
    loop {
        if interrupted() {
            child.shutdown(SHUTDOWN)?;
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

fn current_environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

fn splitproof_executable(root: &Path) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    target
        .join("debug")
        .join(format!("splitproof{}", std::env::consts::EXE_SUFFIX))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<OsString> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
            .to_string_lossy()
            .split(';')
            .map(OsString::from)
            .collect()
    } else {
        vec![OsString::new()]
    };
    for directory in std::env::split_paths(&path) {
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{}", extension.to_string_lossy()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn interrupted() -> bool {
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

    #[test]
    fn path_lookup_uses_fake_executable() {
        let dir = std::env::temp_dir().join(format!("verifyctl-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("cargo-audit{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&fake, b"fake").unwrap();
        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        assert_eq!(
            find_on_path("cargo-audit")
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
        if let Some(old) = old {
            std::env::set_var("PATH", old);
        } else {
            std::env::remove_var("PATH");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interruption_maps_to_stable_exit() {
        assert_eq!(Exit::Interrupted as u8, 130);
        assert_eq!(Exit::Green as u8, 0);
        assert_eq!(Exit::Failed as u8, 1);
        assert_eq!(Exit::Orchestration as u8, 2);
    }
}

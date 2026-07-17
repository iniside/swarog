#[cfg(windows)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(windows)]
use processctl::{OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownPolicy, SpawnSpec};

static VERIFY_RUN_LOCK: Mutex<()> = Mutex::new(());

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_verifyctl-fixture"))
}

fn verifyctl() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_verifyctl"))
}

struct FakeRun {
    root: PathBuf,
    bin: PathBuf,
    target: PathBuf,
    record: PathBuf,
}

impl FakeRun {
    fn new(label: &str, audit_present: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "verifyctl-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = root.join("bin");
        let target = root.join("target");
        std::fs::create_dir_all(target.join("debug")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        for document in ["README.md", "CLAUDE.md", "AGENTS.md"] {
            std::fs::write(root.join(document), "").unwrap();
        }
        for directory in [
            "api",
            "clients/csharp/Generated",
            "cmd",
            "core/edge",
            "run",
            "tools/processctl",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        let contract = root.join("api/fixture/api");
        std::fs::create_dir_all(&contract).unwrap();
        std::fs::write(
            contract.join("Cargo.toml"),
            "[package]\nname = 'fixtureapi'\nversion = '0.1.0'\n",
        )
        .unwrap();
        let baseline = root.join("docs/reference/public-api-baseline");
        std::fs::create_dir_all(&baseline).unwrap();
        std::fs::write(
            baseline.join("fixtureapi.txt"),
            "# cargo-public-api fixture\n",
        )
        .unwrap();
        // The committed ops-catalog artifact the codegen-freshness stage diffs against.
        // The fake cargo copies these exact bytes to its `--out` file, so the freshness
        // check compares equal -> PASS (mirrors the empty clients/csharp/Generated dir).
        let opscatalog_src = root.join("opscatalog/src");
        std::fs::create_dir_all(&opscatalog_src).unwrap();
        std::fs::write(
            opscatalog_src.join("generated.rs"),
            "// fixture ops catalog\n",
        )
        .unwrap();
        copy_as(&fixture(), &bin, "cargo");
        if audit_present {
            copy_as(&fixture(), &bin, "cargo-audit");
        }
        copy_as(&fixture(), &target.join("debug"), "splitproof");
        Self {
            record: root.join("record.log"),
            root,
            bin,
            target,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(verifyctl());
        command
            .current_dir(&self.root)
            .args(args)
            .env("PATH", &self.bin)
            .env("CARGO_TARGET_DIR", &self.target)
            .env("VERIFYCTL_POISON", "must-not-reach-child");
        command
    }
}

impl Drop for FakeRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The one stage a fake root cannot host, and the reason the exit code is no
/// longer this test's discriminator.
///
/// `weles-managed-gateway` BOOTS THE REAL SPLIT FLEET (`weles up split`, a live
/// gateway, the shared Postgres). A fake root is a temp directory whose `cargo`
/// is a fixture that builds nothing, so the stage's first question — did the
/// build stage produce a `weles` binary? — is honestly answered NO, and it
/// reports FAIL. **That is the stage being right, not the harness being
/// unlucky**, and it is not fixable from here:
///
/// * every other stage in the manifest is either pure (`weles-fleet-parity`,
///   `weles-wire-contract`) or delegates its I/O to a binary the fixture stands
///   in for (`cargo`, `cargo-audit`, `splitproof`) — which is exactly why a fake
///   `splitproof` that exits 0 yields `split-proof | PASS`. This stage keeps its
///   proof IN-PROCESS by design, so there is no binary boundary to fake at;
/// * staging a fixture `weles` only moves the failure to
///   `prep::Layout::discover` on a `deploy/` nothing deployed; faking that too
///   leads to a mock 12-service fleet, i.e. a second implementation of the very
///   contract the stage exists to check;
/// * a fixture/skip mode inside the stage is the green-SKIP-wearing-a-PASS shape
///   its own module doc refuses to copy, and it would hollow out the live proof
///   that `weles-fleet-parity`'s managed-gateway exclusion is charged against.
///
/// The cost is paid, not absorbed. One permanently-red BLOCKING row means every
/// fake run exits 1 whatever else happened, so:
///
/// * each scenario is discriminated by its FAIL ROW SET ([`fail_rows`]) instead —
///   strictly more informative than the exit code it replaces, and `pass`
///   asserting this row is the ONLY red is what "everything else went green" now
///   means here;
/// * the green/advisory/strict rules the exit pair used to carry belong to
///   `runner::verdict`'s matrix, which fails every stage of the real manifest in
///   turn in both strict modes — more than this path ever proved.
///
/// Residual gap, named rather than hidden: no test now runs the real binary to a
/// zero exit through the manifest. `Exit::Green as u8 == 0` and `main`'s
/// `ExitCode::from(exit as u8)` are the only unproven link, and exits 1/2/130
/// still pin that cast here.
const LIVE_ONLY_STAGE: &str = "weles-managed-gateway";

/// The stage right beside [`LIVE_ONLY_STAGE`] that a fake root ALSO cannot
/// satisfy — for a different reason.
///
/// `supported-targets` cross-target-typechecks `processctl`/`weles` via
/// `cargo check -p processctl -p weles --target <triple>`. The fake root is a
/// minimal temp workspace (`[workspace]\n`, no members) with no
/// `processctl`/`weles` crates on disk, so `cargo check -p processctl -p
/// weles` genuinely has nothing to check — the fixture `cargo` stands in for
/// I/O, not for "these packages exist". This is the stage correctly reporting
/// FAIL against an honestly-incomplete tree, the same shape as
/// [`LIVE_ONLY_STAGE`]: a live-repo stage that a synthetic fake root cannot
/// host. It runs BEFORE `weles-managed-gateway` in the `BLOCKING` manifest
/// (`tools/verifyctl/src/stages/mod.rs`), so it appears first in every
/// fail-row set below.
const LIVE_ONLY_STAGE_2: &str = "supported-targets";

/// The OTHER stage a fake root cannot satisfy — and the reason the row sets below
/// are worth reading twice.
///
/// The fake `PATH` holds one directory of fixtures and no `dotnet`, so
/// `csharp::run` reports FAIL rather than a green SKIP whenever the run may
/// install (the same fail-closed rule as `audit`; `--no-install` is what turns it
/// into an honest SKIP). It has always done this on the fake path. Nothing
/// noticed, because it is ADVISORY and the assertion here was `exit == 0` —
/// which an advisory FAIL correctly does not change. Only the row set says it
/// out loud.
///
/// Note the contrast that makes this stage's placement matter: an advisory
/// live-only stage costs the fake path nothing, while a BLOCKING one
/// ([`LIVE_ONLY_STAGE`]) costs it every exit assertion it had.
///
/// Host-dependent since the Step 11 port: csharp-client is declared
/// not-applicable on macOS (msquic ships no macOS build), so THERE the runner
/// short-circuits it to a platform SKIP before ever resolving `dotnet` — it is
/// not a FAIL row on macOS. On Windows/Linux it still runs and FAILs. Use
/// [`csharp_fail_rows`] rather than the bare constant in a fail-row set.
const NO_DOTNET_STAGE: &str = "csharp-client";

/// The csharp-client contribution to a fail-row set — empty on macOS (a declared
/// platform SKIP), `[NO_DOTNET_STAGE]` elsewhere (an honest FAIL when `dotnet` is
/// missing and the run may install). Mirrors `model::Platform::current`'s macOS
/// arm.
fn csharp_fail_rows() -> Vec<&'static str> {
    if std::env::consts::OS == "macos" {
        Vec::new()
    } else {
        vec![NO_DOTNET_STAGE]
    }
}

/// The stages the summary table reported FAIL for, in table order.
///
/// Reads the rendered table, not the exit code: with [`LIVE_ONLY_STAGE`]
/// permanently red, `assert_exit(_, 1)` is satisfied by that row alone and can
/// no longer tell one scenario's failure from another. This can.
fn fail_rows(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip_while(|line| !line.starts_with("=== verify summary ==="))
        .filter_map(|line| {
            let mut fields = line.split('|').map(str::trim);
            let stage = fields.next()?;
            (fields.next()? == "FAIL").then(|| stage.to_string())
        })
        .collect()
}

#[test]
fn fake_path_covers_outcomes_audit_install_lease_and_summary_exits() {
    // Poison-tolerant: this mutex only serializes test execution against the
    // shared fixture/PATH environment, it guards no shared data. A sibling
    // test panicking while holding it must not cascade-fail every other test
    // in this file via `PoisonError` — each test's own assertions are what
    // decides its result.
    let _serial = VERIFY_RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = FakeRun::new("pass", true);
    let output = pass.command(&[]).output().unwrap();
    assert_exit(&output, 1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The live-only stage is the ONLY red: this is what the old exit-0 assertion
    // said, said per row.
    assert_eq!(fail_rows(&stdout), [LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]);
    assert!(stdout.contains("build                | PASS"));
    assert!(stdout.contains("docs-current         | PASS"));
    assert!(stdout.contains("split-proof          | PASS"));
    assert!(std::fs::read_to_string(&pass.record)
        .unwrap()
        .contains("splitproof borrowed verify-"));
    assert!(!std::fs::read_to_string(&pass.record)
        .unwrap()
        .contains("POISON LEAKED"));
    assert!(std::fs::read_to_string(&pass.record)
        .unwrap()
        .contains("cargo-audit audit --ignore RUSTSEC-2023-0071"));
    let record = std::fs::read_to_string(&pass.record).unwrap();
    assert!(record.contains("cargo build --workspace --exclude verifyctl"));
    assert!(record.contains("cargo test --workspace --exclude verifyctl"));
    assert!(record.contains("cargo test -p verifyctl --target-dir"));
    assert!(record.contains(
        &pass
            .root
            .join("target/verifyctl-self")
            .display()
            .to_string()
    ));
    assert!(record.contains("splitproof skip-build 1"));

    let no_install = FakeRun::new("no-install", false);
    let output = no_install
        .command(&["--no-install", "--strict"])
        .output()
        .unwrap();
    assert_exit(&output, 1);
    let strict_stdout = String::from_utf8_lossy(&output.stdout);
    // A missing tool under `--no-install` is a SKIP row, not a FAIL row, even
    // under `--strict` — that it is also not a red EXIT is `verdict`'s matrix.
    assert_eq!(
        fail_rows(&strict_stdout),
        [LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]
    );
    assert!(strict_stdout.contains("audit                | SKIP"));
    assert!(strict_stdout.contains("public-api           | PASS"));
    assert!(strict_stdout.contains("fuzz                 | SKIP"));
    assert!(strict_stdout.contains("csharp-client        | SKIP"));
    assert!(strict_stdout.contains("topiccheck           | PASS"));

    let install = FakeRun::new("install", false);
    let output = install.command(&[]).output().unwrap();
    assert_exit(&output, 1);
    // The install SUCCEEDED: audit is green, and the live-only stage is again the
    // only red.
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        [LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]
    );
    assert!(std::fs::read_to_string(&install.record)
        .unwrap()
        .contains("install cargo-audit --locked"));
    assert!(!std::fs::read_to_string(&install.record)
        .unwrap()
        .contains("--version"));

    let install_fail = FakeRun::new("install-fail", false);
    let output = install_fail
        .command(&[])
        .env("RUSTFLAGS", "install-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    // A failed install is audit's OWN failure, and blocking. Without the row this
    // scenario would now be indistinguishable from the install that worked.
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        ["audit", LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]
    );

    let network = FakeRun::new("network", true);
    let output = network
        .command(&[])
        .env("RUSTFLAGS", "audit-network-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    // FAIL, never a green SKIP.
    assert!(String::from_utf8_lossy(&output.stdout).contains("audit                | FAIL"));
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        ["audit", LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]
    );

    let route_fail = FakeRun::new("route-fail", true);
    let output = route_fail
        .command(&[])
        .env("RUSTFLAGS", "route-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        ["routecheck", LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE]
    );

    // `--all` runs the advisory manifest and `--strict` promotes it. The pair
    // used to prove the promotion end to end (exit 0 vs 1); with a blocking row
    // permanently red both exit 1 and both render the same rows, so what survives
    // here is that each level composes the manifest and reaches the advisory
    // control at all. The promotion itself is `runner::verdict`'s matrix.
    let advisory = FakeRun::new("advisory-fail", true);
    let output = advisory
        .command(&["--all"])
        .env("RUSTFLAGS", "advisory-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    let advisory_stdout = String::from_utf8_lossy(&output.stdout);
    let mut expected = vec![LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE, "public-api"];
    expected.extend(csharp_fail_rows());
    assert_eq!(fail_rows(&advisory_stdout), expected);
    assert!(advisory_stdout.contains("public-api           | FAIL"));
    assert!(advisory_stdout.contains("topiccheck           | PASS"));

    let strict_advisory = FakeRun::new("strict-advisory-fail", true);
    let output = strict_advisory
        .command(&["--strict"])
        .env("RUSTFLAGS", "advisory-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    let mut expected = vec![LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE, "public-api"];
    expected.extend(csharp_fail_rows());
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        expected
    );

    let slow = FakeRun::new("slow-fail", true);
    let output = slow
        .command(&["--slow"])
        .env("RUSTFLAGS", "slow-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stdout).contains("mutants              | FAIL"));
    let mut expected = vec![LIVE_ONLY_STAGE_2, LIVE_ONLY_STAGE];
    expected.extend(csharp_fail_rows());
    expected.push("mutants");
    assert_eq!(
        fail_rows(&String::from_utf8_lossy(&output.stdout)),
        expected
    );

    let cli = Command::new(verifyctl())
        .arg("--fast")
        .arg("--all")
        .output()
        .unwrap();
    assert_exit(&cli, 2);

    interruption_cleans_child_and_releases_lease();
}

#[test]
fn verify_and_bless_actions_share_one_rollout_lock() {
    // Poison-tolerant: this mutex only serializes test execution against the
    // shared fixture/PATH environment, it guards no shared data. A sibling
    // test panicking while holding it must not cascade-fail every other test
    // in this file via `PoisonError` — each test's own assertions are what
    // decides its result.
    let _serial = VERIFY_RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (label, args) in [
        ("verify", &[][..]),
        ("public-api-bless", &["--bless-public-api"][..]),
        ("contract-golden-bless", &["--bless-contract-golden"][..]),
    ] {
        let run = FakeRun::new(label, true);
        let owner = processctl::RolloutLock::acquire_exclusive(
            processctl::rollout_lock_path(&run.root),
            "verifyctl-action-contention",
        )
        .unwrap();
        let output = run.command(args).output().unwrap();
        assert_exit(&output, 2);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("acquire shared rollout lease"),
            "{label} did not report shared rollout contention"
        );
        drop(owner);
    }
}

#[cfg(windows)]
#[test]
fn exact_owned_cleanup_leaves_decoy_server_alive() {
    let run = FakeRun::new("decoy-survival", true);
    let first_dir = run.root.join("owned");
    let decoy_dir = run.root.join("decoy");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&decoy_dir).unwrap();
    copy_as(&fixture(), &first_dir, "server");
    copy_as(&fixture(), &decoy_dir, "server");
    let spawn = |label: &str, executable: PathBuf| {
        OwnedChild::spawn(SpawnSpec {
            label: label.into(),
            executable,
            args: Vec::new(),
            env: [(OsString::from("RUSTFLAGS"), OsString::from("sleep-decoy"))]
                .into_iter()
                .collect(),
            cwd: run.root.clone(),
            stdout: OutputDestination::Null,
            stderr: OutputDestination::Null,
            process_group: ProcessGroupPolicy::Owned,
        })
        .unwrap()
    };
    let mut owned = spawn("owned-server", first_dir.join("server.exe"));
    let mut decoy = spawn("decoy-server", decoy_dir.join("server.exe"));
    std::thread::sleep(Duration::from_millis(100));
    owned
        .shutdown(ShutdownPolicy {
            graceful_timeout: Duration::from_millis(100),
            force_timeout: Duration::from_secs(2),
        })
        .unwrap();
    assert!(
        decoy.try_wait().unwrap().is_none(),
        "decoy server was killed"
    );
    decoy
        .shutdown(ShutdownPolicy {
            graceful_timeout: Duration::from_millis(100),
            force_timeout: Duration::from_secs(2),
        })
        .unwrap();
}

fn interruption_cleans_child_and_releases_lease() {
    let run = FakeRun::new("interrupt", true);
    let mut command = run.command(&[]);
    command.env("RUSTFLAGS", "sleep-build");
    prepare_interruptible(&mut command);
    let mut child = command.spawn().unwrap();
    wait_for_record(&run.record, "sleeping");

    assert!(matches!(
        processctl::RolloutLock::acquire_exclusive(
            processctl::rollout_lock_path(&run.root),
            "verifyctl-test-competing"
        ),
        Err(processctl::LeaseError::AlreadyOwned)
    ));

    send_interrupt(child.id());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "verifyctl did not stop"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(status.code(), Some(130));
    let lease = processctl::RolloutLock::acquire_exclusive(
        processctl::rollout_lock_path(&run.root),
        "verifyctl-test-after-interrupt",
    )
    .unwrap();
    drop(lease);
}

fn wait_for_record(path: &Path, needle: &str) {
    let started = Instant::now();
    loop {
        if std::fs::read_to_string(path)
            .ok()
            .is_some_and(|text| text.contains(needle))
        {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "fixture did not report {needle}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn prepare_interruptible(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn prepare_interruptible(_command: &mut Command) {}

#[cfg(windows)]
fn send_interrupt(pid: u32) {
    let ok = unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
            windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
            pid,
        )
    };
    assert_ne!(
        ok,
        0,
        "GenerateConsoleCtrlEvent failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn send_interrupt(pid: u32) {
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
    assert_eq!(
        result,
        0,
        "kill(SIGINT) failed: {}",
        std::io::Error::last_os_error()
    );
}

fn copy_as(source: &Path, directory: &Path, name: &str) {
    let destination = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(source, destination).unwrap();
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The `weles-async-island` stage FAILS, through the real runner, on each
/// condition it exists to catch — and each failure is BLOCKING (exit 1).
///
/// The unit tests in `weles_async_island_tests.rs` pin the predicates over
/// synthetic trees; this pins that the stage is actually wired in, actually
/// blocking, and actually reads the tree it asked for. `island-no-control` is
/// the important one: it proves the stage refuses to pass when it can no longer
/// see the positive control it relies on, rather than going green-and-vacuous.
#[test]
fn weles_async_island_fails_on_a_banned_feature_and_on_a_vacuous_check() {
    // Poison-tolerant: this mutex only serializes test execution against the
    // shared fixture/PATH environment, it guards no shared data. A sibling
    // test panicking while holding it must not cascade-fail every other test
    // in this file via `PoisonError` — each test's own assertions are what
    // decides its result.
    let _serial = VERIFY_RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (label, control) in [
        // A sibling crate arming tokio's `process` feature: resolver-2 unifies
        // it into the weles binary, reaping children out from under try_wait.
        ("island-workspace-process", "island-workspace-process"),
        // weles's own resolve carrying `signal`.
        ("island-weles-signal", "island-weles-signal"),
        // cargo's rendering changed / the tree no longer covers the workspace:
        // the bans would match nothing, so the stage must FAIL, not pass.
        ("island-no-control", "island-no-control"),
        // cargo tree itself failed: an error, never "no findings".
        ("island-tree-fail", "tree-fail"),
    ] {
        let run = FakeRun::new(label, true);
        let output = run.command(&[]).env("RUSTFLAGS", control).output().unwrap();
        assert_exit(&output, 1);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("weles-async-island   | FAIL"),
            "{label}: expected the async-island stage to FAIL\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

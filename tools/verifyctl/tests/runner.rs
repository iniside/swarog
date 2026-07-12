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
            .current_dir(workspace_root())
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

#[test]
fn fake_path_covers_outcomes_audit_install_lease_and_summary_exits() {
    let _serial = VERIFY_RUN_LOCK.lock().unwrap();
    let pass = FakeRun::new("pass", true);
    let output = pass.command(&[]).output().unwrap();
    assert_exit(&output, 0);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("build                | PASS"));
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

    let no_install = FakeRun::new("no-install", false);
    let output = no_install
        .command(&["--no-install", "--strict"])
        .output()
        .unwrap();
    assert_exit(&output, 0);
    let strict_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(strict_stdout.contains("audit                | SKIP"));
    assert!(strict_stdout.contains("public-api           | PASS"));
    assert!(strict_stdout.contains("fuzz                 | SKIP"));
    assert!(strict_stdout.contains("csharp-client        | SKIP"));
    assert!(strict_stdout.contains("topiccheck           | PASS"));

    let install = FakeRun::new("install", false);
    let output = install.command(&[]).output().unwrap();
    assert_exit(&output, 0);
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

    let network = FakeRun::new("network", true);
    let output = network
        .command(&[])
        .env("RUSTFLAGS", "audit-network-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stdout).contains("audit                | FAIL"));

    let route_fail = FakeRun::new("route-fail", true);
    let output = route_fail
        .command(&[])
        .env("RUSTFLAGS", "route-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);

    let advisory = FakeRun::new("advisory-fail", true);
    let output = advisory
        .command(&["--all"])
        .env("RUSTFLAGS", "advisory-fail")
        .output()
        .unwrap();
    assert_exit(&output, 0);
    let advisory_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(advisory_stdout.contains("public-api           | FAIL"));
    assert!(advisory_stdout.contains("topiccheck           | PASS"));

    let strict_advisory = FakeRun::new("strict-advisory-fail", true);
    let output = strict_advisory
        .command(&["--strict"])
        .env("RUSTFLAGS", "advisory-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);

    let slow = FakeRun::new("slow-fail", true);
    let output = slow
        .command(&["--slow"])
        .env("RUSTFLAGS", "slow-fail")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stdout).contains("mutants              | FAIL"));

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
    let _serial = VERIFY_RUN_LOCK.lock().unwrap();
    let owner = processctl::RolloutLock::acquire_exclusive(
        processctl::rollout_lock_path(&workspace_root()),
        "verifyctl-action-contention",
    )
    .unwrap();

    for (label, args) in [
        ("verify", &[][..]),
        ("public-api-bless", &["--bless-public-api"][..]),
        ("contract-golden-bless", &["--bless-contract-golden"][..]),
    ] {
        let run = FakeRun::new(label, true);
        let output = run.command(args).output().unwrap();
        assert_exit(&output, 2);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("acquire shared rollout lease"),
            "{label} did not report shared rollout contention"
        );
    }

    drop(owner);
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
            cwd: workspace_root(),
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
            processctl::rollout_lock_path(&workspace_root()),
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
        processctl::rollout_lock_path(&workspace_root()),
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
#[cfg(windows)]
use std::sync::Once;
use std::time::{Duration, Instant};

use crate::{BorrowedLease, LeaseError, RolloutLock};
use crate::{
    FleetState, OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownOutcome, ShutdownPolicy,
    SpawnSpec, StateStore,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());
#[cfg(windows)]
static PROTECT_TEST_HARNESS: Once = Once::new();

#[test]
fn child_entry() {
    let Ok(mode) = std::env::var("PROCESSCTL_TEST_MODE") else {
        return;
    };
    match mode.as_str() {
        "exit" => {}
        "sleep" => {
            ready_from_env();
            std::thread::sleep(Duration::from_secs(60));
        }
        "ignore" => {
            ignore_graceful_signal();
            ready_from_env();
            std::thread::sleep(Duration::from_secs(60));
        }
        "tree" => {
            ignore_graceful_signal();
            let ready = PathBuf::from(std::env::var_os("PROCESSCTL_TEST_READY").unwrap());
            let grandchild_ready = ready.with_extension("grandchild");
            let grandchild = spawn_test_process("sleep", &grandchild_ready);
            std::fs::write(
                &ready,
                format!("{}\n{}", std::process::id(), grandchild.id()),
            )
            .unwrap();
            std::mem::forget(grandchild);
            std::thread::sleep(Duration::from_secs(60));
        }
        "root-graceful-descendant" => {
            let ready = PathBuf::from(std::env::var_os("PROCESSCTL_TEST_READY").unwrap());
            let grandchild_ready = ready.with_extension("grandchild");
            let grandchild = spawn_test_process("ignore", &grandchild_ready);
            wait_file(&grandchild_ready);
            std::fs::write(
                &ready,
                format!("{}\n{}", std::process::id(), grandchild.id()),
            )
            .unwrap();
            std::mem::forget(grandchild);
            std::thread::sleep(Duration::from_secs(60));
        }
        "handle-check" => {
            #[cfg(windows)]
            {
                use windows_sys::Win32::Foundation::GetHandleInformation;
                let handle = std::env::var("PROCESSCTL_TEST_SENTINEL_HANDLE")
                    .unwrap()
                    .parse::<usize>()
                    .unwrap() as *mut std::ffi::c_void;
                let mut flags = 0u32;
                let closed = unsafe { GetHandleInformation(handle, &mut flags) } == 0;
                std::fs::write(
                    std::env::var_os("PROCESSCTL_TEST_READY").unwrap(),
                    if closed { "closed" } else { "leaked" },
                )
                .unwrap();
            }
        }
        "nested-supervisor" => {
            let ready = PathBuf::from(std::env::var_os("PROCESSCTL_TEST_READY").unwrap());
            let inner_ready = ready.with_extension("inner");
            let mut inner = OwnedChild::spawn(spec("exit", &inner_ready)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while inner.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "nested child did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::write(ready, "nested-job-ok").unwrap();
        }
        "stdin-check" => {
            std::fs::write(
                std::env::var_os("PROCESSCTL_TEST_READY").unwrap(),
                if stdin_is_closed() {
                    "closed"
                } else {
                    "leaked"
                },
            )
            .unwrap();
        }
        "lease-borrower" => {
            let lease = BorrowedLease::consume_inherited("splitproof").unwrap();
            assert_eq!(lease.run_id(), "borrow-run");
            assert!(BorrowedLease::consume_inherited("splitproof").is_err());
            let ready = PathBuf::from(std::env::var_os("PROCESSCTL_TEST_READY").unwrap());
            let child_ready = ready.with_extension("child");
            let grandchild_ready = ready.with_extension("grandchild");
            let mut child = spawn_test_process("stdin-check", &child_ready);
            let mut grandchild = spawn_test_process("stdin-check", &grandchild_ready);
            wait_file(&child_ready);
            wait_file(&grandchild_ready);
            assert!(child.wait().unwrap().success());
            assert!(grandchild.wait().unwrap().success());
            assert_eq!(std::fs::read_to_string(child_ready).unwrap(), "closed");
            assert_eq!(std::fs::read_to_string(grandchild_ready).unwrap(), "closed");
            assert!(Command::new("cargo")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success());
            std::fs::write(ready, "borrowed-ok").unwrap();
        }
        "fd-check" => {
            #[cfg(target_os = "linux")]
            {
                let three = unsafe { libc::fcntl(3, libc::F_GETFD) };
                let three_error = std::io::Error::last_os_error().raw_os_error();
                let four = unsafe { libc::fcntl(4, libc::F_GETFD) };
                let four_error = std::io::Error::last_os_error().raw_os_error();
                let closed = three < 0
                    && three_error == Some(libc::EBADF)
                    && four < 0
                    && four_error == Some(libc::EBADF);
                std::fs::write(
                    std::env::var_os("PROCESSCTL_TEST_READY").unwrap(),
                    if closed { "closed" } else { "open" },
                )
                .unwrap();
            }
        }
        "supervisor-crash" => {
            let supervisor_ready =
                PathBuf::from(std::env::var_os("PROCESSCTL_TEST_SUPERVISOR_READY").unwrap());
            let tree_ready = PathBuf::from(std::env::var_os("PROCESSCTL_TEST_TREE_READY").unwrap());
            let mut owned = OwnedChild::spawn(spec("tree", &tree_ready)).unwrap();
            wait_file(&tree_ready);
            std::fs::write(supervisor_ready, owned.identity().pid.to_string()).unwrap();
            let _ = owned.try_wait();
            std::process::abort();
        }
        other => panic!("unknown child mode {other}"),
    }
}

#[test]
fn graceful_exit_and_repeated_shutdown_are_idempotent() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("graceful");
    let ready = dir.join("ready");
    let mut child = OwnedChild::spawn(spec("sleep", &ready)).unwrap();
    wait_file(&ready);
    let first = child.shutdown(policy(Duration::from_secs(3))).unwrap();
    assert!(matches!(first, ShutdownOutcome::Graceful(_)));
    let second = child.shutdown(policy(Duration::ZERO)).unwrap();
    assert!(matches!(second, ShutdownOutcome::AlreadyExited(_)));
}

#[test]
fn ignored_graceful_signal_forces_the_owned_process() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("force");
    let ready = dir.join("ready");
    let mut child = OwnedChild::spawn(spec("ignore", &ready)).unwrap();
    wait_file(&ready);
    let outcome = child.shutdown(policy(Duration::from_millis(150))).unwrap();
    assert!(matches!(outcome, ShutdownOutcome::Forced(_)));
}

#[test]
fn already_exited_child_is_observed_without_signalling() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("exited");
    let ready = dir.join("unused");
    let mut child = OwnedChild::spawn(spec("exit", &ready)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        child.shutdown(policy(Duration::ZERO)).unwrap(),
        ShutdownOutcome::AlreadyExited(_)
    ));
}

#[test]
fn force_kills_and_reaps_descendants_but_not_a_decoy() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("tree");
    let ready = dir.join("tree.ready");
    let decoy_ready = dir.join("decoy.ready");
    let mut decoy = spawn_test_process("sleep", &decoy_ready);
    wait_file(&decoy_ready);

    let mut child = OwnedChild::spawn(spec("tree", &ready)).unwrap();
    wait_file(&ready);
    let pids = read_pids(&ready);
    let outcome = child.shutdown(policy(Duration::from_millis(100))).unwrap();
    assert!(matches!(outcome, ShutdownOutcome::Forced(_)));
    wait_dead(pids[0]);
    wait_dead(pids[1]);
    assert!(process_alive(decoy.id()), "unrelated decoy was terminated");
    let _ = decoy.kill();
    let _ = decoy.wait();
}

#[test]
fn graceful_root_with_live_descendant_forces_job_remainder() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("graceful-descendant");
    let ready = dir.join("tree.ready");
    let mut child = OwnedChild::spawn(spec("root-graceful-descendant", &ready)).unwrap();
    wait_file(&ready);
    let pids = read_pids(&ready);
    let outcome = child.shutdown(policy(Duration::from_millis(150))).unwrap();
    assert!(matches!(outcome, ShutdownOutcome::Forced(_)));
    wait_dead(pids[0]);
    wait_dead(pids[1]);
}

#[test]
fn startup_handle_list_excludes_unrelated_inheritable_handle() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("handles");
    let ready = dir.join("ready");
    let sentinel = std::fs::File::create(dir.join("sentinel")).unwrap();
    let current = unsafe { GetCurrentProcess() };
    let mut inheritable = std::ptr::null_mut();
    assert_ne!(
        unsafe {
            DuplicateHandle(
                current,
                sentinel.as_raw_handle() as _,
                current,
                &mut inheritable,
                0,
                1,
                DUPLICATE_SAME_ACCESS,
            )
        },
        0
    );
    let mut child_spec = spec("handle-check", &ready);
    child_spec.env.insert(
        OsString::from("PROCESSCTL_TEST_SENTINEL_HANDLE"),
        OsString::from((inheritable as usize).to_string()),
    );
    let mut child = OwnedChild::spawn(child_spec).unwrap();
    unsafe { windows_sys::Win32::Foundation::CloseHandle(inheritable) };
    wait_file(&ready);
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "closed");
    while child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn process_already_inside_owned_job_can_create_nested_owned_job() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("nested-job");
    let ready = dir.join("ready");
    let mut supervisor = OwnedChild::spawn(spec("nested-supervisor", &ready)).unwrap();
    wait_file(&ready);
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "nested-job-ok");
    while supervisor.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn failed_post_spawn_checkpoint_reaps_new_child_and_started_prefix() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("checkpoint-rollback");
    let first_ready = dir.join("first.ready");
    let second_ready = dir.join("second.ready");
    let first = OwnedChild::spawn(spec("ignore", &first_ready)).unwrap();
    let second = OwnedChild::spawn(spec("ignore", &second_ready)).unwrap();
    wait_file(&first_ready);
    wait_file(&second_ready);
    let pids = [first.identity().pid, second.identity().pid];
    let mut started = vec![first, second];
    let store = StateStore::new(dir.join("missing-parent").join("fleet.json"));
    let state = FleetState::new("run-rollback", "split").unwrap();
    let error = store
        .checkpoint_or_rollback(&state, &mut started, policy(Duration::from_millis(100)))
        .unwrap_err();
    assert!(error.cleanup_failures.is_empty());
    wait_dead(pids[0]);
    wait_dead(pids[1]);
}

#[test]
fn inherited_borrower_is_one_shot_and_credential_is_not_reinherited() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("borrower");
    let lock_path = dir.join("rollout.lock");
    let mut owner = RolloutLock::acquire(&lock_path, "borrow-run", "splitproof").unwrap();
    let ready = dir.join("borrower.ready");
    assert!(matches!(
        owner.spawn_borrower(spec("lease-borrower", &ready), "wrong-role"),
        Err(LeaseError::WrongRole { .. })
    ));
    let mut borrower = owner
        .spawn_borrower(spec("lease-borrower", &ready), "splitproof")
        .unwrap();
    assert!(matches!(
        RolloutLock::acquire(&lock_path, "competing-run", "splitproof"),
        Err(LeaseError::AlreadyOwned)
    ));
    wait_file(&ready);
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "borrowed-ok");
    while borrower.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(borrower);
    assert!(!std::fs::read_dir(&dir).unwrap().any(|entry| entry
        .unwrap()
        .path()
        .extension()
        .is_some_and(|extension| extension == "borrowed")));
    assert!(matches!(
        owner.spawn_borrower(spec("lease-borrower", &ready), "splitproof"),
        Err(LeaseError::BorrowerAlreadyIssued)
    ));
}

#[test]
fn supervisor_crash_kills_owned_tree_and_preserves_decoy() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("crash");
    let supervisor_ready = dir.join("supervisor.ready");
    let tree_ready = dir.join("tree.ready");
    let decoy_ready = dir.join("decoy.ready");
    let mut decoy = spawn_test_process("sleep", &decoy_ready);
    wait_file(&decoy_ready);

    let mut supervisor = Command::new(std::env::current_exe().unwrap());
    supervisor
        .args(["--exact", "tests::child_entry", "--nocapture"])
        .env_clear()
        .env("PROCESSCTL_TEST_MODE", "supervisor-crash")
        .env("PROCESSCTL_TEST_SUPERVISOR_READY", &supervisor_ready)
        .env("PROCESSCTL_TEST_TREE_READY", &tree_ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut supervisor = supervisor.spawn().unwrap();
    wait_file(&supervisor_ready);
    let tree_pids = read_pids(&tree_ready);
    let status = supervisor.wait().unwrap();
    assert!(!status.success(), "crash helper unexpectedly succeeded");
    wait_dead(tree_pids[0]);
    wait_dead(tree_pids[1]);
    assert!(process_alive(decoy.id()), "unrelated decoy was terminated");
    let _ = decoy.kill();
    let _ = decoy.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn target_cannot_inherit_guardian_control_pipe_descriptors() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    protect_test_harness();
    let dir = test_dir("fds");
    let ready = dir.join("ready");
    let mut child = OwnedChild::spawn(spec("fd-check", &ready)).unwrap();
    wait_file(&ready);
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "closed");
    let _ = child.shutdown(policy(Duration::from_millis(100)));
}

fn spec(mode: &str, ready: &Path) -> SpawnSpec {
    let mut env = BTreeMap::new();
    env.insert(OsString::from("PROCESSCTL_TEST_MODE"), OsString::from(mode));
    env.insert(
        OsString::from("PROCESSCTL_TEST_READY"),
        ready.as_os_str().to_owned(),
    );
    for key in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }
    SpawnSpec {
        label: format!("test-{mode}"),
        executable: std::env::current_exe().unwrap(),
        args: vec![
            OsString::from("--exact"),
            OsString::from("tests::child_entry"),
            OsString::from("--nocapture"),
        ],
        env,
        cwd: std::env::current_dir().unwrap(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

fn spawn_test_process(mode: &str, ready: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::child_entry", "--nocapture"])
        .env_clear()
        .env("PROCESSCTL_TEST_MODE", mode)
        .env("PROCESSCTL_TEST_READY", ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn ready_from_env() {
    std::fs::write(
        std::env::var_os("PROCESSCTL_TEST_READY").unwrap(),
        std::process::id().to_string(),
    )
    .unwrap();
}

fn policy(graceful_timeout: Duration) -> ShutdownPolicy {
    ShutdownPolicy {
        graceful_timeout,
        force_timeout: Duration::from_secs(5),
    }
}

fn test_dir(name: &str) -> PathBuf {
    let unique = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("processctl-{name}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect()
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} stayed alive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return false;
    }
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(handle) };
    alive
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn ignore_graceful_signal() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn ignore(_: u32) -> i32 {
        1
    }
    assert_ne!(unsafe { SetConsoleCtrlHandler(Some(ignore), 1) }, 0);
}

fn stdin_is_closed() -> bool {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
}

#[cfg(windows)]
fn protect_test_harness() {
    use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT};
    unsafe extern "system" fn handler(event: u32) -> i32 {
        i32::from(event == CTRL_BREAK_EVENT)
    }
    PROTECT_TEST_HARNESS.call_once(|| {
        assert_ne!(unsafe { SetConsoleCtrlHandler(Some(handler), 1) }, 0);
    });
}

#[cfg(target_os = "linux")]
fn protect_test_harness() {}

#[cfg(target_os = "linux")]
fn ignore_graceful_signal() {
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
}

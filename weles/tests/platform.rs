//! Containment tests for `weles::platform`: they spawn the real `weles`
//! binary (via `CARGO_BIN_EXE_weles`) as the hidden `__test-child` fixture and
//! assert drop-kill, graceful-signal delivery, kill-tree, and shutdown
//! outcomes. All waits are poll-with-deadline loops (never racing a real
//! clock: deadlines only bound conditions that are guaranteed by
//! construction), and the scenarios are serialized behind one mutex.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use weles::platform::{spawn, Outcome, OwnedProc, SpawnSpec};

/// Serializes the containment scenarios: they share the console (Windows
/// CTRL_BREAK routing) and are individually timing-generous, so they must not
/// interleave.
fn sequential() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Fixture {
    proc: OwnedProc,
    stdout_path: PathBuf,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort temp cleanup; the OwnedProc drop (force + bounded reap)
        // runs first because `proc` is declared first.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn spawn_fixture(name: &str, extra_args: &[&str]) -> Fixture {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "weles-platform-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create fixture temp dir");
    let stdout_path = dir.join("stdout.log");
    let stdout = File::create(&stdout_path).expect("create fixture stdout log");
    let stderr = File::create(dir.join("stderr.log")).expect("create fixture stderr log");

    let mut args: Vec<OsString> = vec!["__test-child".into()];
    args.extend(extra_args.iter().map(OsString::from));
    let proc = spawn(SpawnSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_weles")),
        args,
        env: fixture_env(),
        cwd: Some(dir.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .expect("spawn __test-child fixture");
    Fixture {
        proc,
        stdout_path,
        dir,
    }
}

/// The COMPLETE fixture environment: minimal deliberate pass-through instead
/// of inheritance (SystemRoot is required by Win32 for a working child).
fn fixture_env() -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for key in ["SystemRoot", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }
    env
}

/// Polls the fixture's stdout log until it contains `needle`; panics with the
/// log contents on deadline.
fn wait_for_marker(fixture: &Fixture, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let contents = std::fs::read_to_string(&fixture.stdout_path).unwrap_or_default();
        if contents.contains(needle) {
            return contents;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?} in fixture stdout; got: {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn parse_grandchild_pid(stdout_contents: &str) -> u32 {
    stdout_contents
        .lines()
        .find_map(|line| line.strip_prefix("test-child: grandchild="))
        .expect("fixture must print the grandchild pid")
        .trim()
        .parse()
        .expect("grandchild pid must parse")
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} stayed alive");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Test-only crude liveness probe (production code never does PID lookups —
/// ownership there is the platform handle).
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    // SAFETY: probing an arbitrary pid; a null handle (gone / access denied)
    // is treated as dead, and the opened handle is closed before returning.
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
            0,
            pid,
        );
        if handle.is_null() {
            return false;
        }
        let alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        alive
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence/permission, sends nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[test]
fn drop_kills_the_child() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("drop-kills", &[]);
    wait_for_marker(&fixture, "test-child: ready");
    let pid = fixture.proc.pid();
    assert!(process_alive(pid), "fixture must be alive after ready");
    assert!(fixture.proc.try_wait().expect("try_wait").is_none());
    drop(fixture);
    wait_dead(pid);
}

#[test]
fn graceful_signal_reaches_the_child() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("graceful", &[]);
    wait_for_marker(&fixture, "test-child: ready");
    fixture.proc.graceful().expect("send graceful signal");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = fixture.proc.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not exit after graceful signal"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // Pins actual CTRL_BREAK / SIGTERM delivery: cooperative exit code 0 AND
    // the marker printed from the fixture's own graceful path.
    assert_eq!(status.code(), Some(0), "graceful exit must be code 0");
    wait_for_marker(&fixture, "test-child: graceful");
}

#[test]
fn force_kills_the_whole_tree() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("kill-tree", &["--spawn-grandchild"]);
    let contents = wait_for_marker(&fixture, "test-child: ready");
    let grandchild_pid = parse_grandchild_pid(&contents);
    let root_pid = fixture.proc.pid();
    assert!(process_alive(grandchild_pid), "grandchild must be alive");
    // The grandchild is inside the container by construction: children of a
    // job member join the job (no breakaway) / a plain fork stays in the
    // process group. Killing the container must reap BOTH.
    fixture.proc.force().expect("force the container");
    // force() deliberately does not reap (see its doc comment) — every real
    // caller follows it with a reap (shutdown()/Drop do). A force-killed-but-
    // unreaped root is a zombie on Unix, and `kill(pid, 0)` reports a zombie
    // as alive, so reap it here via try_wait the same way a real caller
    // would before checking liveness.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture.proc.try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root did not report exited after force()"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_dead(root_pid);
    wait_dead(grandchild_pid);
}

#[test]
fn shutdown_reports_graceful_for_a_cooperating_child() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("shutdown-graceful", &[]);
    wait_for_marker(&fixture, "test-child: ready");
    let outcome = fixture
        .proc
        .shutdown(Duration::from_secs(10), Duration::from_secs(10))
        .expect("shutdown");
    match outcome {
        Outcome::Graceful(status) => {
            assert_eq!(status.code(), Some(0), "cooperative exit must be code 0");
        }
        Outcome::Forced(status) => panic!("expected Graceful, got Forced({status:?})"),
    }
}

/// Pins F2's whole-unit exit semantics: shutdown() may report Graceful only
/// once the ENTIRE containment unit is done, so the (cooperative) grandchild
/// must already be dead — not merely dying — when Graceful comes back.
#[test]
fn whole_tree_is_gone_when_shutdown_reports_graceful() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("graceful-tree", &["--spawn-grandchild"]);
    let contents = wait_for_marker(&fixture, "test-child: ready");
    let grandchild_pid = parse_grandchild_pid(&contents);
    assert!(process_alive(grandchild_pid), "grandchild must be alive");
    let outcome = fixture
        .proc
        .shutdown(Duration::from_secs(15), Duration::from_secs(10))
        .expect("shutdown");
    match outcome {
        Outcome::Graceful(status) => {
            assert_eq!(status.code(), Some(0), "cooperative exit must be code 0");
        }
        Outcome::Forced(status) => panic!("expected Graceful, got Forced({status:?})"),
    }
    wait_dead(grandchild_pid);
}

/// Pins F1's kill/reap ordering on Unix: the root cooperates with SIGTERM but
/// the grandchild ignores it — observing the root's exit must SIGKILL-sweep
/// the group BEFORE the reap, so neither shutdown() nor drop() can orphan the
/// survivor. (Unix-only scenario: on Windows the job stays non-empty, so the
/// same input is a Forced outcome by design.)
#[cfg(unix)]
#[test]
fn shutdown_and_drop_reap_a_stubborn_grandchild() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("stubborn-grandchild", &["--stubborn-grandchild"]);
    let contents = wait_for_marker(&fixture, "test-child: ready");
    let grandchild_pid = parse_grandchild_pid(&contents);
    assert!(process_alive(grandchild_pid), "grandchild must be alive");
    let outcome = fixture
        .proc
        .shutdown(Duration::from_secs(15), Duration::from_secs(10))
        .expect("shutdown");
    assert!(
        matches!(outcome, Outcome::Graceful(_)),
        "root cooperates, so the outcome is Graceful (got {outcome:?})"
    );
    drop(fixture);
    wait_dead(grandchild_pid);
}

#[test]
fn shutdown_reports_forced_for_a_child_that_ignores_graceful() {
    let _guard = sequential();
    let mut fixture = spawn_fixture("shutdown-forced", &["--ignore-graceful"]);
    wait_for_marker(&fixture, "test-child: ready");
    // The 2s graceful window is not a race: the fixture NEVER exits on the
    // graceful signal by construction, so the window only bounds the wait.
    let outcome = fixture
        .proc
        .shutdown(Duration::from_secs(2), Duration::from_secs(10))
        .expect("shutdown");
    let pid = fixture.proc.pid();
    match outcome {
        Outcome::Forced(_) => {}
        Outcome::Graceful(status) => panic!("expected Forced, got Graceful({status:?})"),
    }
    wait_dead(pid);
}

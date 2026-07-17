//! Live containment tests for the macOS guardian (Step 7(d)).
//!
//! The guardian model re-execs the current binary as a supervisor, which then
//! spawns the target. A libtest binary cannot receive the guardian target through
//! its own argv (libtest owns argv), so — mirroring the Windows `child_entry`
//! pattern in `tests.rs` — the parent spawns `current_exe` to run exactly the
//! `guardian_testee` test with the target handed in via env and the fd-3 liveness
//! / fd-4 status pipes wired up in `pre_exec`. That child runs the REAL guardian
//! core (`super::run_supervised_for_test` → `supervise_target`): suspended
//! `posix_spawn`, kqueue supervise, `kill(-pgid)` teardown.

use std::ffi::OsString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::protocol::{read_frame, Frame};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

// The containment tests `fork` (Command + pre_exec), so they share the
// crate-wide fork/flock serializer with the reacquire-sensitive lock tests.
use crate::fork_flock_serial as serial;

/// The re-exec'd guardian body: driven only when the parent sets the marker env.
/// A normal `cargo test` run enters, sees no marker, and returns a passing no-op.
#[test]
fn guardian_testee() {
    if std::env::var_os("PROCESSCTL_GUARDIAN_TESTEE").is_none() {
        return;
    }
    let target = std::env::var_os("PROCESSCTL_GT_TARGET").expect("guardian testee target");
    let args: Vec<OsString> = std::env::var("PROCESSCTL_GT_ARGS")
        .unwrap_or_default()
        .split('\u{1f}')
        .filter(|part| !part.is_empty())
        .map(OsString::from)
        .collect();
    // fd 4 = status write end, fd 3 = liveness read end (established by the parent).
    let mut status = unsafe { File::from_raw_fd(4) };
    let code = super::run_supervised_for_test(&mut status, &target, &args);
    std::process::exit(code);
}

struct Testee {
    child: Child,
    live_write: Option<OwnedFd>,
    status_read: File,
    target_pid: u32,
}

impl Testee {
    fn guardian_pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Simulates supervisor death: closes the only liveness write end.
    fn drop_liveness(&mut self) {
        self.live_write.take();
    }

    /// Reads the guardian's terminal Completion frame: `(raw_wait_status, forced)`.
    fn read_completion(&mut self) -> (i32, bool) {
        match read_frame(&mut self.status_read).expect("completion frame") {
            Frame::Completion {
                raw_target_wait_status,
                forced_remainder,
            } => (raw_target_wait_status, forced_remainder),
            other => panic!("expected completion frame, got {other:?}"),
        }
    }
}

impl Drop for Testee {
    fn drop(&mut self) {
        self.live_write.take();
        let _ = self.child.wait();
    }
}

fn spawn_testee(target: &str, args: &[&str]) -> Testee {
    let (live_read, live_write) = pipe_cloexec();
    let (status_read, status_write) = pipe_cloexec();
    let live_read_fd = live_read.as_raw_fd();
    let status_write_fd = status_write.as_raw_fd();

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "guardian::darwin_tests::guardian_testee",
            "--nocapture",
        ])
        .env("PROCESSCTL_GUARDIAN_TESTEE", "1")
        .env("PROCESSCTL_GT_TARGET", target)
        .env("PROCESSCTL_GT_ARGS", args.join("\u{1f}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(live_read_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(status_write_fd, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // dup2 clears FD_CLOEXEC on the destination, but be explicit in case a
            // source pipe already sat at 3/4 (dup2(fd, fd) is a no-op that would
            // leave the cloexec flag set and close the fd across exec).
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 || libc::fcntl(4, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn guardian testee");
    drop(live_read);
    drop(status_write);

    let mut status_read = File::from(status_read);
    let target_pid = match read_frame(&mut status_read).expect("identity frame") {
        Frame::Identity(identity) => identity.pid,
        other => panic!("expected identity handshake, got {other:?}"),
    };
    Testee {
        child,
        live_write: Some(live_write),
        status_read,
        target_pid,
    }
}

fn pipe_cloexec() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [ends.0.as_raw_fd(), ends.1.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == 0,
            "set cloexec"
        );
    }
    ends
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} stayed alive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_alive(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !process_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} never became live");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "processctl-guardian-{name}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Supervisor death (liveness pipe EOF) must force-kill the target.
#[test]
fn supervisor_death_force_kills_the_target() {
    let _serial = serial();
    let mut testee = spawn_testee("/bin/sleep", &["30"]);
    let target = testee.target_pid;
    wait_alive(target);

    testee.drop_liveness();
    wait_dead(target);

    let (raw, _forced) = testee.read_completion();
    assert!(
        libc::WIFSIGNALED(raw) && libc::WTERMSIG(raw) == libc::SIGKILL,
        "a target with no cooperation must be SIGKILLed, raw={raw:#x}"
    );
}

/// `kill(-pgid)` on teardown must reach the whole process group, not just the
/// group leader — a grandchild the target forked into its group dies too.
#[test]
fn group_kill_reaches_a_forked_descendant() {
    let _serial = serial();
    let dir = test_dir("tree");
    let pidfile = dir.join("grandchild.pid");
    // The shell backgrounds a sleep into its own group, records the pid, and waits.
    let script = format!(
        "sleep 30 & echo $! > {}; wait",
        pidfile.to_string_lossy()
    );
    let mut testee = spawn_testee("/bin/sh", &["-c", &script]);
    let root = testee.target_pid;

    let deadline = Instant::now() + Duration::from_secs(10);
    while !pidfile.exists() {
        assert!(Instant::now() < deadline, "grandchild pidfile never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
    let grandchild: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .expect("grandchild pid");
    wait_alive(grandchild);

    testee.drop_liveness();
    wait_dead(root);
    wait_dead(grandchild);
    // Drain the completion frame so the guardian exits cleanly.
    let _ = testee.read_completion();
}

/// Teardown must not over-kill: an unrelated process outside the target's group
/// survives the guardian's `kill(-pgid)`.
#[test]
fn teardown_spares_an_unrelated_decoy() {
    let _serial = serial();
    let mut decoy = Command::new("/bin/sleep")
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn decoy");

    let mut testee = spawn_testee("/bin/sleep", &["30"]);
    let target = testee.target_pid;
    wait_alive(target);

    testee.drop_liveness();
    wait_dead(target);

    assert!(
        process_alive(decoy.id()),
        "the unrelated decoy was killed by the group teardown"
    );
    let _ = decoy.kill();
    let _ = decoy.wait();
    let _ = testee.read_completion();
}

/// A graceful signal to the guardian is forwarded to the target GROUP: the target
/// dies by SIGTERM (the forwarded signal), distinct from the SIGKILL of a force,
/// and a clean single-process exit leaves no forced group remainder.
#[test]
fn graceful_signal_is_forwarded_to_the_target() {
    let _serial = serial();
    let mut testee = spawn_testee("/bin/sleep", &["30"]);
    let target = testee.target_pid;
    wait_alive(target);

    // "graceful": SIGTERM to the guardian, which its kqueue forwards to -pgid.
    assert_eq!(
        unsafe { libc::kill(testee.guardian_pid(), libc::SIGTERM) },
        0,
        "signal guardian"
    );

    let (raw, forced) = testee.read_completion();
    assert!(
        libc::WIFSIGNALED(raw) && libc::WTERMSIG(raw) == libc::SIGTERM,
        "target should die by the FORWARDED SIGTERM, not a force SIGKILL, raw={raw:#x}"
    );
    assert!(
        !forced,
        "a single-process target leaves no live group remainder after reap"
    );
    wait_dead(target);
}

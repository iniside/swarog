//! The re-exec'd guardian: the private supervisor mode a consumer binary enters
//! via [`crate::dispatch_guardian_from_current_exe`]. It owns exactly one target
//! process, reports the target's post-exec identity back over the fd-4 status
//! pipe, watches the fd-3 liveness pipe for supervisor death, forwards graceful
//! signals, and force-kills the target's process group on demand — reporting a
//! final wait status and a `forced_remainder` bit over the same wire protocol
//! (`crate::protocol`) on every platform.
//!
//! The framing and lifecycle are shared; the containment mechanism is per-OS:
//!
//! * **Linux** (below, `cfg(target_os = "linux")`): `PR_SET_CHILD_SUBREAPER` +
//!   `setsid`, a `PTRACE_TRACEME`/`waitpid(WUNTRACED)` exec-boundary trap for
//!   identity capture, `signalfd` + a 3-fd `poll()` loop, `pidfd_open`, and
//!   `PR_SET_PDEATHSIG` so a SIGKILLed guardian takes its target down with it.
//!
//! * **macOS** (below, `cfg(target_os = "macos")`): a suspended `posix_spawn`
//!   (`POSIX_SPAWN_START_SUSPENDED | SETPGROUP | CLOEXEC_DEFAULT`) captures the
//!   post-exec image while the target is `SSTOP`, one `kqueue` (EVFILT_READ+EV_EOF
//!   on liveness, EVFILT_SIGNAL for the control signals, EVFILT_PROC/NOTE_EXIT on
//!   the target) replaces the poll loop, and plain `kill(2)` against the held pid
//!   replaces `pidfd`.
//!
//! ## Two guarantees macOS structurally cannot match (named, not discovered)
//!
//! 1. **A SIGKILLed guardian orphans its target.** Linux uses `PR_SET_PDEATHSIG`
//!    (SIGKILL) so a hard-killed guardian drops its target. macOS has no
//!    equivalent — the mechanism (a live watcher noticing the parent's death)
//!    needs a live watcher, and the dead guardian *was* it. The liveness pipe
//!    still catches ordinary *supervisor* death; only `SIGKILL` of the guardian
//!    itself leaks the target.
//! 2. **A `setsid()` escapee is unreachable.** Linux uses
//!    `PR_SET_CHILD_SUBREAPER` to adopt descendants that left the process group;
//!    `kill(-pgid)` plus adoption reaches the whole tree. macOS has no
//!    subreaper, so a descendant that `setsid()`s out of the group reparents to
//!    `launchd`, invisible to `kill(-pgid)` and to `proc_listchildpids`. This is
//!    exactly the `forced_adopted` half of `forced_remainder` that the macOS
//!    backend cannot compute — it reports `forced_group` only.

use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use crate::protocol::{write_frame, Frame};

const LIVENESS_FD: RawFd = 3;
const STATUS_FD: RawFd = 4;
pub(crate) const DISPATCH_ARG: &str = "--__processctl-guardian-v1";

pub(crate) fn run() -> i32 {
    if unsafe { libc::fcntl(STATUS_FD, libc::F_GETFD) } < 0 {
        eprintln!("processctl-guardian: status pipe is unavailable");
        return 1;
    }
    let mut status_pipe = unsafe { std::fs::File::from_raw_fd(STATUS_FD) };
    let result = run_inner(&mut status_pipe);
    report(&mut status_pipe, result)
}

/// Writes the terminal frame (Completion on success, GuardianFailed on error)
/// and maps the outcome to the process exit code. Shared by [`run`] and, under
/// test, by the macOS supervise-driver.
fn report(status_pipe: &mut std::fs::File, result: std::io::Result<(ExitStatus, bool)>) -> i32 {
    match result {
        Ok((status, forced_remainder)) => {
            let frame = Frame::Completion {
                raw_target_wait_status: status.into_raw(),
                forced_remainder,
            };
            if let Err(error) = write_frame(status_pipe, &frame) {
                eprintln!("processctl-guardian: write completion: {error}");
                1
            } else {
                0
            }
        }
        Err(error) => {
            let _ = write_frame(status_pipe, &Frame::GuardianFailed(error.to_string()));
            eprintln!("processctl-guardian: {error}");
            1
        }
    }
}

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

/// Blocks the control signals so the guardian receives them via its readiness
/// primitive (Linux `signalfd`, macOS `kqueue` EVFILT_SIGNAL) rather than taking
/// their default action. Portable POSIX; used by both backends.
fn block_control_signals() -> std::io::Result<libc::sigset_t> {
    let mut set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(set)
}

/// Fast pre-spawn check: has the supervisor already closed the liveness pipe?
/// Portable POSIX (`poll` for POLLHUP/POLLERR); used by both backends.
fn liveness_closed() -> std::io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: LIVENESS_FD,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    if unsafe { libc::poll(&mut fd, 1, 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd.revents & (libc::POLLHUP | libc::POLLERR) != 0)
}

/// Derive the `forced_group` remainder oracle and tear the target's process group
/// down in the ONE reuse-safe way (shared by both guardian backends).
///
/// **Invariant: `target_pid` must still be UNREAPED when this is called.** An
/// unreaped process (alive or zombie) pins its pid — and therefore the pgid it
/// leads, since it is its own group leader — so `kill(-target_pid)` here can only
/// ever reach the target's own group. The old code derived the oracle from a
/// `kill(-target_pid) == 0` *after* the reap, when the reap had already released
/// the pid and the pgid could have been recycled onto an unrelated group leader —
/// the one place the zombie-pinning guarantee did not hold (Step 7b).
///
/// The oracle is taken from the group enumeration `members` (excluding the target
/// itself — the target is a member of its own group but is not a "survivor"),
/// NOT from the kill's return value: after the reorder a post-kill enumeration
/// would be racy and a pre-reap `kill` returns 0 unconditionally (the unreaped
/// target is a valid signal target), so kill-return can no longer be the oracle.
/// Returns whether any non-target member survived the target's exit; the caller
/// reaps `target_pid` only AFTER this returns.
fn drain_group_before_reap(target_pid: i32, members: &[i32]) -> bool {
    let survived = members.iter().any(|&pid| pid > 0 && pid != target_pid);
    // pid still pinned by the unreaped target => this reaches only its own group.
    let _ = unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    survived
}

// ======================================================================== Linux

#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::process::{Child, Command};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
fn run_inner(status_pipe: &mut std::fs::File) -> std::io::Result<(ExitStatus, bool)> {
    let mut args = std::env::args_os();
    let _guardian = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(DISPATCH_ARG)) {
        return Err(invalid("guardian dispatch marker missing"));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(invalid("expected `-- <executable> [args...]`"));
    }
    let original_executable = args
        .next()
        .ok_or_else(|| invalid("missing target executable"))?;
    // Canonicalize only to validate existence with a precise error; hand the ORIGINAL
    // path to the child as argv[0]. A `cargo -> rustup` shim dispatches on argv[0]'s
    // basename, so exec'ing the resolved `rustup` path would make it run as rustup.
    // Mirrors the Windows split in platform/windows.rs:125-130.
    let resolved_executable = std::fs::canonicalize(&original_executable)?;
    let target_args: Vec<_> = args.collect();

    close_unrelated_fds()?;
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let signals = block_control_signals()?;
    let signal_fd = create_signal_fd(&signals)?;

    if liveness_closed()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "supervisor exited before target spawn",
        ));
    }
    set_cloexec(LIVENESS_FD)?;
    set_cloexec(STATUS_FD)?;

    let guardian_pid = unsafe { libc::getpid() };
    let mut command = Command::new(&resolved_executable);
    command.arg0(&original_executable);
    command.args(target_args);
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != guardian_pid {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "guardian exited during target spawn",
                ));
            }
            let mut empty = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ptrace(
                libc::PTRACE_TRACEME,
                0,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut target = command.spawn()?;
    let target_pid = target.id() as i32;
    let target_pidfd = match pidfd_open(target_pid) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            kill_and_reap_failed_spawn(&mut target, target_pid)?;
            return Err(error);
        }
    };
    if let Err(error) = wait_for_exec_trap(target_pid) {
        kill_and_reap_failed_spawn(&mut target, target_pid)?;
        return Err(error);
    }
    let target_started = match proc_start_marker(target_pid) {
        Ok(started) => started,
        Err(error) => {
            kill_and_reap_failed_spawn(&mut target, target_pid)?;
            return Err(error);
        }
    };
    let target_executable = match std::fs::read_link(format!("/proc/{target_pid}/exe")) {
        Ok(path) => path,
        Err(error) => {
            kill_and_reap_failed_spawn(&mut target, target_pid)?;
            return Err(error);
        }
    };

    let handshake = write_frame(
        status_pipe,
        &Frame::Identity(crate::ProcessIdentity {
            pid: target_pid as u32,
            executable: target_executable,
            started: crate::StartMarker(target_started),
        }),
    );
    if let Err(error) = handshake {
        kill_and_reap_failed_spawn(&mut target, target_pid)?;
        return Err(error);
    }
    if unsafe {
        libc::ptrace(
            libc::PTRACE_DETACH,
            target_pid,
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_void>(),
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        kill_and_reap_failed_spawn(&mut target, target_pid)?;
        return Err(error);
    }

    supervise(&mut target, target_pid, &target_pidfd, &signal_fd)
}

#[cfg(target_os = "linux")]
fn wait_for_exec_trap(target_pid: i32) -> std::io::Result<()> {
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(target_pid, &mut status, libc::WUNTRACED) };
        if waited == target_pid {
            if libc::WIFSTOPPED(status) && libc::WSTOPSIG(status) == libc::SIGTRAP {
                return Ok(());
            }
            return Err(std::io::Error::other(format!(
                "target did not stop at exec boundary (wait status {status:#x})"
            )));
        }
        if waited < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn kill_and_reap_failed_spawn(target: &mut Child, target_pid: i32) -> std::io::Result<()> {
    let _ = unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    let _ = target.wait();
    reap_descendants(Duration::from_secs(5)).map(|_| ())
}

#[cfg(target_os = "linux")]
fn supervise(
    target: &mut Child,
    target_pid: i32,
    target_pidfd: &OwnedFd,
    signal_fd: &OwnedFd,
) -> std::io::Result<(ExitStatus, bool)> {
    let mut force = false;
    loop {
        let mut pollfds = [
            libc::pollfd {
                fd: LIVENESS_FD,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
            libc::pollfd {
                fd: target_pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: signal_fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if pollfds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            force = true;
        }
        if pollfds[2].revents & libc::POLLIN != 0 {
            let signal = read_signal(signal_fd.as_raw_fd())?;
            if signal == libc::SIGUSR1 as u32 {
                force = true;
            } else if signal == libc::SIGTERM as u32 || signal == libc::SIGINT as u32 {
                let _ = unsafe { libc::kill(-target_pid, signal as i32) };
            }
        }
        if force || pollfds[1].revents & libc::POLLIN != 0 {
            // The target is still UNREAPED here (alive on the force path, a zombie
            // on the exit path), so its pid — and the pgid it leads — is pinned:
            // enumerate the group and force-kill it BEFORE the reap releases the
            // pid. Deriving `forced_group` from a post-reap kill risked signalling
            // a recycled pgid, and a pre-reap kill's return is not a valid oracle
            // (the unreaped target is itself a member), so the remainder comes from
            // the enumeration minus the target (Step 7b).
            let members = list_process_group(target_pid);
            let forced_group = drain_group_before_reap(target_pid, &members);
            let status = target.wait()?;
            // Then reap children reparented to this subreaper (the forced_adopted
            // half — a `setsid()` escapee `kill(-pgid)` could not reach).
            let forced_adopted = reap_descendants(Duration::from_secs(5))?;
            return Ok((status, forced_group || forced_adopted));
        }
    }
}

#[cfg(target_os = "linux")]
fn close_unrelated_fds() -> std::io::Result<()> {
    let mut fds = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        if let Ok(fd) = entry.file_name().to_string_lossy().parse::<RawFd>() {
            if fd > STATUS_FD {
                fds.push(fd);
            }
        }
    }
    for fd in fds {
        unsafe { libc::close(fd) };
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn create_signal_fd(set: &libc::sigset_t) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::signalfd(-1, set, libc::SFD_CLOEXEC) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(target_os = "linux")]
fn read_signal(fd: RawFd) -> std::io::Result<u32> {
    let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::signalfd_siginfo>();
    let read = unsafe { libc::read(fd, (&raw mut info).cast(), size) };
    if read == size as isize {
        Ok(info.ssi_signo)
    } else if read < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Err(invalid("short signalfd read"))
    }
}

#[cfg(target_os = "linux")]
fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn pidfd_open(pid: i32) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as RawFd };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(target_os = "linux")]
fn proc_start_marker(pid: i32) -> std::io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| invalid("malformed target /proc stat"))?;
    stat[close + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| invalid("missing target starttime"))?
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(target_os = "linux")]
fn reap_descendants(timeout: Duration) -> std::io::Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut forced_any = false;
    loop {
        forced_any |= kill_direct_children()? > 0;
        loop {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if waited > 0 {
                continue;
            }
            if waited == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(forced_any);
            }
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out draining guardian descendants",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Enumerate every member of process group `pgid` by scanning `/proc` for
/// processes whose stat `pgrp` (field 5) matches. Zombies are still listed (their
/// `/proc/<pid>/stat` survives until reaped), so a member that has exited but not
/// yet been reaped still counts — exactly what `drain_group_before_reap` needs to
/// decide `forced_group`. Best-effort: an unreadable/vanished entry is skipped.
#[cfg(target_os = "linux")]
fn list_process_group(pgid: i32) -> Vec<i32> {
    let mut members = Vec::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return members,
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        // pgrp is field 5 (1-indexed) of /proc/<pid>/stat; after the ')' closing
        // comm, split_whitespace yields state, ppid, pgrp, ... => nth(2). Same
        // parse shape as `proc_start_marker`.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        if stat[close + 1..]
            .split_whitespace()
            .nth(2)
            .and_then(|field| field.parse::<i32>().ok())
            == Some(pgid)
        {
            members.push(pid);
        }
    }
    members
}

#[cfg(target_os = "linux")]
fn kill_direct_children() -> std::io::Result<usize> {
    let path = format!("/proc/self/task/{}/children", unsafe { libc::getpid() });
    let children = std::fs::read_to_string(path)?;
    let mut signalled = 0;
    for pid in children
        .split_whitespace()
        .filter_map(|pid| pid.parse::<i32>().ok())
    {
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            signalled += 1;
        }
    }
    Ok(signalled)
}

// ======================================================================= macOS

#[cfg(target_os = "macos")]
use std::ffi::CString;
#[cfg(target_os = "macos")]
use std::os::fd::{AsRawFd, OwnedFd};

#[cfg(target_os = "macos")]
fn run_inner(status_pipe: &mut std::fs::File) -> std::io::Result<(ExitStatus, bool)> {
    let mut args = std::env::args_os();
    let _guardian = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(DISPATCH_ARG)) {
        return Err(invalid("guardian dispatch marker missing"));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(invalid("expected `-- <executable> [args...]`"));
    }
    let original_executable = args
        .next()
        .ok_or_else(|| invalid("missing target executable"))?;
    let target_args: Vec<_> = args.collect();
    supervise_target(status_pipe, &original_executable, &target_args)
}

/// The macOS containment core: spawn the target suspended, capture its post-exec
/// identity, hand it back over `status_pipe`, resume it, and supervise via kqueue
/// until it exits or a force is requested. Split from [`run_inner`] so the
/// test harness can drive it with an explicit target (which cannot come from a
/// libtest child's argv).
#[cfg(target_os = "macos")]
fn supervise_target(
    status_pipe: &mut std::fs::File,
    original_executable: &std::ffi::OsStr,
    target_args: &[std::ffi::OsString],
) -> std::io::Result<(ExitStatus, bool)> {
    // Canonicalize only to validate existence and to exec the resolved image;
    // argv[0] stays the ORIGINAL path so a `cargo -> rustup` shim still dispatches
    // on its basename (mirrors the Linux arm and platform/windows.rs:125-130).
    let resolved_executable = std::fs::canonicalize(original_executable)?;

    // Detach into a new session so the guardian is not a member of the target's
    // process group; the target gets its own group via POSIX_SPAWN_SETPGROUP.
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Block the control signals; the kqueue EVFILT_SIGNAL registrations below
    // observe them without their default action firing. No fd sweep is needed:
    // the target's POSIX_SPAWN_CLOEXEC_DEFAULT closes every inherited descriptor.
    let _signals = block_control_signals()?;

    if liveness_closed()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "supervisor exited before target spawn",
        ));
    }

    let target_pid = spawn_suspended(&resolved_executable, original_executable, target_args)?;

    // Register the watch — crucially EVFILT_PROC/NOTE_EXIT — BEFORE SIGCONT, so a
    // target that exits the instant it resumes cannot slip through the gap.
    let kq = match kqueue_watch(target_pid) {
        Ok(kq) => kq,
        Err(error) => {
            kill_and_reap_failed_target(target_pid);
            return Err(error);
        }
    };

    // The target is SSTOP (alive, pre-image-run): libproc reads the post-exec
    // image and start-time. Any read failure here means the spawn went wrong.
    let identity = match crate::platform::observe_process_identity(target_pid as u32) {
        Ok(identity) => identity,
        Err(error) => {
            kill_and_reap_failed_target(target_pid);
            return Err(error);
        }
    };
    if let Err(error) = write_frame(status_pipe, &Frame::Identity(identity)) {
        kill_and_reap_failed_target(target_pid);
        return Err(error);
    }
    if unsafe { libc::kill(target_pid, libc::SIGCONT) } != 0 {
        let error = std::io::Error::last_os_error();
        kill_and_reap_failed_target(target_pid);
        return Err(error);
    }

    supervise_kqueue(&kq, target_pid)
}

/// `posix_spawn` the target STOPPED at its first instruction, in its own process
/// group, with every inherited fd closed except a freshly re-established stdio
/// (0/1/2). Returns the raw pid; the caller holds it unreaped (a zombie pins its
/// pid — the pid-reuse invariant needs no handshake).
#[cfg(target_os = "macos")]
fn spawn_suspended(
    resolved_executable: &std::path::Path,
    argv0: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> std::io::Result<i32> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = cstring(resolved_executable.as_os_str().as_bytes())?;
    let mut argv_owned = Vec::with_capacity(args.len() + 1);
    argv_owned.push(cstring(argv0.as_bytes())?);
    for arg in args {
        argv_owned.push(cstring(arg.as_bytes())?);
    }
    let mut argv: Vec<*mut libc::c_char> = argv_owned
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    argv.push(std::ptr::null_mut());

    // The parent set the target's intended environment on THIS guardian via
    // env_clear().envs(), so the guardian's own environ is the target envp.
    let mut env_owned = Vec::new();
    for (key, value) in std::env::vars_os() {
        let mut kv = key.as_bytes().to_vec();
        kv.push(b'=');
        kv.extend_from_slice(value.as_bytes());
        if let Ok(entry) = CString::new(kv) {
            env_owned.push(entry);
        }
    }
    let mut envp: Vec<*mut libc::c_char> = env_owned
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    envp.push(std::ptr::null_mut());

    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        let rc = libc::posix_spawnattr_init(&mut attr);
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        let flags = (libc::POSIX_SPAWN_START_SUSPENDED
            | libc::POSIX_SPAWN_SETPGROUP
            | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_SETSIGMASK
            | libc::POSIX_SPAWN_SETSIGDEF) as libc::c_short;
        let rc = libc::posix_spawnattr_setflags(&mut attr, flags);
        if rc != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        // pgroup 0 => the target leads its own new process group (pid == pgid),
        // so kill(-pid) reaches the whole group.
        let rc = libc::posix_spawnattr_setpgroup(&mut attr, 0);
        if rc != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        // The Linux target is spawned via `std::process::Command`, which resets the
        // child BOTH ways before exec: an empty signal MASK *and* default signal
        // DISPOSITIONS (notably Rust's startup `SIGPIPE = SIG_IGN` back to
        // `SIG_DFL`). This raw `posix_spawn` must reproduce both, or the darwin
        // target silently diverges from its Linux twin — the exact monolith-parity
        // split this port exists to prevent.
        //
        // Mask: the guardian BLOCKS the control signals so its own kqueue observes
        // them; without an explicit empty mask the target would INHERIT that block
        // and silently ignore a forwarded SIGTERM/SIGINT.
        let mut empty_mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty_mask);
        let rc = libc::posix_spawnattr_setsigmask(&mut attr, &empty_mask);
        if rc != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        // Dispositions: reset to SIG_DFL at least SIGPIPE (the guardian process is
        // a Rust binary, so it carries the runtime's `SIGPIPE = SIG_IGN`, which a
        // raw spawn would otherwise leak into the target where a Linux `Command`
        // child gets `SIG_DFL`), plus the control signals the guardian forwards
        // (SIGTERM/SIGINT) and maps to force (SIGUSR1) — so the target has clean
        // default dispositions like a Linux `Command` child regardless of what
        // this guardian process has installed.
        let mut default_dispositions: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut default_dispositions);
        libc::sigaddset(&mut default_dispositions, libc::SIGPIPE);
        libc::sigaddset(&mut default_dispositions, libc::SIGTERM);
        libc::sigaddset(&mut default_dispositions, libc::SIGINT);
        libc::sigaddset(&mut default_dispositions, libc::SIGUSR1);
        let rc = libc::posix_spawnattr_setsigdefault(&mut attr, &default_dispositions);
        if rc != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(std::io::Error::from_raw_os_error(rc));
        }

        let mut facts: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        let rc = libc::posix_spawn_file_actions_init(&mut facts);
        if rc != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        // CLOEXEC_DEFAULT closes 0/1/2 too; re-establish the guardian's stdio (the
        // per-service log files the parent handed it) for the target to inherit.
        for fd in 0..3 {
            let rc = libc::posix_spawn_file_actions_adddup2(&mut facts, fd, fd);
            if rc != 0 {
                libc::posix_spawn_file_actions_destroy(&mut facts);
                libc::posix_spawnattr_destroy(&mut attr);
                return Err(std::io::Error::from_raw_os_error(rc));
            }
        }

        let mut pid: libc::pid_t = 0;
        let rc = libc::posix_spawn(
            &mut pid,
            c_path.as_ptr(),
            &facts,
            &attr,
            argv.as_ptr(),
            envp.as_ptr(),
        );
        libc::posix_spawn_file_actions_destroy(&mut facts);
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        Ok(pid)
    }
}

/// One kqueue watching liveness EOF, the three control signals, and the target's
/// NOTE_EXIT. Registered as a single changelist; caller must not have resumed the
/// target yet.
#[cfg(target_os = "macos")]
fn kqueue_watch(target_pid: i32) -> std::io::Result<OwnedFd> {
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let kq = unsafe { OwnedFd::from_raw_fd(kq) };
    let changes = [
        kevent_change(LIVENESS_FD as libc::uintptr_t, libc::EVFILT_READ, 0),
        kevent_change(libc::SIGTERM as libc::uintptr_t, libc::EVFILT_SIGNAL, 0),
        kevent_change(libc::SIGINT as libc::uintptr_t, libc::EVFILT_SIGNAL, 0),
        kevent_change(libc::SIGUSR1 as libc::uintptr_t, libc::EVFILT_SIGNAL, 0),
        kevent_change(
            target_pid as libc::uintptr_t,
            libc::EVFILT_PROC,
            libc::NOTE_EXIT,
        ),
    ];
    let rc = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            changes.as_ptr(),
            changes.len() as libc::c_int,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kq)
}

#[cfg(target_os = "macos")]
fn kevent_change(ident: libc::uintptr_t, filter: i16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// The kqueue supervise loop. Mirrors the Linux `supervise` decision structure:
/// liveness EOF or SIGUSR1 forces; SIGTERM/SIGINT are forwarded to the target
/// group; NOTE_EXIT ends it. On force or exit, enumerate the group and force-kill
/// it while the target is still UNREAPED (its pid — and the pgid it leads — pinned
/// against reuse), THEN reap — the reuse-safe order shared with Linux via
/// `drain_group_before_reap` (Step 7b). `forced_adopted` has no macOS analogue
/// (no subreaper), so the remainder is `forced_group` alone.
#[cfg(target_os = "macos")]
fn supervise_kqueue(kq: &OwnedFd, target_pid: i32) -> std::io::Result<(ExitStatus, bool)> {
    let mut force = false;
    loop {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let n = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                std::ptr::null(),
            )
        };
        if n < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if n == 0 {
            continue;
        }
        let mut target_exited = false;
        match event.filter {
            libc::EVFILT_READ if event.ident == LIVENESS_FD as libc::uintptr_t => {
                if event.flags & libc::EV_EOF != 0 {
                    force = true;
                }
            }
            libc::EVFILT_SIGNAL => {
                let signal = event.ident as i32;
                if signal == libc::SIGUSR1 {
                    force = true;
                } else if signal == libc::SIGTERM || signal == libc::SIGINT {
                    let _ = unsafe { libc::kill(-target_pid, signal) };
                }
            }
            libc::EVFILT_PROC
                if event.ident == target_pid as libc::uintptr_t
                    && event.fflags & libc::NOTE_EXIT != 0 =>
            {
                target_exited = true;
            }
            _ => {}
        }
        if force || target_exited {
            // The target is still UNREAPED here (alive on the force path, a zombie
            // after NOTE_EXIT), so its pid — and the pgid it leads — is pinned:
            // enumerate the group and force-kill it BEFORE the reap releases the
            // pid, deriving `forced_group` from the enumeration minus the target
            // rather than from a post-reap kill of a possibly-recycled pgid
            // (Step 7b). No `forced_adopted` half — macOS has no subreaper.
            let members = list_process_group(target_pid);
            let forced_group = drain_group_before_reap(target_pid, &members);
            let status = reap_target(target_pid)?;
            return Ok((status, forced_group));
        }
    }
}

/// Enumerate every member of process group `pgid` via `proc_listpgrppids`.
/// Zombies are still listed until reaped, so a member that has exited but not been
/// reaped still counts — exactly what `drain_group_before_reap` needs to decide
/// `forced_group`. Best-effort: a libproc error yields an empty list.
#[cfg(target_os = "macos")]
fn list_process_group(pgid: i32) -> Vec<i32> {
    // A NULL/0 call returns an upper-bound buffer size in BYTES (enough for every
    // pid on the system). A buffer call returns the number of PIDS written (verified
    // on macOS 26.5.1 — not bytes; the two calls are asymmetric). We do NOT rely on
    // that unit: the buffer is zero-initialized, the returned length is clamped to
    // the slot count, and non-positive slots are filtered out, so the result is the
    // same whether the return is interpreted as a pid count or a byte count.
    let needed = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }
    // Over-allocate against a member forking between the sizing and the read.
    let slots = needed as usize / std::mem::size_of::<i32>() + 16;
    let mut pids = vec![0i32; slots];
    let cap_bytes = (slots * std::mem::size_of::<i32>()) as libc::c_int;
    let written = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), cap_bytes) };
    if written <= 0 {
        return Vec::new();
    }
    pids.truncate((written as usize).min(slots));
    pids.into_iter().filter(|&pid| pid > 0).collect()
}

#[cfg(target_os = "macos")]
fn reap_target(target_pid: i32) -> std::io::Result<ExitStatus> {
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(target_pid, &mut status, 0) };
        if waited == target_pid {
            return Ok(ExitStatus::from_raw(status));
        }
        if waited < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

#[cfg(target_os = "macos")]
fn kill_and_reap_failed_target(target_pid: i32) {
    let _ = unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    let _ = reap_target(target_pid);
}

#[cfg(target_os = "macos")]
fn cstring(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| invalid("path or argument contains a NUL byte"))
}

/// Test-only driver: run the macOS containment core against an explicit target
/// (a libtest child cannot receive the guardian target through argv, so the
/// containment tests hand it in via env and call this).
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn run_supervised_for_test(
    status_pipe: &mut std::fs::File,
    target: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> i32 {
    let result = supervise_target(status_pipe, target, args);
    report(status_pipe, result)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "guardian_darwin_tests.rs"]
mod darwin_tests;

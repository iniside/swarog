//! The macOS parent-side process backend: it launches the embedded guardian
//! (the re-exec'd supervisor in `crate::guardian`) exactly as the Linux backend
//! does — same `Command`, same fd-3 liveness / fd-4 status handshake, same wire
//! protocol — and reads back the target identity the guardian captures at the
//! suspended `posix_spawn` boundary. The two differences from Linux are local to
//! this file: the liveness/status pipes are built with `pipe(2)` + `FD_CLOEXEC`
//! (macOS has no `pipe2`), and the guardian is signalled with plain `kill(2)`
//! against the held, unreaped `Child` (macOS has no `pidfd`). A zombie pins its
//! pid, so `kill` on a guardian this process has not yet reaped is as safe as
//! `pidfd_send_signal`: `graceful`/`force` run only after a `try_wait` that
//! returned `None`, i.e. while the guardian is still our live child.

use std::fs::File;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};

use super::posix::{read_completion, read_handshake};
use crate::process::{ProcessError, ProcessIdentity, SpawnSpec};

const GUARDIAN_LIVENESS_FD: RawFd = 3;
const GUARDIAN_STATUS_FD: RawFd = 4;

pub(crate) struct PlatformChild {
    guardian: Child,
    liveness: Option<OwnedFd>,
    status_pipe: File,
    completion: Option<ExitStatus>,
    completion_forced_remainder: bool,
}

pub(crate) fn spawn(
    spec: &SpawnSpec,
    input: Option<crate::platform::InheritedInput>,
) -> Result<(PlatformChild, ProcessIdentity), ProcessError> {
    let guardian_path = std::env::current_exe().map_err(|source| ProcessError::Io {
        operation: "locate current executable for guardian dispatch",
        source,
    })?;
    let (live_read, live_write) = pipe_cloexec().map_err(|source| ProcessError::Io {
        operation: "create guardian liveness pipe",
        source,
    })?;
    let (status_read, status_write) = pipe_cloexec().map_err(|source| ProcessError::Io {
        operation: "create guardian status pipe",
        source,
    })?;

    let live_read_fd = live_read.as_raw_fd();
    let status_write_fd = status_write.as_raw_fd();
    let mut command = Command::new(guardian_path);
    command
        .arg(crate::guardian::DISPATCH_ARG)
        .arg("--")
        .arg(&spec.executable)
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(match input {
            Some(crate::platform::InheritedInput(file)) => std::process::Stdio::from(file),
            None => std::process::Stdio::null(),
        })
        .stdout(spec.stdout.open()?)
        .stderr(spec.stderr.open()?);
    unsafe {
        command.pre_exec(move || {
            remap_fd(live_read_fd, GUARDIAN_LIVENESS_FD)?;
            remap_fd(status_write_fd, GUARDIAN_STATUS_FD)?;
            Ok(())
        });
    }
    let mut guardian = command.spawn().map_err(|source| ProcessError::Io {
        operation: "spawn process guardian",
        source,
    })?;
    drop(live_read);
    drop(status_write);

    let mut status = File::from(status_read);
    let identity = match read_handshake(&mut status) {
        Ok(identity) => identity,
        Err(source) => {
            drop(live_write);
            let guardian_status = guardian.wait().ok();
            return Err(ProcessError::GuardianHandshake(format!(
                "target identity was not reported ({source}); guardian status {guardian_status:?}"
            )));
        }
    };
    let target_pid = identity.pid as i32;
    if target_pid <= 0 {
        drop(live_write);
        let _ = guardian.wait();
        return Err(ProcessError::GuardianHandshake(format!(
            "invalid target pid {target_pid}"
        )));
    }
    Ok((
        PlatformChild {
            guardian,
            liveness: Some(live_write),
            status_pipe: status,
            completion: None,
            completion_forced_remainder: false,
        },
        identity,
    ))
}

impl PlatformChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(status) = self.completion {
            return Ok(Some(status));
        }
        let Some(_guardian_status) = self.guardian.try_wait()? else {
            return Ok(None);
        };
        let (status, forced_remainder) = read_completion(&mut self.status_pipe)?;
        self.completion = Some(status);
        self.completion_forced_remainder = forced_remainder;
        Ok(self.completion)
    }

    pub(crate) fn graceful(&mut self) -> std::io::Result<()> {
        signal_guardian(&self.guardian, libc::SIGTERM)
    }

    pub(crate) fn completion_forced_remainder(&self, _status: ExitStatus) -> bool {
        self.completion_forced_remainder
    }

    pub(crate) fn force(&mut self) -> std::io::Result<()> {
        self.liveness.take();
        signal_guardian(&self.guardian, libc::SIGUSR1)
    }
}

/// Signals the held guardian by pid. Callers reach this only after a `try_wait`
/// that returned `None` (see [`crate::process::OwnedChild::shutdown`] and its
/// `Drop`), so the guardian is still an unreaped live child of this process and
/// its pid cannot have been recycled — no `pidfd` is needed.
fn signal_guardian(guardian: &Child, sig: i32) -> std::io::Result<()> {
    let pid = guardian.id() as i32;
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `pipe(2)` + `FD_CLOEXEC` on both ends. macOS has no `pipe2`, so setting the
/// close-on-exec flag is a second syscall and therefore not atomic with the
/// pipe creation the way `pipe2(O_CLOEXEC)` is; a concurrent fork/exec in
/// another thread could observe the ends before the flag is set. processctl
/// spawns are driven from a single thread per child, so no such racing spawn
/// exists here.
fn pipe_cloexec() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [ends.0.as_raw_fd(), ends.1.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(ends)
}

unsafe fn remap_fd(source: RawFd, destination: RawFd) -> std::io::Result<()> {
    if source == destination {
        if libc::fcntl(destination, libc::F_SETFD, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        return Ok(());
    }
    if libc::dup2(source, destination) < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Reads a live process's identity via libproc. Returns `Err` for any pid that
/// is dead or a zombie: `proc_pidpath`/`proc_pidinfo` fail with `ESRCH` once the
/// task is gone, which is the required `OwnerNotLive` signal — the exact analogue
/// of Linux's `read_link(/proc/<pid>/exe)` failing on an exited process.
pub(crate) fn observe_process_identity(pid: u32) -> std::io::Result<ProcessIdentity> {
    let pid = pid as libc::c_int;
    let started = proc_start_marker(pid)?;
    let executable = proc_pidpath(pid)?;
    Ok(ProcessIdentity {
        pid: pid as u32,
        executable,
        started: crate::StartMarker(started),
    })
}

/// Packs `pbi_start_tvsec`/`pbi_start_tvusec` into one opaque `u64` (microseconds
/// since the epoch). `StartMarker` is serde-persisted but never compared across
/// platforms, so any injective packing is sound; microseconds cannot overflow a
/// `u64` for any realistic wall clock.
pub(crate) fn proc_start_marker(pid: libc::c_int) -> std::io::Result<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    if n != size {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

fn proc_pidpath(pid: libc::c_int) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(PathBuf::from(OsString::from_vec(buf)))
}

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

use crate::process::{ProcessError, ProcessIdentity, SpawnSpec};
use crate::protocol::{read_frame, Frame};

const GUARDIAN_LIVENESS_FD: RawFd = 3;
const GUARDIAN_STATUS_FD: RawFd = 4;

pub(crate) struct PlatformChild {
    guardian: Child,
    guardian_pidfd: OwnedFd,
    liveness: Option<OwnedFd>,
    status_pipe: File,
    completion: Option<ExitStatus>,
    completion_forced_remainder: bool,
}

pub(crate) fn spawn(spec: &SpawnSpec) -> Result<(PlatformChild, ProcessIdentity), ProcessError> {
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
        .stdin(std::process::Stdio::null())
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

    let guardian_pidfd = match pidfd_open(guardian.id() as i32) {
        Ok(pidfd) => pidfd,
        Err(source) => {
            drop(live_write);
            let _ = guardian.wait();
            return Err(ProcessError::Io {
                operation: "open guardian pidfd",
                source,
            });
        }
    };
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
            guardian_pidfd,
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
        pidfd_send_signal(self.guardian_pidfd.as_raw_fd(), libc::SIGTERM)
    }

    pub(crate) fn completion_forced_remainder(&self, _status: ExitStatus) -> bool {
        self.completion_forced_remainder
    }

    pub(crate) fn force(&mut self) -> std::io::Result<()> {
        self.liveness.take();
        pidfd_send_signal(self.guardian_pidfd.as_raw_fd(), libc::SIGUSR1)
    }
}

fn pipe_cloexec() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
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

fn pidfd_open(pid: i32) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as RawFd };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn pidfd_send_signal(pidfd: RawFd, signal: i32) -> std::io::Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(super) fn read_handshake(reader: &mut impl Read) -> std::io::Result<ProcessIdentity> {
    match read_frame(reader)? {
        Frame::Identity(identity) => Ok(identity),
        Frame::GuardianFailed(message) => Err(std::io::Error::other(format!(
            "guardian failed before target handshake: {message}"
        ))),
        Frame::Completion { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "guardian sent completion before identity",
        )),
    }
}

pub(super) fn read_completion(reader: &mut impl Read) -> std::io::Result<(ExitStatus, bool)> {
    use std::os::unix::process::ExitStatusExt;
    match read_frame(reader)? {
        Frame::Completion {
            raw_target_wait_status,
            forced_remainder,
        } => Ok((
            ExitStatus::from_raw(raw_target_wait_status),
            forced_remainder,
        )),
        Frame::GuardianFailed(message) => Err(std::io::Error::other(format!(
            "process guardian failed: {message}"
        ))),
        Frame::Identity(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "guardian sent a second identity frame",
        )),
    }
}

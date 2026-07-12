use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use crate::process::{ProcessError, ProcessIdentity, SpawnSpec, StartMarker};

const GUARDIAN_LIVENESS_FD: RawFd = 3;
const GUARDIAN_STATUS_FD: RawFd = 4;

pub(crate) struct PlatformChild {
    guardian: Child,
    guardian_pidfd: OwnedFd,
    liveness: Option<OwnedFd>,
    target_pid: i32,
}

pub(crate) fn spawn(spec: &SpawnSpec) -> Result<(PlatformChild, ProcessIdentity), ProcessError> {
    let guardian_path = guardian_path().map_err(|source| ProcessError::Io {
        operation: "locate process guardian",
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
    let identity = match read_identity(&mut status) {
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
            target_pid,
        },
        identity,
    ))
}

impl PlatformChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.guardian.try_wait()
    }

    pub(crate) fn observe_identity(&self) -> std::io::Result<ProcessIdentity> {
        observe_pid(self.target_pid as u32)
    }

    pub(crate) fn graceful(&mut self) -> std::io::Result<()> {
        pidfd_send_signal(self.guardian_pidfd.as_raw_fd(), libc::SIGTERM)
    }

    pub(crate) fn force(&mut self) -> std::io::Result<()> {
        self.liveness.take();
        pidfd_send_signal(self.guardian_pidfd.as_raw_fd(), libc::SIGUSR1)
    }
}

fn guardian_path() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let directory = current.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "current executable has no parent",
        )
    })?;
    let bin_dir = if directory.file_name().is_some_and(|name| name == "deps") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let path = bin_dir.join("processctl-guardian");
    if path.is_file() {
        Ok(path)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("guardian binary not found at {}", path.display()),
        ))
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

fn observe_pid(pid: u32) -> std::io::Result<ProcessIdentity> {
    let proc_dir = Path::new("/proc").join(pid.to_string());
    let executable = std::fs::read_link(proc_dir.join("exe"))?;
    let stat = std::fs::read_to_string(proc_dir.join("stat"))?;
    let close = stat.rfind(')').ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed /proc stat")
    })?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let started = fields
        .get(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing starttime"))?
        .parse::<u64>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(ProcessIdentity {
        pid,
        executable,
        started: StartMarker(started),
    })
}

fn read_identity(reader: &mut impl Read) -> std::io::Result<ProcessIdentity> {
    let mut pid = [0u8; 4];
    let mut started = [0u8; 8];
    let mut path_len = [0u8; 4];
    reader.read_exact(&mut pid)?;
    reader.read_exact(&mut started)?;
    reader.read_exact(&mut path_len)?;
    let path_len = u32::from_ne_bytes(path_len) as usize;
    if path_len == 0 || path_len > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "guardian reported an invalid executable-path length",
        ));
    }
    let mut path = vec![0u8; path_len];
    reader.read_exact(&mut path)?;
    Ok(ProcessIdentity {
        pid: u32::from_ne_bytes(pid),
        executable: PathBuf::from(OsString::from_vec(path)),
        started: StartMarker(u64::from_ne_bytes(started)),
    })
}

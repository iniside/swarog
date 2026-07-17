use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitStatus;
#[cfg(unix)]
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputDestination {
    Inherit,
    Null,
    File(PathBuf),
}

impl OutputDestination {
    #[cfg(unix)]
    pub(crate) fn open(&self) -> Result<Stdio, ProcessError> {
        match self {
            Self::Inherit => Ok(Stdio::inherit()),
            Self::Null => Ok(Stdio::null()),
            Self::File(path) => std::fs::File::create(path)
                .map(Stdio::from)
                .map_err(|source| ProcessError::Io {
                    operation: "create process log",
                    source,
                }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessGroupPolicy {
    #[default]
    Owned,
}

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub label: String,
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
    pub stdout: OutputDestination,
    pub stderr: OutputDestination,
    pub process_group: ProcessGroupPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub executable: PathBuf,
    pub started: StartMarker,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartMarker(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownPolicy {
    pub graceful_timeout: Duration,
    pub force_timeout: Duration,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            graceful_timeout: Duration::from_secs(5),
            force_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
pub enum ShutdownOutcome {
    AlreadyExited(ExitStatus),
    Graceful(ExitStatus),
    Forced(ExitStatus),
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("{operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported process-control platform: {0}")]
    UnsupportedPlatform(&'static str),
    #[error(
        "cannot verify process identity for {label}: expected {expected:?}, observed {observed:?}"
    )]
    IdentityMismatch {
        label: String,
        expected: ProcessIdentity,
        observed: Option<ProcessIdentity>,
    },
    #[error("process {label} did not exit within {timeout:?} after forced termination")]
    ForceTimeout { label: String, timeout: Duration },
    #[error("process guardian failed before reporting its target: {0}")]
    GuardianHandshake(String),
}

pub struct OwnedChild {
    pub(crate) label: String,
    pub(crate) identity: ProcessIdentity,
    pub(crate) inner: Option<PlatformChild>,
    pub(crate) status: Option<ExitStatus>,
}

pub fn observe_process_identity(pid: u32) -> Result<ProcessIdentity, ProcessError> {
    crate::platform::observe_process_identity(pid).map_err(|source| ProcessError::Io {
        operation: "observe process identity",
        source,
    })
}

type PlatformChild = crate::platform::PlatformChild;

impl OwnedChild {
    pub fn spawn(spec: SpawnSpec) -> Result<Self, ProcessError> {
        let (inner, identity) = crate::platform::spawn(&spec, None)?;
        Ok(Self {
            label: spec.label,
            identity,
            inner: Some(inner),
            status: None,
        })
    }

    /// Spawns an owned process with a bounded byte sequence delivered through an
    /// anonymous, non-nameable stdin pipe. The bytes never enter argv or the child
    /// environment and the write end is closed before this function returns.
    pub fn spawn_with_stdin_bytes(spec: SpawnSpec, bytes: &[u8]) -> Result<Self, ProcessError> {
        if bytes.len() > 4096 {
            return Err(ProcessError::Io {
                operation: "validate child stdin",
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "private child stdin exceeds 4096 bytes",
                ),
            });
        }
        let (input, mut writer) = private_stdin_pipe()?;
        let (inner, identity) = crate::platform::spawn(&spec, Some(input))?;
        let mut child = Self {
            label: spec.label,
            identity,
            inner: Some(inner),
            status: None,
        };
        if let Err(source) = writer.write_all(bytes) {
            let _ = child.shutdown(ShutdownPolicy::default());
            return Err(ProcessError::Io {
                operation: "write child stdin",
                source,
            });
        }
        drop(writer);
        Ok(child)
    }

    #[cfg(any(windows, unix))]
    pub(crate) fn spawn_with_input(
        spec: SpawnSpec,
        input: crate::platform::InheritedInput,
    ) -> Result<Self, ProcessError> {
        let (inner, identity) = crate::platform::spawn(&spec, Some(input))?;
        Ok(Self {
            label: spec.label,
            identity,
            inner: Some(inner),
            status: None,
        })
    }

    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let Some(inner) = self.inner.as_mut() else {
            return Ok(self.status);
        };
        if let Some(status) = inner.try_wait().map_err(|source| ProcessError::Io {
            operation: "query child status",
            source,
        })? {
            self.status = Some(status);
        }
        Ok(self.status)
    }

    pub fn shutdown(&mut self, policy: ShutdownPolicy) -> Result<ShutdownOutcome, ProcessError> {
        if let Some(status) = self.try_wait()? {
            return Ok(ShutdownOutcome::AlreadyExited(status));
        }
        let inner = self
            .inner
            .as_mut()
            .expect("live process has a platform handle");
        if inner.graceful().is_ok() {
            if let Some(status) = self.wait_for(policy.graceful_timeout)? {
                let forced_remainder = self
                    .inner
                    .as_ref()
                    .is_some_and(|inner| inner.completion_forced_remainder(status));
                return Ok(if forced_remainder {
                    ShutdownOutcome::Forced(status)
                } else {
                    ShutdownOutcome::Graceful(status)
                });
            }
        }

        let inner = self
            .inner
            .as_mut()
            .expect("live process has a platform handle");
        inner.force().map_err(|source| ProcessError::Io {
            operation: "force owned process group",
            source,
        })?;
        if let Some(status) = self.wait_for(policy.force_timeout)? {
            return Ok(ShutdownOutcome::Forced(status));
        }
        Err(ProcessError::ForceTimeout {
            label: self.label.clone(),
            timeout: policy.force_timeout,
        })
    }

    fn wait_for(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, ProcessError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "linux")]
fn private_stdin_pipe() -> Result<(crate::platform::InheritedInput, std::fs::File), ProcessError> {
    use std::os::fd::FromRawFd as _;
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(ProcessError::Io {
            operation: "create child stdin pipe",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe {
        (
            crate::platform::InheritedInput(std::fs::File::from_raw_fd(fds[0])),
            std::fs::File::from_raw_fd(fds[1]),
        )
    })
}

// macOS has no `pipe2`; create the pipe and set `FD_CLOEXEC` on both ends in a
// second, non-atomic step. See the same note on `platform::darwin::pipe_cloexec`.
#[cfg(target_os = "macos")]
fn private_stdin_pipe() -> Result<(crate::platform::InheritedInput, std::fs::File), ProcessError> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(ProcessError::Io {
            operation: "create child stdin pipe",
            source: std::io::Error::last_os_error(),
        });
    }
    let ends = unsafe {
        (
            crate::platform::InheritedInput(std::fs::File::from_raw_fd(fds[0])),
            std::fs::File::from_raw_fd(fds[1]),
        )
    };
    for fd in [ends.0 .0.as_raw_fd(), ends.1.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
            return Err(ProcessError::Io {
                operation: "set close-on-exec on child stdin pipe",
                source: std::io::Error::last_os_error(),
            });
        }
    }
    Ok(ends)
}

#[cfg(windows)]
fn private_stdin_pipe() -> Result<(crate::platform::InheritedInput, std::fs::File), ProcessError> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    let mut attributes: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    attributes.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    attributes.bInheritHandle = 1;
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 4096) } == 0 {
        return Err(ProcessError::Io {
            operation: "create child stdin pipe",
            source: std::io::Error::last_os_error(),
        });
    }
    if unsafe { SetHandleInformation(write, HANDLE_FLAG_INHERIT, 0) } == 0 {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(read);
            windows_sys::Win32::Foundation::CloseHandle(write);
        }
        return Err(ProcessError::Io {
            operation: "make child stdin writer private",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe {
        (
            crate::platform::InheritedInput(std::fs::File::from_raw_handle(read.cast())),
            std::fs::File::from_raw_handle(write.cast()),
        )
    })
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.status.is_some() || self.inner.is_none() {
            return;
        }
        // The platform handle is itself the ownership proof (Job Object or pidfd
        // guardian). Drop never falls back to a PID/name lookup.
        if let Some(inner) = self.inner.as_mut() {
            let _ = inner.force();
        }
        let _ = self.wait_for(Duration::from_secs(5));
        self.inner.take();
    }
}

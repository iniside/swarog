use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputDestination {
    Inherit,
    Null,
    File(PathBuf),
}

impl OutputDestination {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub executable: PathBuf,
    pub started: StartMarker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[cfg(any(windows, target_os = "linux"))]
type PlatformChild = crate::platform::PlatformChild;

#[cfg(not(any(windows, target_os = "linux")))]
struct PlatformChild;

#[cfg(not(any(windows, target_os = "linux")))]
impl PlatformChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        Err(unsupported_io())
    }

    fn graceful(&mut self) -> std::io::Result<()> {
        Err(unsupported_io())
    }

    fn completion_forced_remainder(&self, _status: ExitStatus) -> bool {
        false
    }

    fn force(&mut self) -> std::io::Result<()> {
        Err(unsupported_io())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn unsupported_io() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "processctl supports only Windows and Linux",
    )
}

impl OwnedChild {
    pub fn spawn(spec: SpawnSpec) -> Result<Self, ProcessError> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            let (inner, identity) = crate::platform::spawn(&spec)?;
            Ok(Self {
                label: spec.label,
                identity,
                inner: Some(inner),
                status: None,
            })
        }

        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = spec;
            Err(ProcessError::UnsupportedPlatform(std::env::consts::OS))
        }
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

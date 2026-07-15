//! Cross-platform owned-process containment: spawn a child inside its own
//! kill-tree container (Windows Job Object / Unix process group) with graceful
//! and forced shutdown. Ownership is the platform handle itself — no code path
//! ever falls back to a PID or name lookup, so a reused PID can never be
//! signalled by mistake.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

/// Complete specification of a child process. `env` is the ENTIRE child
/// environment (the supervisor's environment is never inherited), not a set of
/// additions. `None` stdio redirects to the platform null device.
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}

/// Platform-neutral exit information (`ExitStatus`-like). `code` is `None`
/// when the child was killed by a signal (Unix only).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitInfo {
    code: Option<i32>,
}

impl ExitInfo {
    pub fn code(&self) -> Option<i32> {
        self.code
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// How a [`OwnedProc::shutdown`] round ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Graceful(ExitInfo),
    Forced(ExitInfo),
}

/// A spawned child owned through its platform containment handle.
pub struct OwnedProc {
    inner: imp::PlatformProc,
    status: Option<ExitInfo>,
}

/// Spawns `spec` inside a fresh containment unit (Job Object / process group).
pub fn spawn(spec: SpawnSpec) -> Result<OwnedProc> {
    let inner = imp::spawn(spec)?;
    Ok(OwnedProc {
        inner,
        status: None,
    })
}

impl OwnedProc {
    pub fn pid(&self) -> u32 {
        self.inner.pid()
    }

    /// Non-blocking status probe; caches the exit status once observed.
    pub fn try_wait(&mut self) -> Result<Option<ExitInfo>> {
        if self.status.is_none() {
            self.status = self
                .inner
                .try_wait()
                .context("query owned process status")?;
        }
        Ok(self.status)
    }

    /// Requests a cooperative stop (CTRL_BREAK to the group / SIGTERM to the
    /// group). Does not wait.
    pub fn graceful(&mut self) -> Result<()> {
        self.inner
            .graceful()
            .context("signal owned process group gracefully")
    }

    /// Force-kills the whole containment unit. Does not wait.
    pub fn force(&mut self) -> Result<()> {
        self.inner.force().context("force owned process container")
    }

    /// Graceful → bounded poll-wait → force → bounded poll-wait → error on
    /// force timeout. An already-exited child counts as `Graceful` (it stopped
    /// without needing force).
    pub fn shutdown(
        &mut self,
        graceful_timeout: Duration,
        force_timeout: Duration,
    ) -> Result<Outcome> {
        if let Some(status) = self.try_wait()? {
            return Ok(Outcome::Graceful(status));
        }
        // A failed graceful signal (e.g. the child died concurrently) falls
        // through to force rather than aborting the shutdown.
        if self.graceful().is_ok() {
            if let Some(status) = self.wait_for(graceful_timeout)? {
                return Ok(Outcome::Graceful(status));
            }
        }
        self.force()?;
        if let Some(status) = self.wait_for(force_timeout)? {
            return Ok(Outcome::Forced(status));
        }
        bail!(
            "owned process {} did not exit within {force_timeout:?} after forced termination",
            self.pid()
        )
    }

    /// Poll-with-deadline wait (no blocking platform wait, so it can never
    /// hang past `timeout`).
    fn wait_for(&mut self, timeout: Duration) -> Result<Option<ExitInfo>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedProc {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        // Force + bounded reap (≤5s): never blocks forever and never consults
        // PID/name — the platform handle is the ownership proof. On Windows the
        // job handle closing afterwards (KILL_ON_JOB_CLOSE) is the backstop even
        // if this force/wait fails.
        let _ = self.inner.force();
        let _ = self.wait_for(Duration::from_secs(5));
    }
}

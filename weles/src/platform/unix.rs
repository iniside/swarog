//! Unix containment: the child becomes its own process-group leader via
//! `setpgid(0, 0)` in `pre_exec`, so the group id equals the child pid and the
//! whole tree can be signalled with `kill(-pid, …)`. On Linux the child
//! additionally gets `PR_SET_PDEATHSIG = SIGKILL` as a backstop against
//! supervisor death. Graceful = SIGTERM to the group; force = SIGKILL to the
//! group.
//!
//! Kill/reap ordering invariant (the authority for whole-tree cleanup): the
//! root child is NEVER reaped before the group has been swept with SIGKILL.
//! An exited-but-unreaped root is a zombie that pins the pid (and thus the
//! pgid), so `kill(-pid)` before the reap can never hit a reused pid — and
//! once `status` is cached the group was provably already swept, so no group
//! survivor (grandchild) can be orphaned by a later force()/Drop no-op.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::{ExitInfo, SpawnSpec};

pub(super) struct PlatformProc {
    child: Child,
    /// Set only by [`Self::try_wait`], and only AFTER the group sweep + reap.
    status: Option<ExitInfo>,
}

pub(super) fn spawn(spec: SpawnSpec) -> Result<PlatformProc> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    // env is the COMPLETE child environment, never additions.
    command.env_clear();
    command.envs(&spec.env);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.stdin(Stdio::null());
    command.stdout(spec.stdout.map_or_else(Stdio::null, Stdio::from));
    command.stderr(spec.stderr.map_or_else(Stdio::null, Stdio::from));
    // SAFETY: the pre_exec closure runs post-fork/pre-exec and performs only
    // async-signal-safe syscalls (setpgid, prctl) — no allocation, no locks.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .with_context(|| format!("spawn {}", spec.program.display()))?;
    Ok(PlatformProc {
        child,
        status: None,
    })
}

impl PlatformProc {
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Non-blocking exit probe. Exit is observed with a non-reaping peek
    /// (`waitid` + `WNOWAIT`); on root exit the whole group is SIGKILL-swept
    /// BEFORE the real reap, so a group survivor (grandchild) can never
    /// outlive an observed exit.
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitInfo>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        // SAFETY: a zeroed siginfo_t is valid out-storage for waitid; with
        // WNOHANG an untouched (zero) si_pid means "no state change" (POSIX).
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid on our own un-reaped child; WNOWAIT leaves the
        // zombie in place so the pid/pgid stays pinned across the sweep.
        #[allow(clippy::unnecessary_cast)] // id_t is platform-defined; the cast is a no-op on some targets
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        if !waitid_found_child(&info) {
            return Ok(None);
        }
        // Root exited but is still an unreaped zombie: sweep the group
        // BEFORE the first reaping waitpid (the kill/reap ordering authority).
        self.sweep_group()?;
        // The real reap goes through std so the Child's bookkeeping stays
        // consistent; the peek guarantees it completes immediately.
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.status = Some(ExitInfo {
            code: status.code(),
        });
        Ok(self.status)
    }

    pub(super) fn graceful(&mut self) -> io::Result<()> {
        // status cached ⇒ the group was already SIGKILL-swept at reap time,
        // so there is provably nothing left to signal — not a silent skip.
        if self.status.is_some() {
            return Ok(());
        }
        self.signal_group(libc::SIGTERM)
    }

    pub(super) fn force(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        self.sweep_group()
    }

    /// SIGKILLs the whole group, tolerating "no signalable member left" — the
    /// intended non-error when only the root zombie remains (zombies cannot
    /// receive signals). This branch runs BEFORE the reap (the kill/reap
    /// ordering authority): the zombie still pins the pid/pgid, so the sweep can
    /// never hit a reused group, and the tolerated outcome only means the group
    /// held nothing but that pinned zombie.
    ///
    /// Linux reports that condition as `ESRCH`. macOS/BSD report it as `EPERM`
    /// instead: `kill(-pgid)` there finds the group non-empty (the zombie is a
    /// member) but unsignalable, and returns `EPERM` rather than `ESRCH`. Both
    /// mean the same "nothing signalable left" here. For OUR OWN child's group we
    /// always have permission to signal a LIVE member, so an `EPERM` cannot be
    /// masking a surviving grandchild — a live member would have been signalled
    /// and `kill` would have returned success.
    fn sweep_group(&mut self) -> io::Result<()> {
        match self.signal_group(libc::SIGKILL) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            #[cfg(not(target_os = "linux"))]
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => Ok(()),
            other => other,
        }
    }

    fn signal_group(&mut self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: callers guarantee `status` is None, i.e. the root is not yet
        // reaped (only try_wait reaps, and it sets `status`) — alive or zombie
        // it pins the pid, so this negative-pid signal targets OUR group and
        // can never hit a reused pgid.
        if unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// With `WNOHANG`, waitid reports "no state change" by leaving `si_pid` zero.
#[cfg(target_os = "linux")]
fn waitid_found_child(info: &libc::siginfo_t) -> bool {
    // SAFETY: si_pid is valid to read for a SIGCHLD-class siginfo, and the
    // struct was zeroed beforehand (an untouched buffer reads as 0).
    unsafe { info.si_pid() != 0 }
}

/// With `WNOHANG`, waitid reports "no state change" by leaving `si_pid` zero.
#[cfg(all(unix, not(target_os = "linux")))]
fn waitid_found_child(info: &libc::siginfo_t) -> bool {
    info.si_pid != 0
}

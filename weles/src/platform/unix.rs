//! Unix containment: the child becomes its own process-group leader via
//! `setpgid(0, 0)` in `pre_exec`, so the group id equals the child pid and the
//! whole tree can be signalled with `kill(-pid, …)`. On Linux the child
//! additionally gets `PR_SET_PDEATHSIG = SIGKILL` as a backstop against
//! supervisor death. Graceful = SIGTERM to the group; force = SIGKILL to the
//! group.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::{ExitInfo, SpawnSpec};

pub(super) struct PlatformProc {
    child: Child,
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
    Ok(PlatformProc { child })
}

impl PlatformProc {
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitInfo>> {
        Ok(self.child.try_wait()?.map(|status| ExitInfo {
            code: status.code(),
        }))
    }

    pub(super) fn graceful(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGTERM)
    }

    pub(super) fn force(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGKILL)
    }

    fn signal_group(&mut self, signal: libc::c_int) -> io::Result<()> {
        // Never signal a reaped pid: once try_wait has reaped the child its
        // pid (and thus the pgid) may be reused by an unrelated process. An
        // exited-but-unreaped child still reserves the pid as a zombie, so the
        // reap can only happen through this Child — the guard is race-free.
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: a negative pid signals the process group we created with
        // setpgid(0, 0); the guard above guarantees the pgid is still ours.
        if unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

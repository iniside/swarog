use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

const LIVENESS_FD: RawFd = 3;
const STATUS_FD: RawFd = 4;
pub(crate) const DISPATCH_ARG: &str = "--__processctl-guardian-v1";
const FORCED_REMAINDER_EXIT: i32 = 190;

pub(crate) fn run() -> i32 {
    match run_inner() {
        Ok((_status, true)) => FORCED_REMAINDER_EXIT,
        Ok((status, false)) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        Err(error) => {
            eprintln!("processctl-guardian: {error}");
            1
        }
    }
}

fn run_inner() -> std::io::Result<(ExitStatus, bool)> {
    let mut args = std::env::args_os();
    let _guardian = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(DISPATCH_ARG)) {
        return Err(invalid("guardian dispatch marker missing"));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(invalid("expected `-- <executable> [args...]`"));
    }
    let executable = args
        .next()
        .ok_or_else(|| invalid("missing target executable"))?;
    let executable = std::fs::canonicalize(executable)?;
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
    let mut command = Command::new(&executable);
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

    let mut status_pipe = unsafe { std::fs::File::from_raw_fd(STATUS_FD) };
    let path = target_executable.as_os_str().as_bytes();
    let path_len = match u32::try_from(path.len()) {
        Ok(path_len) => path_len,
        Err(_) => {
            kill_and_reap_failed_spawn(&mut target, target_pid)?;
            return Err(invalid("target path is too long"));
        }
    };
    let handshake = status_pipe
        .write_all(&(target_pid as u32).to_ne_bytes())
        .and_then(|()| status_pipe.write_all(&target_started.to_ne_bytes()))
        .and_then(|()| status_pipe.write_all(&path_len.to_ne_bytes()))
        .and_then(|()| status_pipe.write_all(path));
    if let Err(error) = handshake {
        kill_and_reap_failed_spawn(&mut target, target_pid)?;
        return Err(error);
    }
    drop(status_pipe);
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

fn kill_and_reap_failed_spawn(target: &mut Child, target_pid: i32) -> std::io::Result<()> {
    let _ = unsafe { libc::kill(-target_pid, libc::SIGKILL) };
    let _ = target.wait();
    reap_descendants(Duration::from_secs(5)).map(|_| ())
}

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
        if force {
            let _ = unsafe { libc::kill(-target_pid, libc::SIGKILL) };
        }
        if force || pollfds[1].revents & libc::POLLIN != 0 {
            let status = target.wait()?;
            // A target may leave ordinary descendants alive. Kill its owned group,
            // then reap children reparented to this subreaper before exiting.
            let forced_group = unsafe { libc::kill(-target_pid, libc::SIGKILL) } == 0;
            let forced_adopted = reap_descendants(Duration::from_secs(5))?;
            return Ok((status, forced_group || forced_adopted));
        }
    }
}

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

fn create_signal_fd(set: &libc::sigset_t) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::signalfd(-1, set, libc::SFD_CLOEXEC) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

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

fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pidfd_open(pid: i32) -> std::io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as RawFd };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

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

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

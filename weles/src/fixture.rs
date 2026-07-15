//! Hidden `weles __test-child` fixture driven by the platform containment
//! tests: prints readiness/graceful markers, cooperates with (or, under
//! `--ignore-graceful`, deliberately swallows) the platform graceful signal,
//! and under `--spawn-grandchild` first spawns a plain copy of itself in the
//! same process group / job so kill-tree behavior can be asserted.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

static IGNORE_GRACEFUL: AtomicBool = AtomicBool::new(false);
static TERM_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn run(spawn_grandchild: bool, ignore_graceful: bool) -> Result<()> {
    IGNORE_GRACEFUL.store(ignore_graceful, Ordering::SeqCst);
    install_graceful_handler()?;
    if spawn_grandchild {
        let exe = std::env::current_exe().context("resolve test-child executable")?;
        // A plain child: no new process group / no job breakaway, so it stays
        // inside the parent's containment unit by construction.
        let grandchild = std::process::Command::new(exe)
            .arg("__test-child")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawn grandchild")?;
        println!("test-child: grandchild={}", grandchild.id());
        // Deliberately leak the Child: it must keep running after we are
        // force-killed so the test can observe the container reaping it.
        std::mem::forget(grandchild);
    }
    println!("test-child: ready");
    std::io::stdout().flush().ok();

    // Hang guard: never outlive the test run by more than 60s.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if TERM_REQUESTED.load(Ordering::SeqCst) {
            println!("test-child: graceful");
            std::io::stdout().flush().ok();
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    std::process::exit(3);
}

#[cfg(windows)]
fn install_graceful_handler() -> Result<()> {
    use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT};

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        if ctrl_type == CTRL_BREAK_EVENT {
            if !IGNORE_GRACEFUL.load(Ordering::SeqCst) {
                TERM_REQUESTED.store(true, Ordering::SeqCst);
            }
            // Handled either way: never fall through to default termination,
            // so the graceful exit (code 0 + marker) comes from the main loop.
            return 1;
        }
        0
    }

    // SAFETY: registers a process-wide ctrl handler that touches only atomics.
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(std::io::Error::last_os_error()).context("install ctrl handler");
    }
    Ok(())
}

#[cfg(unix)]
fn install_graceful_handler() -> Result<()> {
    unsafe extern "C" fn handler(_signal: libc::c_int) {
        // Async-signal-safe: atomic loads/stores only.
        if !IGNORE_GRACEFUL.load(Ordering::SeqCst) {
            TERM_REQUESTED.store(true, Ordering::SeqCst);
        }
    }

    let handler: unsafe extern "C" fn(libc::c_int) = handler;
    // SAFETY: installs a SIGTERM handler that performs only atomic operations.
    if unsafe { libc::signal(libc::SIGTERM, handler as libc::sighandler_t) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error()).context("install SIGTERM handler");
    }
    Ok(())
}

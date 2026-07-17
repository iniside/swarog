//! Fleet-liveness tests for the split-proof harness. These exercise processctl's
//! PRODUCTION spawn path (`OwnedChild::spawn`), which on unix re-execs
//! `current_exe --__processctl-guardian-v1`. A libtest unit binary has no early
//! guardian hook, so that re-exec lands on the test harness and exits 101 — the
//! exact cross-unix gap these two hit on any unix (they only ever passed under
//! Windows Job Objects, which don't re-exec).
//!
//! This is a `harness = false` target (see `tools/splitproof/Cargo.toml`): its
//! `main` owns the entrypoint, so it can call
//! `dispatch_guardian_from_current_exe()` FIRST — exactly as the production
//! `main.rs` does — before running any test logic. The two tests' assertions are
//! byte-identical to their previous libtest form; only their location and this
//! guardian-dispatch bootstrap changed. They are Postgres-free.
//!
//! Mirrors `tools/devctl/tests/supervised.rs` (Step 8b).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use processctl::{OutputDestination, OwnedChild, ProcessGroupPolicy, SpawnSpec};
use splitproof::{fleet_liveness, Running};

fn main() -> ExitCode {
    // Production ordering: the guardian re-exec must be caught before anything else
    // runs. Without this, the two tests below could never spawn a managed child on
    // unix (the re-exec would land on this test binary with an unknown flag).
    if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
        return code;
    }

    let cases: &[(&str, fn())] = &[
        (
            "fleet_liveness_reports_a_child_that_already_exited",
            fleet_liveness_reports_a_child_that_already_exited,
        ),
        (
            "fleet_liveness_ignores_a_still_running_child",
            fleet_liveness_ignores_a_still_running_child,
        ),
    ];

    let mut passed = 0usize;
    let mut ok = true;
    for (name, case) in cases {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(case)) {
            Ok(()) => {
                println!("test {name} ... ok");
                passed += 1;
            }
            Err(_) => {
                eprintln!("test {name} ... FAILED");
                ok = false;
            }
        }
    }
    println!("fleet_liveness: {passed}/{} passed", cases.len());
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A trivial process that exits almost immediately, so `try_wait` observes it as dead
/// well within the test's own polling budget. Cross-platform per-OS command (no
/// dependency on any GameBackend binary being built).
#[cfg(windows)]
fn exit_soon_spec(cwd: &Path) -> SpawnSpec {
    let comspec =
        std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("C:/Windows/System32/cmd.exe"));
    SpawnSpec {
        label: "liveness-fixture".into(),
        executable: PathBuf::from(comspec),
        args: vec![OsString::from("/C"), OsString::from("exit 3")],
        env: BTreeMap::new(),
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

#[cfg(not(windows))]
fn exit_soon_spec(cwd: &Path) -> SpawnSpec {
    SpawnSpec {
        label: "liveness-fixture".into(),
        executable: PathBuf::from("/bin/sh"),
        args: vec![OsString::from("-c"), OsString::from("exit 3")],
        env: BTreeMap::new(),
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

/// The failing branch `fleet_liveness` exists to catch: a fleet child that has already
/// exited (a dead/finished process, standing in for a service that died after clearing
/// its health gate) must be reported by name, not silently treated as alive.
fn fleet_liveness_reports_a_child_that_already_exited() {
    let cwd = std::env::temp_dir();
    let mut child = OwnedChild::spawn(exit_soon_spec(&cwd)).expect("spawn liveness fixture");

    // Wait for the fixture to actually finish before handing it to fleet_liveness —
    // the assertion under test is that fleet_liveness DETECTS a dead child, not that
    // it blocks until one dies.
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "liveness fixture did not exit in time");
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut fleet = vec![Running { name: "fixture-svc", child }];
    let dead = fleet_liveness(&mut fleet);

    assert_eq!(dead.len(), 1, "expected exactly one dead entry: {dead:?}");
    assert!(
        dead[0].contains("fixture-svc"),
        "detail must name the dead service: {dead:?}"
    );
    // The detail must also carry the exit status ("exited with exit code: 3" on
    // Windows / "exit status: 3" on unix), not just the name.
    assert!(
        dead[0].contains("exit") && dead[0].contains('3'),
        "detail must carry the exit status: {dead:?}"
    );
}

/// A still-running child must NOT be reported dead (the positive control for the
/// negative-path test above).
fn fleet_liveness_ignores_a_still_running_child() {
    let cwd = std::env::temp_dir();
    let sleep_spec = {
        let mut spec = exit_soon_spec(&cwd);
        // NB: `timeout /T` refuses redirected stdin (OwnedChild wires stdin to NUL,
        // processctl platform/windows.rs) and exits immediately with an error —
        // `ping -n` is stdin-agnostic, so this fixture genuinely stays alive.
        #[cfg(windows)]
        {
            spec.args = vec![OsString::from("/C"), OsString::from("ping -n 31 127.0.0.1 >NUL")];
        }
        #[cfg(not(windows))]
        {
            spec.args = vec![OsString::from("-c"), OsString::from("sleep 30")];
        }
        spec
    };
    let child = OwnedChild::spawn(sleep_spec).expect("spawn sleeping fixture");
    let mut fleet = vec![Running { name: "sleeping-svc", child }];

    let dead = fleet_liveness(&mut fleet);
    assert!(dead.is_empty(), "still-running child reported dead: {dead:?}");

    // `fleet` drops here: OwnedChild's Drop force-kills the still-running fixture, so
    // the test leaves no orphaned process behind.
}

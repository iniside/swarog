use super::*;

#[test]
fn status_stub_bails() {
    // Pins the stubs-bail behavior until the later M0 steps land.
    let err = run(Command::Status).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

// `up` is the real supervisor as of M0 Step 5 (`supervisor::run_up`: rollout
// lock, prep, boot, monitor/restart loop, teardown) against the actual repo
// root and local Postgres. That makes it a live/integration path (real
// process spawns + DB access), not a fast unit test — the restart policy is
// unit-tested in `supervisor_tests`, and the live path gets its acceptance
// run in M0 Step 7, not here.

#[test]
fn down_stub_bails() {
    let err = run(Command::Down).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

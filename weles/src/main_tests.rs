use super::*;

#[test]
fn status_stub_bails() {
    // Pins the stubs-bail behavior until the later M0 steps land.
    let err = run(Command::Status).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

// `up` is no longer a pure stub as of M0 Step 4: it runs the real prep
// pipeline (manifest validation, `cargo build`, CA mint, admin seed) against
// the actual repo root and local Postgres before bailing with the
// remaining-work message. That makes it a live/integration path (real
// process spawns + DB access), not a fast unit test — it is exercised
// manually per the Step 4 hand-off ("weles up split" smoke test) and will
// gain a committed harness assertion when the supervisor loop lands
// (M0 Step 5+/Step 7), not here.

#[test]
fn down_stub_bails() {
    let err = run(Command::Down).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

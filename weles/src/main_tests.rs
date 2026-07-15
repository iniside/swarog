use super::*;

// `status`/`down` are the real control clients as of M0 Step 6. Their decision
// logic (identity classification, stale/inactive/connect) and the client/server
// frame roundtrip are unit-tested in `control::control_tests`; those paths read
// the shared `run/weles/state.json`, so they are not re-driven here (a real
// state file from a concurrent run would make a main-level test nondeterministic).
// `up` remains the live supervisor path, exercised in M0 Step 7.

#[test]
fn state_path_points_at_the_weles_run_dir() {
    let path = state_path().expect("resolve state path");
    assert!(
        path.ends_with("run/weles/state.json"),
        "unexpected state path: {}",
        path.display()
    );
}

use super::*;

// `status`/`down` are the real control clients as of M0 Step 6. Their decision
// logic (identity classification, stale/inactive/connect) and the client/server
// frame roundtrip are unit-tested in `control::control_tests`; those paths read
// the shared `run/weles/state.json`, so they are not re-driven here (a real
// state file from a concurrent run would make a main-level test nondeterministic).
// `up` remains the live supervisor path, exercised in M0 Step 7.

#[test]
fn state_path_points_at_the_weles_run_dir() {
    // Pass an explicit root so this is deterministic (no cwd/WELES_ROOT read):
    // state_path composes `<root>/run/weles/state.json` off the one resolve_root
    // authority.
    let root = std::env::temp_dir().join("weles-state-path-test");
    let path = state_path(Some(root.clone())).expect("resolve state path");
    assert!(
        path.ends_with("run/weles/state.json"),
        "unexpected state path: {}",
        path.display()
    );
    assert_eq!(path, root.join("run").join("weles").join("state.json"));
}

#[test]
fn state_path_and_discover_share_one_resolved_root() {
    // De-duplication pin: the two former CARGO_MANIFEST_DIR derivations
    // (state_path for status/down, discover_layout for up/deploy) now BOTH go
    // through prep::resolve_root — given one --root, they resolve the SAME root.
    let root = std::env::temp_dir().join(format!(
        "weles-dedup-root-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create dedup test root");

    let sp = state_path(Some(root.clone())).expect("resolve state path");
    let layout =
        supervisor::discover_layout_for_deploy(Some(root.clone())).expect("discover layout");

    assert_eq!(layout.root, root, "discover must resolve the passed root");
    assert_eq!(
        sp,
        layout.root.join("run").join("weles").join("state.json"),
        "state_path and discover must agree on the root"
    );
}

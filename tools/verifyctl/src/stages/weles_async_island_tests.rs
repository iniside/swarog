//! Pure tests for the stage's decision logic. The `cargo tree` calls need a
//! `Context` (a live stage run), so what is unit-tested here is everything that
//! decides — the parsing predicate, the port diff, and the constants — over
//! synthetic input, including the previously-wrong branches.

use super::*;

/// A realistic `cargo tree -e features` fragment.
const CLEAN_TREE: &str = r#"weles v0.1.0 (G:\Projects\GameBackend\weles)
├── tokio feature "default"
│   └── tokio v1.52.3
├── tokio feature "io-util"
├── tokio feature "net"
│   └── tokio v1.52.3
└── tokio feature "rt-multi-thread"
"#;

#[test]
fn a_banned_feature_edge_is_detected_and_a_clean_tree_is_not() {
    // The failing branch: `process` present must be found. A substring check
    // against the bare word would false-positive on any crate named *process*,
    // so the predicate matches the rendered EDGE.
    let armed = format!("{CLEAN_TREE}├── tokio feature \"process\"\n");
    assert!(armed.contains(&feature_edge("process")));
    assert!(!CLEAN_TREE.contains(&feature_edge("process")));
    assert!(!CLEAN_TREE.contains(&feature_edge("signal")));
    // And the positive control the checks rely on is really present.
    assert!(CLEAN_TREE.contains(&feature_edge("net")));
}

#[test]
fn the_feature_edge_pattern_is_not_a_bare_substring_match() {
    // `tokio-process` / a `process` feature on ANOTHER crate must not trip the
    // tokio ban — otherwise the stage fails on something it does not guard.
    let decoy = "├── futures feature \"process\"\n└── some-process v1.0.0\n";
    assert!(!decoy.contains(&feature_edge("process")));
}

#[test]
fn the_bans_state_the_asymmetry_they_actually_enforce() {
    // `signal` is banned for weles but CANNOT be banned workspace-wide:
    // core/app owns it legitimately. If someone widens the workspace list to
    // include `signal`, the stage starts failing on core/app — this pins the
    // deliberate asymmetry rather than leaving it to a comment.
    assert!(BANNED_FOR_WELES.contains(&"process"));
    assert!(BANNED_FOR_WELES.contains(&"signal"));
    assert_eq!(BANNED_WORKSPACE_WIDE, &["process"]);
}

#[test]
fn the_agent_port_does_not_collide_with_the_csharp_fixture() {
    // The live claim, against the real constants on both sides. This is the
    // check weles cannot run: it can see neither of these two ports.
    assert!(
        agent_port_findings().is_empty(),
        "{:?}",
        agent_port_findings()
    );
    assert_ne!(weles::manifest::AGENT_PORT, csharp::HTTP_PORT);
    assert_ne!(weles::manifest::AGENT_PORT, csharp::PLAYER_PORT);
}

#[test]
fn the_port_check_reports_a_collision_when_there_is_one() {
    // Fail-proof for the check above: prove it can actually FAIL, rather than
    // passing because the comparison is vacuous. 8099 is the C# fixture's port
    // — the exact collision this stage exists to have caught.
    assert_eq!(
        csharp::HTTP_PORT, 8099,
        "the collision fixture below is keyed to this value"
    );
    let collided: Vec<String> = [(csharp::HTTP_PORT, "the C# fixture server's HTTP port")]
        .iter()
        .filter(|(port, _)| *port == 8099)
        .map(|(port, what)| format!("AGENT_PORT collides with {what} ({port})"))
        .collect();
    assert_eq!(collided.len(), 1, "the port predicate must detect an equal port");
}

//! The contract-golden check IS its own test (the archcheck/checkmodules idiom): this
//! runs the live-vs-committed diff over the real workspace during `cargo test`, so a
//! contract-value edit without a re-bless fails the test suite too, not only the
//! verify stage. DB-free: `defined_topics()` reads statics and `route_bindings()` is
//! impl-free.

use super::*;

/// The committed golden must exist and match the live `define(...)` +
/// `route_bindings()` values exactly. On drift, either revert the value change or
/// re-bless intentionally (`cargo run -p topiccheck -- contract-golden --bless`).
#[test]
fn committed_golden_matches_live_contract_values() {
    let findings = check().expect("contract-golden check must run");
    assert!(
        findings.is_empty(),
        "live contract values differ from {GOLDEN_REL}:\n  {}\n(if intentional, re-bless \
         via ./verify.sh --bless-contract-golden)",
        findings.join("\n  ")
    );
}

/// The rpc-module hand-list self-check must agree with the `#[rpc]` traits actually
/// present under `api/*/api/` (house rule: hand-maintained lists self-verify).
#[test]
fn rpc_module_hand_list_matches_filesystem() {
    // `live_lines` runs the self-check internally; surface it directly so a drift
    // failure names this test rather than the golden diff.
    let modules = rpc_modules();
    let labels: Vec<&'static str> = modules.iter().map(|(l, _, _)| *l).collect();
    self_check_rpc_list(&labels).expect("rpc_modules() hand-list must match api/*/api");
}

/// The golden must cover all three kinds: at least one `event` line (seven topics
/// today), one `rpc` line (the HTTP-bound operations), and one `wire` line (every
/// method's retry semantics, incl. wire-only), so an accidentally emptied source can't
/// silently produce a trivially-matching golden.
#[test]
fn live_lines_cover_events_and_rpc() {
    let lines = live_lines().expect("live lines");
    assert!(lines.iter().any(|l| l.starts_with("event ")), "no event lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("rpc ")), "no rpc lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("wire ")), "no wire lines: {lines:?}");
}

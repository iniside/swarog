use super::*;

/// The check IS the test (archcheck/checkmodules self-test pattern): run the full
/// two-config route-parity check over the real workspace module lists and assert a
/// clean tree. This is what makes the invariant enforceable from `cargo test
/// --workspace`, not only from the dedicated verify stage.
///
/// Env note: `run_all` flips the gate env vars before creating each config's
/// runtime. This crate's test binary contains ONLY this test, so no concurrent
/// test thread races the env mutation.
#[test]
fn workspace_route_sets_are_structurally_equal() {
    let findings = run_all().expect("routecheck harness must build every process");
    assert!(
        findings.is_empty(),
        "routecheck found {} route-parity violation(s):\n  - {}",
        findings.len(),
        findings.join("\n  - ")
    );
}

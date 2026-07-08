//! `skip_infra` covers exactly the three infra endpoints and nothing else.

use super::skip_infra;

#[test]
fn skips_the_three_infra_endpoints() {
    assert!(skip_infra("/healthz"));
    assert!(skip_infra("/readyz"));
    assert!(skip_infra("/metrics"));
}

#[test]
fn does_not_skip_domain_paths() {
    assert!(!skip_infra("/characters"));
    assert!(!skip_infra("/leaderboard"));
    assert!(!skip_infra("/"));
    // A near-miss (prefix, trailing slash) is NOT infra — Go matched exact paths.
    assert!(!skip_infra("/healthz/"));
    assert!(!skip_infra("/metricsx"));
}

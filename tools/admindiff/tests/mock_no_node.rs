//! Its own test binary, so emptying `PATH` reaches no other test's `node`.

#[test]
fn a_missing_node_is_an_error_not_a_skip() {
    std::env::set_var("PATH", "");
    let e = admindiff::mock::render(&admindiff::repo_root(), "dashboard", "").expect_err("node is not reachable");
    assert_eq!(
        format!("{e:#}"),
        "`node` is not on PATH; it is a dev prerequisite for admindiff (docs/reference/testing.md)"
    );
}

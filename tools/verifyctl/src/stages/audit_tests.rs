//! Pure tests for `premise_finding`, the classifier of the sqlite ignore's
//! premise check. Each case feeds the classifier one shape of `cargo tree -i
//! libsqlite3-sys` output and asserts the class of its finding. The runner
//! tests in `tests/runner.rs` drive the same shapes through the real stage.

use super::*;

/// Real cargo's stderr for a crate that is in the lock but not in the build
/// graph.
const NOTHING_TO_PRINT: &str = "warning: nothing to print.\n";

/// Real cargo's stderr for a crate that is not in the lock at all.
const NO_MATCH: &str =
    "error: package ID specification `libsqlite3-sys` did not match any packages\n";

/// An inverted tree: the crate is compiled.
const TREE: &str = "libsqlite3-sys v0.30.1\n└── sqlx-sqlite v0.8.6\n";

fn finding_of(passed: bool, stdout: &str, stderr: &str) -> String {
    premise_finding(passed, stdout, stderr)
        .unwrap_or_else(|| panic!("expected a finding for {passed} / {stdout:?} / {stderr:?}"))
}

#[test]
fn a_lock_only_crate_justifies_the_ignore() {
    assert_eq!(premise_finding(true, "", NOTHING_TO_PRINT), None);
    // Whitespace-only stdout is still an empty tree.
    assert_eq!(premise_finding(true, "\n  \n", NOTHING_TO_PRINT), None);
}

#[test]
fn a_compiled_crate_fails_even_beside_the_warning() {
    let finding = finding_of(true, TREE, "");
    assert!(
        finding.starts_with("libsqlite3-sys is compiled;"),
        "{finding}"
    );
    // The tree wins over a "nothing to print" that also appears on stderr.
    let finding = finding_of(true, TREE, NOTHING_TO_PRINT);
    assert!(
        finding.starts_with("libsqlite3-sys is compiled;"),
        "{finding}"
    );
}

#[test]
fn an_empty_tree_without_the_warning_fails() {
    let finding = finding_of(true, "", "");
    assert!(
        finding.contains("printed nothing without its \"nothing to print\" warning"),
        "{finding}"
    );
}

#[test]
fn a_crate_absent_from_the_lock_is_a_stale_ignore() {
    assert_eq!(
        finding_of(false, "", NO_MATCH),
        "RUSTSEC-2026-0459 ignore is stale; remove it"
    );
}

#[test]
fn any_other_cargo_failure_fails_and_is_not_read_as_stale_or_lock_only() {
    // A failing exit is never a pass, even with the lock-only warning on stderr.
    for stderr in [
        "error: the lock file needs to be updated but --locked was passed\n",
        "",
        NOTHING_TO_PRINT,
    ] {
        let finding = finding_of(false, "", stderr);
        assert!(
            finding.starts_with("cargo tree -i libsqlite3-sys failed"),
            "{stderr:?}: {finding}"
        );
    }
}

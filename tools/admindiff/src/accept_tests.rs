use std::collections::{BTreeMap, BTreeSet};

use super::{judge, parse};

fn err(src: &str) -> String {
    format!("{:#}", parse(src).expect_err("must not parse"))
}

#[test]
fn comments_and_blank_lines_are_skipped_and_the_last_bar_splits() {
    let src = "# header\n\n   # indented comment\nframe div.app: mock-only a | b | data difference: two bars\n  \n";
    let entries = parse(src).expect("parses");
    assert_eq!(
        entries,
        BTreeMap::from([("frame div.app: mock-only a | b".to_string(), "data difference: two bars".to_string())])
    );
}

#[test]
fn an_entry_without_a_reason_is_an_error() {
    assert_eq!(err("frame div.app: mock-only a"), "line 1: no \" | <reason>\"");
    // The line is trimmed first, so a bare ` | ` at its end leaves no separator at all.
    assert_eq!(err("# c\nframe div.app: mock-only a |   "), "line 2: no \" | <reason>\"");
    assert_eq!(err("frame div.app: mock-only a|reason"), "line 1: no \" | <reason>\"", "the bar needs its spaces");
}

#[test]
fn a_duplicate_entry_is_an_error_and_every_problem_is_listed() {
    let e = err("a | one\nb\na | two\n");
    assert_eq!(e, "line 2: no \" | <reason>\"\nline 3: duplicate entry a");
}

#[test]
fn judge_sorts_differences_into_accepted_unaccepted_and_stale() {
    let diffs: BTreeSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    let accept: BTreeMap<String, String> =
        [("a", "why"), ("z", "fixed long ago")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let v = judge(&diffs, &accept);
    assert_eq!(v.accepted, ["a"]);
    assert_eq!(v.unaccepted, ["b", "c"]);
    assert_eq!(v.stale, ["z"], "an entry matching no difference is stale");
    let clean = judge(&BTreeSet::new(), &BTreeMap::new());
    assert!(clean.accepted.is_empty() && clean.unaccepted.is_empty() && clean.stale.is_empty());
}

/// Every committed accept file parses.
#[test]
fn the_committed_accept_files_parse() {
    let dir = crate::repo_root().join("tools").join("admindiff").join("accept");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("accept dir") {
        let path = entry.expect("entry").path();
        let src = std::fs::read_to_string(&path).expect("read");
        parse(&src).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
        seen += 1;
    }
    assert!(seen > 0, "no accept file under {}", dir.display());
}

// ---- printed hints ---------------------------------------------------------------------

#[test]
fn a_printed_hint_is_not_part_of_the_key() {
    let entries = parse("x: mock-only tr#ab (\"failed · m-1\") | reason").expect("parses");
    assert_eq!(entries, BTreeMap::from([("x: mock-only tr#ab".to_string(), "reason".to_string())]));
}

#[test]
fn a_suffix_that_is_not_a_printed_hint_is_kept_verbatim() {
    for diff in [
        "x: mock-only tr#ab (\"fai\"led\")",
        "x: mock-only tr#ab (failed)",
        "frame div.app: mock-only [panel \"A (2)\"]",
        "mock-only panel \"A (2)\"",
    ] {
        let entries = parse(&format!("{diff} | reason")).expect("parses");
        assert_eq!(entries.keys().collect::<Vec<_>>(), [diff]);
    }
}

#[test]
fn an_entry_and_its_hinted_copy_are_a_duplicate() {
    assert_eq!(
        err("x: mock-only tr#ab | one\nx: mock-only tr#ab (\"failed · m-1\") | two\n"),
        "line 2: duplicate entry x: mock-only tr#ab"
    );
}

#[test]
fn a_hinted_entry_matches_its_difference_whatever_the_hint_reads_now() {
    let diffs: BTreeSet<String> = BTreeSet::from(["x: mock-only tr#ab".to_string()]);
    let accept = parse("x: mock-only tr#ab (\"a hint the page no longer prints\") | why").expect("parses");
    let v = judge(&diffs, &accept);
    assert_eq!(v.accepted, ["x: mock-only tr#ab"]);
    assert!(v.unaccepted.is_empty() && v.stale.is_empty());
}

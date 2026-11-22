//! The expected/new/stale sort of a run's failed checks against `expected-red.txt`.

use super::*;

fn list(lines: &[(&str, &str)]) -> ExpectedRed {
    lines.iter().map(|(id, reason)| (id.to_string(), reason.to_string())).collect()
}

fn names(failed: &[&str]) -> Vec<String> {
    failed.iter().map(|n| n.to_string()).collect()
}

#[test]
fn sort_puts_each_failure_in_expected_or_new_and_unmatched_entries_in_stale() {
    let expected = list(&[("AD4", "razed api-keys page"), ("ML3", "retired mail page"), ("WL6m", "retired wallet page")]);
    let failed = names(&["[WL6m] monolith over-revoke", "[AD4] no-CSRF admin POST", "[IV1] Inventory tab", "a check with no id"]);
    let sorted = sort(&expected, &failed);
    assert_eq!(
        sorted.expected,
        vec![
            ("[WL6m] monolith over-revoke".to_string(), "retired wallet page".to_string()),
            ("[AD4] no-CSRF admin POST".to_string(), "razed api-keys page".to_string()),
        ]
    );
    assert_eq!(sorted.new, names(&["[IV1] Inventory tab", "a check with no id"]));
    assert_eq!(sorted.stale, vec![("ML3".to_string(), "retired mail page".to_string())]);
    assert_eq!(sorted.blocking(), 3);
}

#[test]
fn only_listed_failures_and_no_stale_entries_do_not_block() {
    let expected = list(&[("M3b", "razed api-keys page"), ("NT4", "retired wallet page")]);
    // Two failed checks share M3b: one entry covers both, and is not stale.
    let failed = names(&["[M3b] local rich-form create-role", "[M3b] local form-submit form present", "[NT4] grant to inbox"]);
    let sorted = sort(&expected, &failed);
    assert_eq!(sorted.expected.len(), 3);
    assert!(sorted.new.is_empty() && sorted.stale.is_empty(), "{sorted:?}");
    assert_eq!(sorted.blocking(), 0);
}

#[test]
fn a_passing_listed_check_is_stale_and_blocks() {
    let expected = list(&[("PH3", "retired wallet page")]);
    let sorted = sort(&expected, &[]);
    assert_eq!(sorted.stale, vec![("PH3".to_string(), "retired wallet page".to_string())]);
    assert_eq!(sorted.blocking(), 1);
}

#[test]
fn ids_match_whole_so_a_split_entry_does_not_cover_its_monolith_twin() {
    // WL6 (split) and WL6m (monolith) are different checks: a prefix match would hide a
    // monolith regression behind the split entry, and the reverse would hide a split one.
    let sorted = sort(&list(&[("WL6", "retired wallet page")]), &names(&["[WL6m] monolith over-revoke"]));
    assert_eq!(sorted.new, names(&["[WL6m] monolith over-revoke"]));
    assert_eq!(sorted.stale, vec![("WL6".to_string(), "retired wallet page".to_string())]);
    let sorted = sort(&list(&[("WL6m", "retired wallet page")]), &names(&["[WL6] remote over-revoke"]));
    assert_eq!(sorted.new, names(&["[WL6] remote over-revoke"]));
    assert_eq!(sorted.stale.len(), 1);
}

#[test]
fn check_id_is_the_leading_bracket_text() {
    assert_eq!(check_id("[IV1m] Inventory tab draws"), Some("IV1m"));
    assert_eq!(check_id("[AD6b] remote create-role"), Some("AD6b"));
    assert_eq!(check_id("Inventory tab [IV1]"), None);
    assert_eq!(check_id("[unterminated"), None);
}

#[test]
fn parse_reads_id_reason_lines_and_skips_comments_and_blanks() {
    let parsed = parse("# header\n\n  AD4 | razed api-keys page  \nWL6m|retired wallet page | owner 2026-11-21\n").unwrap();
    assert_eq!(
        parsed,
        list(&[("AD4", "razed api-keys page"), ("WL6m", "retired wallet page | owner 2026-11-21")])
    );
}

#[test]
fn parse_refuses_a_malformed_line() {
    for bad in ["AD4 razed api-keys page", "AD4 |", " | a reason", "[AD4] | a reason", "AD 4 | a reason", "AD4 | one\nAD4 | two"] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn the_committed_list_parses_and_is_not_empty() {
    let list = load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))).unwrap();
    assert!(!list.is_empty());
}

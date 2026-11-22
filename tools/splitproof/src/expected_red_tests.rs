//! The expected/changed/new/stale sort of a run's failed checks against `expected-red.txt`.

use super::*;

fn list(lines: &[(&str, &str, &str)]) -> ExpectedRed {
    lines
        .iter()
        .map(|(id, signature, reason)| {
            (id.to_string(), Entry { signature: signature.to_string(), reason: reason.to_string() })
        })
        .collect()
}

fn failures(failed: &[(&str, &str)]) -> Vec<Failure> {
    failed.iter().map(|(name, detail)| (name.to_string(), detail.to_string())).collect()
}

#[test]
fn sort_puts_each_failure_in_expected_or_new_and_unmatched_entries_in_stale() {
    let expected = list(&[
        ("AD4", "404 Not Found", "razed api-keys page"),
        ("ML3", "code=404", "retired mail page"),
        ("WL6m", "revoke=404", "retired wallet page"),
    ]);
    let failed = failures(&[
        ("[WL6m] monolith over-revoke", "grant=404 revoke=404 balance=None"),
        ("[AD4] no-CSRF admin POST", "404 Not Found"),
        ("[IV1] Inventory tab", "code=500"),
        ("a check with no id", "boom"),
    ]);
    let sorted = sort(&expected, &failed).unwrap();
    assert_eq!(
        sorted.expected,
        vec![
            ("[WL6m] monolith over-revoke".to_string(), "retired wallet page".to_string()),
            ("[AD4] no-CSRF admin POST".to_string(), "razed api-keys page".to_string()),
        ]
    );
    assert_eq!(sorted.new, failures(&[("[IV1] Inventory tab", "code=500"), ("a check with no id", "boom")]));
    assert!(sorted.changed.is_empty(), "{sorted:?}");
    assert_eq!(sorted.stale, vec![("ML3".to_string(), "retired mail page".to_string())]);
    assert_eq!(sorted.blocking(), 3);
}

#[test]
fn only_listed_failures_carrying_their_signature_and_no_stale_entries_do_not_block() {
    let expected = list(&[("AD3a", "code=404", "retired characters page"), ("NT4", "grant=404", "retired wallet page")]);
    let failed = failures(&[("[AD3a] characters page", "code=404"), ("[NT4] grant to inbox", "grant=404 inbox_row=false")]);
    let sorted = sort(&expected, &failed).unwrap();
    assert_eq!(sorted.expected.len(), 2);
    assert!(sorted.changed.is_empty() && sorted.new.is_empty() && sorted.stale.is_empty(), "{sorted:?}");
    assert_eq!(sorted.blocking(), 0);
}

#[test]
fn a_listed_check_red_for_another_reason_is_changed_and_blocks() {
    // The page came back (no 404) but the check is still red: the entry's reason no longer
    // describes this failure, so it must not pass as expected.
    let expected = list(&[("AD3a", "code=404", "retired characters page")]);
    let failed = failures(&[("[AD3a] characters page", "code=500")]);
    let sorted = sort(&expected, &failed).unwrap();
    assert!(sorted.expected.is_empty(), "{sorted:?}");
    assert_eq!(
        sorted.changed,
        vec![Changed {
            name: "[AD3a] characters page".to_string(),
            detail: "code=500".to_string(),
            signature: "code=404".to_string(),
            reason: "retired characters page".to_string(),
        }]
    );
    // A changed check is still red, so its entry is not stale as well.
    assert!(sorted.new.is_empty() && sorted.stale.is_empty(), "{sorted:?}");
    assert_eq!(sorted.blocking(), 1);
}

#[test]
fn an_empty_detail_carries_no_signature() {
    let sorted = sort(&list(&[("WL5", "rows=0", "grants 404")]), &failures(&[("[WL5] wallet.changed", "")])).unwrap();
    assert_eq!(sorted.changed.len(), 1, "{sorted:?}");
    assert_eq!(sorted.blocking(), 1);
}

#[test]
fn sort_refuses_two_different_checks_under_one_listed_id() {
    let expected = list(&[("M3b", "role_rows", "razed api-keys page")]);
    let failed = failures(&[
        ("[M3b] local rich-form create-role", "post=404 role_rows=None"),
        ("[M3b] local form-submit form present", "no _csrf field on apikeys page"),
    ]);
    let err = sort(&expected, &failed).unwrap_err().to_string();
    assert!(err.contains("M3b") && err.contains("form present") && err.contains("create-role"), "{err}");
    // The same check failing twice is one check, not two.
    let twice = failures(&[("[M3b] create-role", "role_rows=None"), ("[M3b] create-role", "role_rows=None")]);
    assert_eq!(sort(&expected, &twice).unwrap().expected.len(), 2);
    // Unlisted ids are new either way; the refusal is about what one entry can describe.
    let unlisted = failures(&[("[X1] one", "a"), ("[X1] two", "b")]);
    assert_eq!(sort(&expected, &unlisted).unwrap().new.len(), 2);
}

#[test]
fn a_passing_listed_check_is_stale_and_blocks() {
    let expected = list(&[("PH3", "grant=404", "retired wallet page")]);
    let sorted = sort(&expected, &[]).unwrap();
    assert_eq!(sorted.stale, vec![("PH3".to_string(), "retired wallet page".to_string())]);
    assert_eq!(sorted.blocking(), 1);
}

#[test]
fn ids_match_whole_so_a_split_entry_does_not_cover_its_monolith_twin() {
    // WL6 (split) and WL6m (monolith) are different checks: a prefix match would hide a
    // monolith regression behind the split entry, and the reverse would hide a split one.
    let wl6 = list(&[("WL6", "code=404", "retired wallet page")]);
    let sorted = sort(&wl6, &failures(&[("[WL6m] monolith over-revoke", "code=404")])).unwrap();
    assert_eq!(sorted.new, failures(&[("[WL6m] monolith over-revoke", "code=404")]));
    assert_eq!(sorted.stale, vec![("WL6".to_string(), "retired wallet page".to_string())]);
    let wl6m = list(&[("WL6m", "code=404", "retired wallet page")]);
    let sorted = sort(&wl6m, &failures(&[("[WL6] remote over-revoke", "code=404")])).unwrap();
    assert_eq!(sorted.new, failures(&[("[WL6] remote over-revoke", "code=404")]));
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
fn parse_reads_id_signature_reason_lines_and_skips_comments_and_blanks() {
    let parsed =
        parse("# header\n\n  AD4 | 404 Not Found |  razed api-keys page  \nWL6m|revoke=404|retired wallet page | owner 2026-11-21\n")
            .unwrap();
    assert_eq!(
        parsed,
        list(&[
            ("AD4", "404 Not Found", "razed api-keys page"),
            ("WL6m", "revoke=404", "retired wallet page | owner 2026-11-21"),
        ])
    );
}

#[test]
fn parse_refuses_a_malformed_line() {
    for bad in [
        "AD4 razed api-keys page",
        "AD4 | a reason",
        "AD4 | 404 Not Found |",
        "AD4 | | a reason",
        " | 404 | a reason",
        "[AD4] | 404 | a reason",
        "AD 4 | 404 | a reason",
        "AD4 | 404 | one\nAD4 | 404 | two",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn the_committed_list_parses_and_is_not_empty() {
    let list = load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))).unwrap();
    assert!(!list.is_empty());
}

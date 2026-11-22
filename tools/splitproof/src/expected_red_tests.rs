//! The expected/changed/new/stale sort of a run's failed checks against `expected-red.txt`.

use super::*;

/// Entries whose signature is one fragment.
fn list(lines: &[(&str, &str, &str)]) -> ExpectedRed {
    lines
        .iter()
        .map(|(id, signature, reason)| {
            (id.to_string(), Entry { fragments: vec![signature.to_string()], reason: reason.to_string() })
        })
        .collect()
}

fn failures(failed: &[(&str, &str)]) -> Vec<Failure> {
    failed.iter().map(|(name, detail)| (name.to_string(), detail.to_string())).collect()
}

/// Sort with only the failed checks' names recorded, as a run in which every check failed.
fn sort(expected: &ExpectedRed, failed: &[Failure]) -> Result<Sorted> {
    super::sort(expected, &Names::new(), failed)
}

fn names(seen: &[&str]) -> Names {
    let mut out = Names::new();
    for name in seen {
        record(&mut out, name);
    }
    out
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
            missing: vec!["code=404".to_string()],
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
    // An unlisted id is refused too: listing it later must not find two checks behind it.
    let unlisted = failures(&[("[X1] one", "a"), ("[X1] two", "b")]);
    let err = sort(&expected, &unlisted).unwrap_err().to_string();
    assert!(err.contains("X1") && err.contains("one") && err.contains("two"), "{err}");
}

#[test]
fn sort_refuses_an_id_whose_twin_passed() {
    // [WL7] used to name both the grant check and its register precondition. With the
    // precondition passing and the grant red, only the failed name reaches the failure list,
    // so the refusal must come from the names recorded for passing checks.
    let expected = list(&[("WL7", "credited=false", "starter grant off")]);
    let failed = failures(&[("[WL7] a new registration receives the starter grant", "credited=false")]);
    let seen = names(&["[WL7] register a player for the starter grant", "[WL7] a new registration receives the starter grant"]);
    let err = super::sort(&expected, &seen, &failed).unwrap_err().to_string();
    assert!(err.contains("WL7") && err.contains("register a player") && err.contains("receives the starter grant"), "{err}");
    // Without the passing twin the same failure is expected: the refusal is the twin's doing.
    let alone = names(&["[WL7] a new registration receives the starter grant"]);
    assert_eq!(super::sort(&expected, &alone, &failed).unwrap().expected.len(), 1);
    // Two passing twins under an unlisted id are refused as well, with nothing failed.
    let passing = names(&["[6] set the cap", "[6] create past the cap -> 409"]);
    let err = super::sort(&ExpectedRed::new(), &passing, &[]).unwrap_err().to_string();
    assert!(err.contains("[6] set the cap") && err.contains("create past the cap"), "{err}");
}

#[test]
fn record_files_each_name_under_its_id_once_and_skips_names_without_one() {
    let seen = names(&["[PY7] socket", "[PY7] socket", "[PY7m] socket", "no id here"]);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen["PY7"].len(), 1);
    assert_eq!(seen["PY7m"].len(), 1);
}

#[test]
fn every_fragment_of_a_signature_must_be_in_the_detail() {
    // PH6 pins the grant AND the subscription state, an independent field: a run where the
    // grant still 404s but the subscription went stale is red for another reason too.
    let expected = parse("PH6 | grant=404 && subscription=Some((\"active\", 0)) | retired wallet page\n").unwrap();
    let name = "[PH6] nobody connected";
    let ok = failures(&[(name, "subscription=Some((\"active\", 0)) grant=404 inbox_row=false")]);
    let sorted = sort(&expected, &ok).unwrap();
    assert_eq!(sorted.expected.len(), 1, "fragments match in any order: {sorted:?}");
    assert_eq!(sorted.blocking(), 0);
    let drifted = failures(&[(name, "grant=404 inbox_row=false subscription=Some((\"stalled\", 3))")]);
    let sorted = sort(&expected, &drifted).unwrap();
    assert!(sorted.expected.is_empty(), "{sorted:?}");
    assert_eq!(sorted.changed.len(), 1, "{sorted:?}");
    assert_eq!(sorted.changed[0].missing, vec!["subscription=Some((\"active\", 0))".to_string()]);
    assert_eq!(sorted.blocking(), 1);
    // Every lacking fragment is reported, not only the first.
    let both = failures(&[(name, "grant=303")]);
    assert_eq!(sort(&expected, &both).unwrap().changed[0].missing.len(), 2);
}

#[test]
fn parse_splits_a_signature_into_trimmed_fragments() {
    let parsed = parse("PY7 |quiesced=true&&  grant=404 && state=Some((\"party\",| retired wallet page\n").unwrap();
    assert_eq!(
        parsed["PY7"].fragments,
        vec!["quiesced=true".to_string(), "grant=404".to_string(), "state=Some((\"party\",".to_string()]
    );
    assert_eq!(parse("ML3 | code=404 | retired mail page").unwrap()["ML3"].fragments, vec!["code=404".to_string()]);
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
        "AD4 | 404 && | a reason",
        "AD4 | && 404 | a reason",
        "AD4 | 404 && && 405 | a reason",
        "AD4 | && | a reason",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn the_committed_list_parses_and_is_not_empty() {
    let list = load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))).unwrap();
    assert!(!list.is_empty());
    // The entries that pin independent fields keep more than one fragment.
    for id in ["PY7", "PY7m", "PH6", "ML4b", "ML4bm"] {
        assert!(list[id].fragments.len() > 1, "{id}: {:?}", list[id].fragments);
    }
}

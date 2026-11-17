use std::collections::BTreeMap;

use super::{boot_key, lookup, mockup_pages, validate, validate_lists, Page, Route, EXCLUDED, PAGE_MAP};
use crate::tests::ScratchDir;

/// Every page the two lists name, as a mockup map would hold it.
fn listed() -> BTreeMap<String, String> {
    let pages = PAGE_MAP.iter().map(|p| p.page).chain(EXCLUDED.iter().map(|(p, _)| *p));
    pages.map(|p| (p.to_string(), format!("k-{p}"))).collect()
}

fn drift(mockup: &BTreeMap<String, String>) -> String {
    format!("{:#}", validate(mockup).expect_err("must drift"))
}

#[test]
fn the_page_map_matches_the_real_mockup() {
    let mockup = mockup_pages(&crate::repo_root().join("UILayout").join("redesign")).expect("the mockup reads");
    validate(&mockup).expect("PAGE_MAP and EXCLUDED cover the mockup exactly");
    assert_eq!(mockup.get("index").map(String::as_str), Some("dashboard"), "{mockup:?}");
}

#[test]
fn validate_rejects_a_listed_page_the_mockup_lacks() {
    let mut mockup = listed();
    validate(&mockup).expect("every listed page present passes");
    mockup.remove("players");
    let e = drift(&mockup);
    assert!(e.contains("PAGE_MAP lists players, which is not a mockup page"), "{e}");
    let mut mockup = listed();
    mockup.remove("states");
    let e = drift(&mockup);
    assert!(e.contains("EXCLUDED lists states, which is not a mockup page"), "{e}");
}

#[test]
fn validate_rejects_a_mockup_page_in_neither_list() {
    let mut mockup = listed();
    mockup.insert("newpage".to_string(), "newPage".to_string());
    let e = drift(&mockup);
    assert!(e.contains("mockup page newpage is in neither PAGE_MAP nor EXCLUDED"), "{e}");
}

#[test]
fn boot_key_reads_the_pages_boot_call() {
    assert_eq!(boot_key(r#"<script>PAGES.boot("events");</script>"#).as_deref(), Some("events"));
    assert_eq!(boot_key(r#"PAGES.boot("a") PAGES.boot("b")"#).as_deref(), Some("a"), "the first call wins");
    assert_eq!(boot_key(r#"PAGES.boot('events')"#), None, "only the double-quoted form");
    assert_eq!(boot_key(r#"PAGES.boot("open"#), None, "an unterminated key");
    assert_eq!(boot_key("<html></html>"), None);
}

#[test]
fn mockup_pages_reads_html_stems_and_fails_on_a_page_without_boot() {
    let dir = ScratchDir::new("pages-mockup");
    std::fs::write(dir.path().join("index.html"), r#"PAGES.boot("dashboard")"#).expect("write");
    std::fs::write(dir.path().join("notes.txt"), "not a page").expect("write");
    let pages = mockup_pages(dir.path()).expect("reads");
    assert_eq!(pages, BTreeMap::from([("index".to_string(), "dashboard".to_string())]));
    std::fs::write(dir.path().join("broken.html"), "<html></html>").expect("write");
    let e = format!("{:#}", mockup_pages(dir.path()).expect_err("a page without PAGES.boot"));
    assert!(e.contains("broken.html") && e.contains("no PAGES.boot"), "{e}");
}

#[test]
fn lookup_returns_mapped_and_pending_pages_and_refuses_excluded_and_unknown() {
    assert_eq!(lookup("index").expect("mapped").route, Route::Mapped("/admin"));
    let pending = lookup("player").expect("pending pages resolve");
    assert_eq!(pending.route, Route::Pending);
    assert_eq!(pending.search, "?u=aiko.tanaka", "an entity page carries its fixed entity");
    let excluded = format!("{:#}", lookup("states").expect_err("excluded"));
    assert!(excluded.contains("page states is excluded: Declared deviation D3"), "{excluded}");
    let unknown = format!("{:#}", lookup("nope").expect_err("unknown"));
    assert!(unknown.contains("unknown page nope; pages: index, events"), "{unknown}");
}

/// The const lists themselves hold none of the defects `validate` reports for them.
#[test]
fn the_const_lists_are_disjoint_unique_and_reasoned() {
    let mut seen = std::collections::BTreeSet::new();
    for page in PAGE_MAP.iter().map(|p| p.page).chain(EXCLUDED.iter().map(|(p, _)| *p)) {
        assert!(seen.insert(page), "{page} is listed twice");
    }
    assert!(EXCLUDED.iter().all(|(_, why)| !why.trim().is_empty()));
}

// ---- validate_lists: each defect beside its clean twin --------------------------------

const MAPPED: Page = Page { page: "a", route: Route::Mapped("/admin/a"), search: "" };
const PENDING: Page = Page { page: "b", route: Route::Pending, search: "" };

/// Mockup pages `a`, `b` and `c`.
fn abc() -> BTreeMap<String, String> {
    ["a", "b", "c"].iter().map(|p| (p.to_string(), format!("k-{p}"))).collect()
}

/// The one drift `validate_lists` reports for the given lists.
fn lists_drift(page_map: &[Page], excluded: &[(&str, &str)]) -> String {
    format!("{:#}", validate_lists(&abc(), page_map, excluded).expect_err("must drift"))
}

#[test]
fn validate_lists_passes_the_clean_twin() {
    validate_lists(&abc(), &[MAPPED, PENDING], &[("c", "why")]).expect("every page listed once, with a reason");
}

#[test]
fn validate_lists_rejects_a_page_listed_twice_in_one_list() {
    let twice = lists_drift(&[MAPPED, PENDING, MAPPED], &[("c", "why")]);
    assert_eq!(twice, "page map drift:
  a is listed in PAGE_MAP and again in PAGE_MAP");
    let excluded_twice = lists_drift(&[MAPPED, PENDING], &[("c", "why"), ("c", "why")]);
    assert_eq!(excluded_twice, "page map drift:
  c is listed in EXCLUDED and again in EXCLUDED");
}

#[test]
fn validate_lists_rejects_a_page_in_both_lists() {
    let both = lists_drift(&[MAPPED, PENDING], &[("c", "why"), ("a", "why")]);
    assert_eq!(both, "page map drift:
  a is listed in PAGE_MAP and again in EXCLUDED");
}

#[test]
fn validate_lists_rejects_an_excluded_page_without_a_reason() {
    let blank = lists_drift(&[MAPPED, PENDING], &[("c", " 	 ")]);
    assert_eq!(blank, "page map drift:
  EXCLUDED c has no reason");
    let empty = lists_drift(&[MAPPED, PENDING], &[("c", "")]);
    assert_eq!(empty, "page map drift:
  EXCLUDED c has no reason");
}

#[test]
fn validate_lists_rejects_an_unknown_page_in_either_list() {
    let z = Page { page: "z", route: Route::Pending, search: "" };
    let mapped = lists_drift(&[MAPPED, PENDING, z], &[("c", "why")]);
    assert_eq!(mapped, "page map drift:
  PAGE_MAP lists z, which is not a mockup page");
    let excluded = lists_drift(&[MAPPED, PENDING], &[("c", "why"), ("z", "why")]);
    assert_eq!(excluded, "page map drift:
  EXCLUDED lists z, which is not a mockup page");
}

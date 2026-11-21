use std::collections::BTreeMap;

use super::{
    boot_key, excluded_menu_id, lookup, mockup_pages, validate_excluded_modals, validate_lists, validate_menus, validate_mock_modal_hosts, validate_tabs,
    with_tab,
    DrawnTabs, Lists, Menu, Modal, ModalWhy, Page, Route, EXCLUDED, EXCLUDED_TABS, LISTS, PAGE_MAP,
};
use crate::tests::ScratchDir;

/// Every page the two lists name, as a mockup map would hold it.
fn listed() -> BTreeMap<String, String> {
    let pages = PAGE_MAP.iter().map(|p| p.page).chain(EXCLUDED.iter().map(|(p, _)| *p));
    pages.map(|p| (p.to_string(), format!("k-{p}"))).collect()
}

fn drift(mockup: &BTreeMap<String, String>) -> String {
    format!("{:#}", validate_lists(mockup, LISTS.page_map, LISTS.excluded).expect_err("must drift"))
}

#[test]
fn the_page_map_matches_the_real_mockup() {
    let mockup = crate::validated_mockup(&crate::repo_root(), &LISTS).expect("PAGE_MAP and EXCLUDED cover the mockup exactly");
    assert_eq!(mockup.get("index").map(String::as_str), Some("dashboard"), "{mockup:?}");
}

#[test]
fn validate_rejects_a_listed_page_the_mockup_lacks() {
    let mut mockup = listed();
    validate_lists(&mockup, LISTS.page_map, LISTS.excluded).expect("every listed page present passes");
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
fn lookup_returns_mapped_first_row_and_pending_pages_and_refuses_excluded_and_unknown() {
    assert_eq!(lookup(&LISTS, "index").expect("mapped").route, Route::Mapped("/admin"));
    assert_eq!(lookup(&LISTS, "apikeys").expect("pending pages resolve").route, Route::Pending);
    let entity = lookup(&LISTS, "player").expect("first-row pages resolve");
    assert_eq!(entity.route, Route::FirstRow("players"));
    assert_eq!(entity.search, "?u=aiko.tanaka", "an entity page carries its fixed entity");
    let excluded = format!("{:#}", lookup(&LISTS, "states").expect_err("excluded"));
    assert!(excluded.contains("page states is excluded: Declared deviation D3"), "{excluded}");
    let tab = format!("{:#}", lookup(&LISTS, "player.wallet").expect_err("an excluded tab"));
    assert!(tab.contains("tab player.wallet is excluded: not ported yet"), "{tab}");
    let unknown = format!("{:#}", lookup(&LISTS, "nope").expect_err("unknown"));
    assert!(unknown.contains("unknown page nope; pages: index, events"), "{unknown}");
}

/// `lookup` reads the exclusions of the lists it is given: a page or tab only they exclude
/// fails with their reason, and a production exclusion they drop is an unknown page.
#[test]
fn lookup_refuses_by_the_given_exclusion_lists() {
    let excluded = Lists { excluded: &[("zzz", "why page")], ..LISTS };
    let e = format!("{:#}", lookup(&excluded, "zzz").expect_err("excluded by the given list"));
    assert_eq!(e, "page zzz is excluded: why page");
    let e = format!("{:#}", lookup(&Lists { excluded: &[], ..LISTS }, "states").expect_err("unknown without its exclusion"));
    assert!(e.starts_with("unknown page states; pages: "), "{e}");
    let tabs = Lists { excluded_tabs: &[("player.zzz", "why tab")], ..LISTS };
    let e = format!("{:#}", lookup(&tabs, "player.zzz").expect_err("excluded by the given list"));
    assert_eq!(e, "tab player.zzz is excluded: why tab");
    let e = format!("{:#}", lookup(&Lists { excluded_tabs: &[], ..LISTS }, "player.wallet").expect_err("unknown without its exclusion"));
    assert!(e.starts_with("unknown page player.wallet; pages: "), "{e}");
}

/// The const lists themselves hold none of the defects `validate_lists` reports for them.
#[test]
fn the_const_lists_are_disjoint_unique_and_reasoned() {
    let mut seen = std::collections::BTreeSet::new();
    let pages = PAGE_MAP.iter().filter(|p| p.tab.is_none() && p.is_page()).map(|p| p.page);
    for page in pages.chain(EXCLUDED.iter().map(|(p, _)| *p)) {
        assert!(seen.insert(page), "{page} is listed twice");
    }
    assert!(EXCLUDED.iter().all(|(_, why)| !why.trim().is_empty()));
}

// ---- validate_lists: each defect beside its clean twin --------------------------------

const MAPPED: Page = Page { page: "a", route: Route::Mapped("/admin/a"), search: "", tab: None, menu: None, modal: None, force: false };
const PENDING: Page = Page { page: "b", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false };

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
    let z = Page { page: "z", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false };
    let mapped = lists_drift(&[MAPPED, PENDING, z], &[("c", "why")]);
    assert_eq!(mapped, "page map drift:
  PAGE_MAP lists z, which is not a mockup page");
    let excluded = lists_drift(&[MAPPED, PENDING], &[("c", "why"), ("z", "why")]);
    assert_eq!(excluded, "page map drift:
  EXCLUDED lists z, which is not a mockup page");
}

#[test]
fn validate_lists_rejects_a_first_row_page_following_a_page_that_is_not_mapped() {
    let follows = |list: &'static str| Page { page: "c", route: Route::FirstRow(list), search: "", tab: None, menu: None, modal: None, force: false };
    validate_lists(&abc(), &[MAPPED, PENDING, follows("a")], &[]).expect("following a Mapped page passes");
    let pending = lists_drift(&[MAPPED, PENDING, follows("b")], &[]);
    assert_eq!(pending, "page map drift:
  PAGE_MAP c follows b, which is not a Mapped page");
    let unlisted = lists_drift(&[MAPPED, PENDING, follows("z")], &[]);
    assert_eq!(unlisted, "page map drift:
  PAGE_MAP c follows z, which is not a Mapped page");
}

// ---- tab targets ---------------------------------------------------------------------

const FOLLOWS: Page = Page { page: "c", route: Route::FirstRow("a"), search: "?u=x", tab: None, menu: None, modal: None, force: false };

#[test]
fn a_tab_entry_has_its_own_id_and_adds_its_tab_to_the_search() {
    let tab = Page { tab: Some("items"), ..FOLLOWS };
    assert_eq!((FOLLOWS.id(), FOLLOWS.mock_search()), ("c".to_string(), "?u=x".to_string()));
    assert_eq!((tab.id(), tab.mock_search()), ("c.items".to_string(), "?u=x&tab=items".to_string()));
    assert_eq!(Page { search: "", ..tab }.mock_search(), "?tab=items");
    assert_eq!(with_tab("/admin/e/player/p-1", "items"), "/admin/e/player/p-1?tab=items");
    assert_eq!(with_tab("/admin/e/player/p-1?x=1", "items"), "/admin/e/player/p-1?x=1&tab=items");
}

#[test]
fn validate_lists_checks_tab_entries() {
    let tab = |id: &'static str| Page { tab: Some(id), ..FOLLOWS };
    validate_lists(&abc(), &[MAPPED, PENDING, FOLLOWS, tab("items"), tab("saves")], &[]).expect("tabs of an untabbed FirstRow pass");
    let twice = lists_drift(&[MAPPED, PENDING, FOLLOWS, tab("items"), tab("items")], &[]);
    assert_eq!(twice, "page map drift:\n  c.items is listed in PAGE_MAP and again in PAGE_MAP");
    let bad = lists_drift(&[MAPPED, PENDING, FOLLOWS, tab("It ems")], &[]);
    assert_eq!(bad, "page map drift:\n  PAGE_MAP c.It ems names tab \"It ems\", which is not a [a-z0-9_-]+ id");
    let mapped = lists_drift(&[MAPPED, PENDING, FOLLOWS, Page { tab: Some("items"), ..MAPPED }], &[]);
    assert_eq!(mapped, "page map drift:\n  PAGE_MAP a.items names a tab, which only a FirstRow page has");
    let orphan = lists_drift(&[MAPPED, PENDING, tab("items")], &[("c", "why")]);
    assert_eq!(orphan, "page map drift:\n  PAGE_MAP c.items is a tab of c, which has no untabbed entry with the same route");
}

#[test]
fn lookup_finds_an_entry_by_its_id() {
    for p in PAGE_MAP {
        assert_eq!(lookup(&LISTS, &p.id()).expect("listed").id(), p.id());
    }
}

// ---- validate_tabs: each defect beside its clean twin ---------------------------------

/// Page `c` draws tabs `profile` (selected), `items` and `saves`.
fn drawn_c() -> Vec<(&'static str, DrawnTabs)> {
    let tabs = ["profile", "items", "saves"].iter().map(|t| t.to_string()).collect();
    vec![("c", DrawnTabs { tabs, selected: Some("profile".to_string()) })]
}

/// `PAGE_MAP` with `c` and its tab entry `c.items`.
fn c_with_items() -> Vec<Page> {
    vec![MAPPED, FOLLOWS, Page { tab: Some("items"), ..FOLLOWS }]
}

fn tabs_drift(excluded_tabs: &[(&str, &str)]) -> String {
    format!("{:#}", validate_tabs(&drawn_c(), &c_with_items(), excluded_tabs).expect_err("must drift"))
}

#[test]
fn validate_tabs_passes_the_clean_twin() {
    validate_tabs(&drawn_c(), &c_with_items(), &[("c.saves", "why")]).expect("items mapped, saves excluded, profile selected");
}

#[test]
fn validate_tabs_rejects_a_drawn_tab_neither_mapped_nor_excluded() {
    assert_eq!(tabs_drift(&[]), "tab drift:\n  mockup tab c.saves is in neither PAGE_MAP nor EXCLUDED_TABS");
    let e = format!("{:#}", validate_tabs(&drawn_c(), &[MAPPED, FOLLOWS], &[("c.saves", "why")]).expect_err("items unlisted"));
    assert_eq!(e, "tab drift:\n  mockup tab c.items is in neither PAGE_MAP nor EXCLUDED_TABS");
}

#[test]
fn validate_tabs_rejects_a_stale_exclusion() {
    let gone = tabs_drift(&[("c.saves", "why"), ("c.gone", "why")]);
    assert_eq!(gone, "tab drift:\n  EXCLUDED_TABS lists c.gone, which the mockup does not draw");
    let other_page = tabs_drift(&[("c.saves", "why"), ("a.saves", "why")]);
    assert_eq!(other_page, "tab drift:\n  EXCLUDED_TABS lists a.saves, which the mockup does not draw");
    let selected = tabs_drift(&[("c.saves", "why"), ("c.profile", "why")]);
    assert_eq!(selected, "tab drift:\n  EXCLUDED_TABS lists c.profile, the tab the untabbed c entry diffs");
}

#[test]
fn validate_tabs_rejects_a_mapped_tab_the_mockup_does_not_draw() {
    let tabs = ["profile", "saves"].iter().map(|t| t.to_string()).collect();
    let drawn = vec![("c", DrawnTabs { tabs, selected: Some("profile".to_string()) })];
    let e = format!("{:#}", validate_tabs(&drawn, &c_with_items(), &[("c.saves", "why")]).expect_err("items undrawn"));
    assert_eq!(e, "tab drift:\n  PAGE_MAP maps c.items, which the mockup does not draw");
    let selected = vec![MAPPED, FOLLOWS, Page { tab: Some("profile"), ..FOLLOWS }];
    let e = format!("{:#}", validate_tabs(&drawn_c(), &selected, &[("c.saves", "why"), ("c.items", "why")]).expect_err("profile"));
    assert_eq!(e, "tab drift:\n  PAGE_MAP maps c.profile, the tab the untabbed c entry diffs");
    let e = format!("{:#}", validate_tabs(&[], &c_with_items(), &[]).expect_err("c not drawn"));
    assert_eq!(e, "tab drift:\n  PAGE_MAP maps c.items, which the mockup does not draw");
}

#[test]
fn validate_tabs_rejects_a_tab_both_mapped_and_excluded() {
    let both = tabs_drift(&[("c.saves", "why"), ("c.items", "why")]);
    assert_eq!(both, "tab drift:\n  c.items is listed in PAGE_MAP and again in EXCLUDED_TABS");
}

#[test]
fn validate_tabs_rejects_an_exclusion_listed_twice_or_without_a_reason() {
    let twice = tabs_drift(&[("c.saves", "why"), ("c.saves", "why")]);
    assert_eq!(twice, "tab drift:\n  c.saves is listed in EXCLUDED_TABS and again in EXCLUDED_TABS");
    let blank = tabs_drift(&[("c.saves", " \t ")]);
    assert_eq!(blank, "tab drift:\n  EXCLUDED_TABS c.saves has no reason");
}

#[test]
fn every_excluded_tab_names_a_first_row_page() {
    for (id, _) in EXCLUDED_TABS {
        let (page, tab) = id.split_once('.').expect("<page>.<tab>");
        assert!(PAGE_MAP.iter().any(|p| p.page == page && matches!(p.route, Route::FirstRow(_))), "{id}");
        assert!(!tab.is_empty(), "{id}");
    }
}

// ---- menu targets --------------------------------------------------------------------

const MENU: Menu = Menu { mock: "user", kind: "player" };

#[test]
fn a_menu_entry_has_its_own_id_and_its_host_is_the_entry_without_it() {
    let menu = Page { menu: Some(MENU), ..MAPPED };
    assert_eq!((menu.id(), menu.host()), ("a.menu.user".to_string(), MAPPED));
    let tabbed = Page { tab: Some("items"), menu: Some(MENU), ..FOLLOWS };
    assert_eq!(tabbed.id(), "c.items.menu.user");
    assert_eq!(tabbed.host().id(), "c.items");
}

#[test]
fn validate_lists_checks_menu_entries() {
    let menu = Page { menu: Some(MENU), ..MAPPED };
    validate_lists(&abc(), &[MAPPED, PENDING, menu], &[("c", "why")]).expect("a menu of a listed page passes");
    let orphan = lists_drift(&[PENDING, menu], &[("c", "why")]);
    assert_eq!(orphan, "page map drift:\n  PAGE_MAP a.menu.user is a menu of a, which has no entry with the same route");
    let moved = Page { search: "?u=x", ..menu };
    let e = lists_drift(&[MAPPED, PENDING, moved], &[("c", "why")]);
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.menu.user is a menu of a, which has no entry with the same route");
    let pending = lists_drift(&[MAPPED, PENDING, Page { menu: Some(MENU), ..PENDING }], &[("c", "why")]);
    assert_eq!(pending, "page map drift:\n  PAGE_MAP b.menu.user names a menu of b, which is pending");
    let bad = Page { menu: Some(Menu { mock: "User", kind: "play/er" }), ..MAPPED };
    let e = lists_drift(&[MAPPED, PENDING, bad], &[("c", "why")]);
    assert_eq!(
        e,
        concat!(
            "page map drift:\n  PAGE_MAP a.menu.User names menu \"User\", which is not a [a-z0-9_-]+ id\n",
            "  PAGE_MAP a.menu.User names kind \"play/er\", which is not a [a-z0-9_-]+ id",
        )
    );
}

fn strings(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn validate_menus_needs_a_menu_entry_for_each_drawn_menu() {
    let page_map = [MAPPED, Page { menu: Some(MENU), ..MAPPED }];
    let drawn = |host: &str, ids: &[&str]| vec![(host.to_string(), strings(ids))];
    let defined = strings(&["user", "lock"]);
    let excluded = [("lock", "why")];
    validate_menus(&drawn("a", &["user"]), &defined, &page_map, &excluded).expect("the drawn menu is an entry");
    validate_menus(&drawn("a", &[]), &defined, &page_map, &excluded).expect("a page that draws none");
    let e = format!("{:#}", validate_menus(&drawn("a", &["user", "lock"]), &defined, &page_map, &excluded).expect_err("lock drawn"));
    assert_eq!(e, "menu drift:\n  mockup menu a.menu.lock is not a PAGE_MAP menu entry");
    let e = format!("{:#}", validate_menus(&drawn("c", &["user"]), &defined, &page_map, &excluded).expect_err("another host"));
    assert_eq!(e, "menu drift:\n  mockup menu c.menu.user is not a PAGE_MAP menu entry");
}

/// The registry inventory, each defect beside the clean twin: every defined id is an entry's
/// or excluded, and every entry and exclusion names a defined id.
#[test]
fn validate_menus_needs_every_registry_menu_declared() {
    let page_map = [MAPPED, Page { menu: Some(MENU), ..MAPPED }];
    let check = |defined: &[&str], excluded: &[(&str, &str)]| {
        validate_menus(&[], &strings(defined), &page_map, excluded).map_err(|e| format!("{e:#}"))
    };
    check(&["user", "lock"], &[("lock", "why")]).expect("the clean twin");
    let e = check(&["user", "lock"], &[]).expect_err("lock undeclared");
    assert_eq!(e, "menu drift:\n  mockup menu lock is in neither PAGE_MAP nor EXCLUDED_MOCK_MENUS");
    let e = check(&["user", "lock"], &[("lock", "why"), ("user", "why")]).expect_err("user both");
    assert_eq!(e, "menu drift:\n  mockup menu user is listed in PAGE_MAP and again in EXCLUDED_MOCK_MENUS");
    let e = check(&["lock"], &[("lock", "why")]).expect_err("user not defined");
    assert_eq!(e, "menu drift:\n  PAGE_MAP a.menu.user names menu user, which the mockup registry does not define");
    let e = check(&["user"], &[("lock", "why")]).expect_err("lock stale");
    assert_eq!(e, "menu drift:\n  EXCLUDED_MOCK_MENUS lists lock, which the mockup registry does not define");
    let e = check(&["user", "lock"], &[("lock", "why"), ("lock", " ")]).expect_err("twice, no reason");
    assert_eq!(
        e,
        concat!(
            "menu drift:\n  lock is listed in EXCLUDED_MOCK_MENUS and again in EXCLUDED_MOCK_MENUS\n",
            "  EXCLUDED_MOCK_MENUS lock has no reason",
        )
    );
}

// ---- modal targets -------------------------------------------------------------------

const MODAL: Modal = Modal { id: "cancel", opener: "cancelMail", args: &["m-1"], module: "mail", action: "cancel" };

#[test]
fn a_modal_entry_has_its_own_id_and_its_host_is_the_entry_without_it() {
    let modal = Page { modal: Some(MODAL), ..MAPPED };
    assert_eq!((modal.id(), modal.host()), ("a.modal.cancel".to_string(), MAPPED));
    assert!(!modal.is_page() && MAPPED.is_page(), "a modal entry is no page target");
    let tabbed = Page { tab: Some("items"), modal: Some(MODAL), ..FOLLOWS };
    assert_eq!(tabbed.id(), "c.items.modal.cancel");
    assert_eq!(tabbed.host().id(), "c.items");
    assert_eq!(MODAL.action_path(), "mail/cancel");
}

#[test]
fn validate_lists_checks_modal_entries() {
    let modal = Page { modal: Some(MODAL), ..MAPPED };
    validate_lists(&abc(), &[MAPPED, PENDING, modal], &[("c", "why")]).expect("a modal of a listed page passes");
    let dotted = Page { modal: Some(Modal { module: "core.events", action: "poison.skip", opener: "$skip_2", ..MODAL }), ..MAPPED };
    validate_lists(&abc(), &[MAPPED, PENDING, dotted], &[("c", "why")]).expect("dotted ids and a $_ opener pass");
    let orphan = lists_drift(&[PENDING, modal], &[("c", "why")]);
    assert_eq!(orphan, "page map drift:\n  PAGE_MAP a.modal.cancel is a modal of a, which has no entry with the same route");
    let moved = Page { search: "?u=x", ..modal };
    let e = lists_drift(&[MAPPED, PENDING, moved], &[("c", "why")]);
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.modal.cancel is a modal of a, which has no entry with the same route");
    let pending = lists_drift(&[MAPPED, PENDING, Page { modal: Some(MODAL), ..PENDING }], &[("c", "why")]);
    assert_eq!(pending, "page map drift:\n  PAGE_MAP b.modal.cancel names a modal of b, which is pending");
    let with = |m: Modal| lists_drift(&[MAPPED, PENDING, Page { modal: Some(m), ..MAPPED }], &[("c", "why")]);
    let e = with(Modal { id: "Skip", ..MODAL });
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.modal.Skip names modal \"Skip\", which is not a [a-z0-9_-]+ id");
    let e = with(Modal { opener: "1x", ..MODAL });
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.modal.cancel names opener \"1x\", which is not a JavaScript identifier");
    let e = with(Modal { module: "Mail", action: "a/b", ..MODAL });
    assert_eq!(
        e,
        concat!(
            "page map drift:\n  PAGE_MAP a.modal.cancel names module \"Mail\", which is not a [a-z0-9._-]+ id\n",
            "  PAGE_MAP a.modal.cancel names action \"a/b\", which is not a [a-z0-9._-]+ id",
        )
    );
}

/// A modal hosted by a menu target validates against that menu entry, as a page modal does
/// against its page entry.
#[test]
fn validate_lists_checks_menu_modal_entries() {
    let menu = Page { menu: Some(MENU), ..MAPPED };
    let modal = Page { modal: Some(MODAL), ..menu };
    assert_eq!((modal.id(), modal.host()), ("a.menu.user.modal.cancel".to_string(), menu));
    validate_lists(&abc(), &[MAPPED, PENDING, menu, modal], &[("c", "why")]).expect("a modal of a listed menu target passes");
    let orphan = lists_drift(&[MAPPED, PENDING, modal], &[("c", "why")]);
    let want = "page map drift:\n  PAGE_MAP a.menu.user.modal.cancel is a modal of menu a.menu.user, which is no PAGE_MAP menu entry";
    assert_eq!(orphan, want);
}

/// A modal exclusion may name as its host a menu target of an untabbed page, or an
/// `EXCLUDED_MENUS` menu of one, each beside a host that is neither: a menu id no entry maps and
/// an excluded menu of a kind no exclusion lists. A modal mapped and excluded on the same menu
/// fails.
#[test]
fn validate_excluded_modals_accepts_a_menu_target_or_excluded_menu_host() {
    let menu = Page { menu: Some(MENU), ..MAPPED };
    let page_map = [MAPPED, PENDING, menu, Page { modal: Some(MODAL), ..menu }];
    let excluded_menus = [("a", "guild", "why")];
    let check = |excluded_modals: &[(&str, &str, ModalWhy<'_>)]| {
        validate_excluded_modals(&Lists { page_map: &page_map, excluded_menus: &excluded_menus, excluded_modals, ..LISTS })
    };
    check(&[("a.menu.user", "mail/test-send", ModalWhy { why: "why" })]).expect("a menu target hosts it");
    check(&[("a.excluded-menu.guild", "mail/test-send", ModalWhy { why: "why" })]).expect("an excluded menu hosts it");
    for host in ["a.menu.other", "a.excluded-menu.other"] {
        let e = format!("{:#}", check(&[(host, "mail/test-send", ModalWhy { why: "why" })]).expect_err("neither a menu target nor an excluded menu"));
        let want = format!(
            "modal drift:\n  EXCLUDED_MODALS lists {host} modal mail/test-send, and {host} is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu"
        );
        assert_eq!(e, want, "host {host}");
    }
    let e = format!("{:#}", check(&[("a.menu.user", "mail/cancel", ModalWhy { why: "why" })]).expect_err("mapped on the menu too"));
    assert_eq!(e, "modal drift:\n  a.menu.user modal mail/cancel is listed in PAGE_MAP and again in EXCLUDED_MODALS");
}

/// A modal hosted by a tab entry, or by a menu target on a tab page, validates against that
/// entry, and its exclusion names that entry as its host, each beside its twin with no such
/// entry; a modal mapped and excluded on the same tab fails.
#[test]
fn a_tab_or_a_tab_menu_target_hosts_modals() {
    let tab = Page { tab: Some("items"), ..FOLLOWS };
    let modal = Page { modal: Some(MODAL), ..tab };
    assert_eq!((modal.id(), modal.host()), ("c.items.modal.cancel".to_string(), tab));
    validate_lists(&abc(), &[MAPPED, PENDING, FOLLOWS, tab, modal], &[]).expect("a modal of a listed tab passes");
    let orphan = lists_drift(&[MAPPED, PENDING, FOLLOWS, modal], &[]);
    assert_eq!(orphan, "page map drift:\n  PAGE_MAP c.items.modal.cancel is a modal of c.items, which has no entry with the same route");
    let menu = Page { menu: Some(MENU), ..tab };
    let menu_modal = Page { modal: Some(MODAL), ..menu };
    assert_eq!(menu_modal.id(), "c.items.menu.user.modal.cancel");
    validate_lists(&abc(), &[MAPPED, PENDING, FOLLOWS, tab, menu, menu_modal], &[]).expect("a modal of a tab's menu target passes");

    let with = |page_map: &[Page], host: &str, action: &str| check_modals(page_map, &[(host, action, ModalWhy { why: "why" })]);
    with(&[MAPPED, FOLLOWS, tab, menu], "c.items.menu.user", "guild/kick").expect("a tab's menu target hosts it");
    let e = format!("{:#}", with(&[MAPPED, FOLLOWS, tab], "c.items.menu.user", "guild/kick").expect_err("no such menu target"));
    let want = "modal drift:\n  EXCLUDED_MODALS lists c.items.menu.user modal guild/kick, and c.items.menu.user is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu";
    assert_eq!(e, want);
    with(&[MAPPED, FOLLOWS, tab, modal], "c.items", "guild/kick").expect("another action on the tab");
    let e = format!("{:#}", with(&[MAPPED, FOLLOWS, tab, modal], "c.items", "mail/cancel").expect_err("mapped on the tab too"));
    assert_eq!(e, "modal drift:\n  c.items modal mail/cancel is listed in PAGE_MAP and again in EXCLUDED_MODALS");
}

/// An `EXCLUDED_MENUS` menu of a tab entry, `excluded_menu_id(<page>.<tab>, kind)`, hosts a
/// modal exclusion as one of a page does; one whose host is no compared page or tab, a tab no
/// entry lists or a pending page, hosts none.
#[test]
fn an_excluded_menu_of_a_tab_hosts_modal_exclusions() {
    let tab = Page { tab: Some("items"), ..FOLLOWS };
    let page_map = [MAPPED, PENDING, FOLLOWS, tab];
    let check = |page: &str| {
        let host = excluded_menu_id(page, "guild");
        let excluded_menus = [(page, "guild", "why")];
        let excluded_modals = [(host.as_str(), "guild/kick", ModalWhy { why: "why" })];
        let lists = Lists { page_map: &page_map, excluded_menus: &excluded_menus, excluded_modals: &excluded_modals, ..LISTS };
        validate_excluded_modals(&lists).map_err(|e| format!("{e:#}"))
    };
    assert_eq!(excluded_menu_id("c.items", "guild"), "c.items.excluded-menu.guild");
    check("c.items").expect("an excluded menu of the tab hosts it");
    for page in ["c.saves", "b"] {
        let host = excluded_menu_id(page, "guild");
        let e = check(page).expect_err("an excluded menu of no compared page or tab");
        let want = format!(
            "modal drift:\n  EXCLUDED_MODALS lists {host} modal guild/kick, and {host} is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu"
        );
        assert_eq!(e, want, "excluded menu host {page}");
    }
}

/// `a` maps the `mail/cancel` modal; `b` is pending; `c` is an untabbed `FirstRow` page with a
/// tab entry `c.items`.
fn modal_map() -> Vec<Page> {
    vec![MAPPED, PENDING, FOLLOWS, Page { tab: Some("items"), ..FOLLOWS }, Page { modal: Some(MODAL), ..MAPPED }]
}

/// [`validate_excluded_modals`] of `page_map`, `excluded_modals` and no menu exclusion.
fn check_modals(page_map: &[Page], excluded_modals: &[(&str, &str, ModalWhy<'_>)]) -> anyhow::Result<()> {
    validate_excluded_modals(&Lists { page_map, excluded_menus: &[], excluded_modals, ..LISTS })
}

fn modals_drift(excluded_modals: &[(&str, &str, ModalWhy<'_>)]) -> String {
    format!("{:#}", check_modals(&modal_map(), excluded_modals).expect_err("must drift"))
}

#[test]
fn validate_excluded_modals_passes_the_clean_twin() {
    let clean = [
        ("a", "mail/test-send", ModalWhy { why: "why" }),
        ("c", "core.events/poison.skip", ModalWhy { why: "why" }),
        ("c", "mail/cancel", ModalWhy { why: "mapped on a only" }),
        ("c.items", "mail/test-send", ModalWhy { why: "why" }),
    ];
    check_modals(&modal_map(), &clean).expect("each exclusion once, reasoned, on a page or tab, not mapped there");
    check_modals(&modal_map(), &[]).expect("no exclusion");
}

#[test]
fn validate_excluded_modals_rejects_an_exclusion_listed_twice_or_without_a_reason() {
    let twice = modals_drift(&[("a", "mail/test-send", ModalWhy { why: "why" }), ("a", "mail/test-send", ModalWhy { why: "why" })]);
    assert_eq!(twice, "modal drift:\n  a modal mail/test-send is listed in EXCLUDED_MODALS and again in EXCLUDED_MODALS");
    let blank = modals_drift(&[("a", "mail/test-send", ModalWhy { why: " \t " })]);
    assert_eq!(blank, "modal drift:\n  EXCLUDED_MODALS a modal mail/test-send has no reason");
}

#[test]
fn validate_excluded_modals_rejects_a_host_that_is_no_page_or_tab_that_is_not_pending() {
    for host in ["z", "b", "c.saves", "a.items"] {
        let e = modals_drift(&[(host, "mail/test-send", ModalWhy { why: "why" })]);
        let want = format!(
            "modal drift:\n  EXCLUDED_MODALS lists {host} modal mail/test-send, and {host} is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu"
        );
        assert_eq!(e, want, "host {host}");
    }
    let tab_only = [MAPPED, Page { tab: Some("items"), ..FOLLOWS }];
    check_modals(&tab_only, &[("c.items", "mail/test-send", ModalWhy { why: "why" })]).expect("the tab entry hosts it");
    let e = format!("{:#}", check_modals(&tab_only, &[("c", "mail/test-send", ModalWhy { why: "why" })]).expect_err("c is tabbed only"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MODALS lists c modal mail/test-send, and c is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu");
    let pending_tab = [MAPPED, Page { tab: Some("items"), ..PENDING }];
    let e = format!("{:#}", check_modals(&pending_tab, &[("b.items", "mail/test-send", ModalWhy { why: "why" })]).expect_err("b.items is pending"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MODALS lists b.items modal mail/test-send, and b.items is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu");
    let modal_only = [PENDING, Page { modal: Some(MODAL), ..MAPPED }];
    let e = format!("{:#}", check_modals(&modal_only, &[("a", "mail/test-send", ModalWhy { why: "why" })]).expect_err("a is a modal entry only"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MODALS lists a modal mail/test-send, and a is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu");
}

#[test]
fn validate_excluded_modals_rejects_a_malformed_action() {
    for action in ["mail", "Mail/x", "a/b/c", "/x", "x/"] {
        let e = modals_drift(&[("a", action, ModalWhy { why: "why" })]);
        let want =
            format!("modal drift:\n  EXCLUDED_MODALS a names action {action:?}, which is not a <module>/<action> of [a-z0-9._-]+ ids");
        assert_eq!(e, want, "action {action}");
    }
}

#[test]
fn validate_excluded_modals_rejects_a_modal_both_mapped_and_excluded() {
    let both = modals_drift(&[("a", "mail/cancel", ModalWhy { why: "why" })]);
    assert_eq!(both, "modal drift:\n  a modal mail/cancel is listed in PAGE_MAP and again in EXCLUDED_MODALS");
}

#[test]
fn the_production_modal_exclusions_are_valid() {
    validate_excluded_modals(&LISTS).expect("EXCLUDED_MODALS holds to PAGE_MAP and EXCLUDED_MENUS");
    validate_mock_modal_hosts(&LISTS).expect("every EXCLUDED_MOCK_MODALS claim names a compared target");
    assert!(PAGE_MAP.iter().any(|p| p.modal.is_some()), "PAGE_MAP names a modal");
    for p in PAGE_MAP.iter().filter(|p| p.modal.is_some()) {
        assert!(p.id().contains(".modal.") && PAGE_MAP.iter().any(|q| q.hosts(&p.host())), "{}", p.id());
    }
}

// ---- forced states -------------------------------------------------------------------

#[test]
fn validate_lists_checks_forced_states() {
    let forced = Page { force: true, ..MAPPED };
    assert_eq!((forced.id(), forced.host()), ("a".to_string(), MAPPED), "a forced page is the same target");
    validate_lists(&abc(), &[forced, PENDING], &[("c", "why")]).expect("a forced page passes");
    let tab = Page { tab: Some("items"), force: true, ..FOLLOWS };
    validate_lists(&abc(), &[MAPPED, PENDING, FOLLOWS, tab], &[]).expect("a forced tab passes");
    let menu = Page { menu: Some(MENU), ..MAPPED };
    let modal = Page { modal: Some(MODAL), ..MAPPED };
    validate_lists(&abc(), &[forced, PENDING, menu, modal], &[("c", "why")]).expect("a menu and a modal of a forced page pass");
    let e = lists_drift(&[MAPPED, PENDING, Page { force: true, ..menu }], &[("c", "why")]);
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.menu.user forces the ok state, which only a page or tab entry may");
    let e = lists_drift(&[MAPPED, PENDING, Page { force: true, ..modal }], &[("c", "why")]);
    assert_eq!(e, "page map drift:\n  PAGE_MAP a.modal.cancel forces the ok state, which only a page or tab entry may");
}

#[test]
fn validate_lists_refuses_a_state_parameter_in_search() {
    let near = Page { search: "?u=x&states=1", ..MAPPED };
    validate_lists(&abc(), &[near, PENDING], &[("c", "why")]).expect("another parameter passes");
    for search in ["?state=ok", "?u=x&state=error", "?state"] {
        let e = lists_drift(&[Page { search, ..MAPPED }, PENDING], &[("c", "why")]);
        assert_eq!(e, "page map drift:\n  PAGE_MAP a carries state in its search; a forced state is the force field", "{search}");
    }
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::mock::mock_tests::FakeRepo;
use crate::pages::{
    Lists, MockModalExclusion, MockOpener, Modal, ModalWhy, Page, PortalModal, EXCLUDED_MOCK_MENUS, EXCLUDED_MOCK_MODALS, EXCLUDED_TABS, LISTS,
    PAGE_MAP, PALETTE_ONCLICK,
};
use crate::{crawl, parse_args, run_diff, validate_renders, validated_mockup, Command, DEFAULT_BASE_URL, DEFAULT_USER};

// ---- shared fixtures ---------------------------------------------------------------

/// A fresh directory under the system temp dir, removed on drop.
pub(crate) struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn new(name: &str) -> ScratchDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join("admindiff-tests").join(format!("{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        ScratchDir(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create parent");
    std::fs::write(path, body).expect("write fixture");
}

/// Holds every [`crate::skeleton::REPEAT`] container, a tile and, with `panel`, one panel.
pub(crate) fn body(panel: bool) -> String {
    let mut out = concat!(
        r#"<table><tbody><tr><td></td></tr></tbody></table><ul><li></li></ul><div class="svc"></div>"#,
        r#"<div class="pop-row"></div><div class="uptime"><i></i></div><select><option></option></select>"#,
        r#"<a class="panel"><div class="kpi"><div class="kpi-label">Tile</div></div></a>"#,
    )
    .to_string();
    if panel {
        out.push_str(r#"<section class="panel"><span class="panel-title">Panel</span><div class="panel-body"></div></section>"#);
    }
    out
}

/// The mockup side: every [`crate::skeleton::MOCK_STRIP`] entry around `body`.
pub(crate) fn mock_html(body: &str) -> String {
    format!(
        r#"<div class="app"><div class="devbar"></div><div class="frag"><div class="frag-label">x</div>{body}</div><div class="annot-legend"></div></div>"#
    )
}

/// The portal side: the crawl's assembled page with the `.slot` wrapper.
fn portal_html(body: &str) -> String {
    format!(r#"<html><body><div class="app"><div class="slot" data-state="ok">{body}</div></div></body></html>"#)
}

/// A crawl run directory holding `index.html`, with or without its CLEAN marker.
fn crawl_dir(name: &str, page: &str, clean: bool) -> ScratchDir {
    let dir = ScratchDir::new(name);
    write(&dir.path().join("index.html"), page);
    if clean {
        write(&dir.path().join(crawl::CLEAN_MARKER), "");
    }
    dir
}

// ---- run_diff ------------------------------------------------------------------------

#[test]
fn run_diff_fails_a_page_with_zero_matched_panels_and_nothing_else_wrong() {
    let tiles_only = FakeRepo::new("diff-zero", &mock_html(&body(false)));
    let portal = crawl_dir("diff-zero-crawl", &portal_html(&body(false)), true);
    assert!(!run_diff(tiles_only.path(), &LISTS, "index", portal.path(), false).expect("runs"), "no panel matched");

    let with_panel = FakeRepo::new("diff-one", &mock_html(&body(true)));
    let portal = crawl_dir("diff-one-crawl", &portal_html(&body(true)), true);
    let clean = run_diff(with_panel.path(), &LISTS, "index", portal.path(), false).expect("runs");
    assert!(clean, "the same page with a panel is clean");
}

#[test]
fn run_diff_refuses_a_crawl_without_its_clean_marker() {
    let repo = FakeRepo::new("diff-clean", &mock_html(&body(true)));
    let dir = crawl_dir("diff-unclean-crawl", &portal_html(&body(true)), false);
    let e = format!("{:#}", run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect_err("no CLEAN marker"));
    assert!(e.contains("has no CLEAN marker: its crawl failed, did not finish or ran with --allow-errors"), "{e}");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    assert!(run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs once marked clean"));
}

#[test]
fn run_diff_refuses_a_pending_page() {
    let repo = FakeRepo::new("diff-pending", &mock_html(&body(true)));
    let dir = crawl_dir("diff-pending-crawl", &portal_html(&body(true)), true);
    let e = format!("{:#}", run_diff(repo.path(), &LISTS, "apikeys", dir.path(), false).expect_err("pending"));
    assert!(e.contains("page apikeys is pending"), "{e}");
}

#[test]
fn run_diff_fails_on_an_unaccepted_difference_and_on_a_stale_accept_entry() {
    let repo = FakeRepo::new("diff-accept", &mock_html(&body(true)));
    let extra = format!("{}<footer></footer>", body(true));
    let dir = crawl_dir("diff-accept-crawl", &portal_html(&extra), true);
    assert!(!run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs"), "an unaccepted portal-only footer");

    let accept = repo.path().join("tools/admindiff/accept/index.txt");
    write(&accept, "frame div.app: portal-only footer | template choice: test\n");
    assert!(run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs"), "the accepted difference passes");

    write(&accept, "frame div.app: portal-only footer | template choice: test\nframe div.app: mock-only nav | gone\n");
    assert!(!run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs"), "a stale entry fails");

    write(&accept, "frame div.app: portal-only footer\n");
    assert!(run_diff(repo.path(), &LISTS, "index", dir.path(), false).is_err(), "a malformed accept file is an error");
}

// ---- parse_args --------------------------------------------------------------------

fn args(line: &str) -> anyhow::Result<Command> {
    parse_args(&line.split_whitespace().map(str::to_string).collect::<Vec<_>>())
}

fn args_err(line: &str) -> String {
    format!("{:#}", args(line).expect_err("must not parse"))
}

#[test]
fn parse_args_reads_each_command_with_its_defaults() {
    assert_eq!(args("").expect("no args"), Command::Help);
    for help in ["help", "-h", "--help"] {
        assert_eq!(args(help).expect("help"), Command::Help);
    }
    assert_eq!(
        args("crawl").expect("crawl"),
        Command::Crawl {
            base_url: DEFAULT_BASE_URL.to_string(),
            user: DEFAULT_USER.to_string(),
            allow_errors: false,
            allow_empty: false,
        }
    );
    assert_eq!(
        args("crawl --base-url http://localhost:8085/ --user ops --allow-errors").expect("crawl"),
        Command::Crawl {
            base_url: "http://localhost:8085".to_string(),
            user: "ops".to_string(),
            allow_errors: true,
            allow_empty: false,
        }
    );
    assert_eq!(
        args("crawl --allow-empty").expect("crawl"),
        Command::Crawl {
            base_url: DEFAULT_BASE_URL.to_string(),
            user: DEFAULT_USER.to_string(),
            allow_errors: false,
            allow_empty: true,
        }
    );
    assert_eq!(args("mock --page index").expect("mock"), Command::Mock { page: "index".to_string() });
    assert_eq!(
        args("diff --portal target/run --text --page events").expect("diff"),
        Command::Diff { page: "events".to_string(), portal: PathBuf::from("target/run"), text: true }
    );
}

#[test]
fn parse_args_rejects_unknown_missing_and_misplaced_arguments() {
    assert!(args_err("frobnicate").starts_with("admindiff: unknown command \"frobnicate\""));
    assert!(args_err("frobnicate --page x").starts_with("admindiff: unknown command \"frobnicate\""));
    assert!(args_err("crawl --page index").starts_with("admindiff crawl: unknown argument \"--page\""));
    assert!(args_err("mock --text").starts_with("admindiff mock: unknown argument \"--text\""));
    let password = args_err("crawl --password x");
    assert!(password.starts_with("admindiff crawl: unknown argument \"--password\""), "no password in argv");
    assert!(args_err("mock").starts_with("admindiff mock: --page is required"));
    assert!(args_err("diff --page index").starts_with("admindiff diff: --portal is required"));
    assert!(args_err("diff --portal x").starts_with("admindiff diff: --page is required"));
    assert_eq!(args_err("diff --page"), "admindiff: --page requires a value");
}

/// The production repeat check `run_diff` makes through `validate_renders`: the same page and crawl as the clean
/// twin, minus the mockup's only `.uptime > i`, is an error before anything is compared.
#[test]
fn run_diff_fails_on_a_repeat_entry_dead_in_the_mockup() {
    let portal = crawl_dir("diff-repeat-crawl", &portal_html(&body(true)), true);
    let live = FakeRepo::new("diff-repeat-live", &mock_html(&body(true)));
    assert!(run_diff(live.path(), &LISTS, "index", portal.path(), false).expect("runs"), "the clean twin");

    let bars = r#"<div class="uptime"><i></i></div>"#;
    assert!(body(true).contains(bars), "the fixture holds the bars it removes");
    let dead = FakeRepo::new("diff-repeat-dead", &mock_html(&body(true).replace(bars, "")));
    let e = format!("{:#}", run_diff(dead.path(), &LISTS, "index", portal.path(), false).expect_err("a dead REPEAT entry"));
    assert_eq!(
        e,
        "REPEAT entries matching nothing in the mockup: .uptime > i (uptime bars: a bar's class is its sample's tone, which is data)"
    );
}

/// An accept entry kept with the hint the diff printed still matches once the row's text,
/// and so its hint, has changed: it is not stale and the difference is accepted.
#[test]
fn run_diff_matches_a_hinted_accept_entry_whose_hint_has_changed() {
    let row = r#"<tr class="x"><td><span class="mono">m-1</span></td><td><span class="badge">failed</span></td></tr>"#;
    let plain = "<tbody><tr><td></td></tr>";
    assert!(body(true).contains(plain), "the fixture holds the tbody it extends");
    let repo = FakeRepo::new("diff-hint", &mock_html(&body(true).replace(plain, &format!("{plain}{row}"))));
    let dir = crawl_dir("diff-hint-crawl", &portal_html(&body(true)), true);
    assert!(!run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs"), "the extra mock row is unaccepted");

    let wrapped = crate::html::parse(&format!(r#"<div class="app">{row}</div>"#)).expect("parses");
    let root = crate::skeleton::prepare(&wrapped, &[], "t").expect("has .app");
    let label = crate::skeleton::page(&root, false).frame.children[0].label();
    let key = format!("frame div.app > table > tbody > tr*: mock-only {label}");
    let accept = repo.path().join("tools/admindiff/accept/index.txt");
    write(&accept, &format!("{key} (\"done · m-0\") | data difference: test\n"));
    assert!(run_diff(repo.path(), &LISTS, "index", dir.path(), false).expect("runs"), "the hinted entry accepts the row");
}

/// The production `validate_renders` call in `run_diff`: the clean twin's player render
/// drawing one more tab, which neither a page map tab entry nor `EXCLUDED_TABS` declares, is
/// an error before anything is compared. The stand-in renders the same body for every page.
#[test]
fn run_diff_fails_on_a_mockup_tab_no_list_declares() {
    let portal = crawl_dir("diff-tab-crawl", &portal_html(&body(true)), true);
    let live = FakeRepo::new("diff-tab-live", &mock_html(&body(true)));
    assert!(run_diff(live.path(), &LISTS, "index", portal.path(), false).expect("runs"), "the clean twin");

    let zzz = r#"<button class="tab" onclick="PAGES.tab='zzz';REG.render()">zzz</button>"#;
    let undeclared = FakeRepo::new("diff-tab-undeclared", &mock_html(&format!("{}{zzz}", body(true))));
    let e = format!("{:#}", run_diff(undeclared.path(), &LISTS, "index", portal.path(), false).expect_err("an undeclared tab"));
    assert_eq!(e, "tab drift:\n  mockup tab player.zzz is in neither PAGE_MAP nor EXCLUDED_TABS");
}

/// [`validate_renders`] of `lists` against the mockup [`validated_mockup`] checks them against,
/// as `run_diff` calls it.
fn renders(repo: &Path, lists: &Lists) -> anyhow::Result<()> {
    let mockup = validated_mockup(repo, lists)?;
    validate_renders(repo, lists, &mockup)
}

/// The real mockup, rendered through node: every tab it draws on every `FirstRow` page is
/// declared, and dropping one exclusion is caught.
#[test]
fn the_real_mockup_draws_only_declared_tabs() {
    let repo = crate::repo_root();
    renders(&repo, &LISTS).expect("every real mockup tab is declared");
    let fewer: Vec<(&str, &str)> = EXCLUDED_TABS.iter().copied().filter(|(id, _)| *id != "player.wallet").collect();
    let e = format!("{:#}", renders(&repo, &Lists { excluded_tabs: &fewer, ..LISTS }).expect_err("wallet undeclared"));
    assert_eq!(e, "tab drift:\n  mockup tab player.wallet is in neither PAGE_MAP nor EXCLUDED_TABS");
}

/// The real mockup, rendered through node: a `NON_TAB_KEYS` key no `.tab` a compared render draws
/// assigns is stale. `repoTab` is assigned only on lore-repo, which no page map entry compares.
#[test]
fn a_non_tab_key_no_compared_tab_assigns_is_stale() {
    let repo = crate::repo_root();
    let e = format!("{:#}", renders(&repo, &Lists { non_tab_keys: &["repoTab"], ..LISTS }).expect_err("repoTab is stale"));
    assert_eq!(e, "tab drift:\n  NON_TAB_KEYS lists PAGES.repoTab, which no .tab a compared render draws assigns");
}

/// The real mockup, rendered through node: every menu it draws on a compared page is a menu
/// entry, a menu entry whose page draws no such menu fails, and dropping one entry is caught.
#[test]
fn the_real_mockup_draws_only_declared_menus() {
    let repo = crate::repo_root();
    let fewer: Vec<crate::pages::Page> = PAGE_MAP.iter().copied().filter(|p| p.id() != "players.menu.user").collect();
    assert_eq!(fewer.len() + 1, PAGE_MAP.len(), "PAGE_MAP lists players.menu.user");
    let e = format!("{:#}", renders(&repo, &Lists { page_map: &fewer, ..LISTS }).expect_err("players' user menu undeclared"));
    assert_eq!(e, "menu drift:\n  mockup menu players.menu.user is not a PAGE_MAP menu entry");
    let events = PAGE_MAP.iter().find(|p| p.page == "events").expect("PAGE_MAP lists events");
    let menu = crate::pages::Menu { mock: "user", kind: "player" };
    let mut more = PAGE_MAP.to_vec();
    more.push(crate::pages::Page { menu: Some(menu), ..*events });
    let e = format!("{:#}", renders(&repo, &Lists { page_map: &more, ..LISTS }).expect_err("events draws no user menu"));
    assert_eq!(e, "P.events draws no user menu; the mockup has no such menu");
}

/// The real mockup's registry, read through node: every menu it defines is declared, and
/// dropping the exclusion of `repo`, which no compared page draws, is caught; an exclusion the
/// registry does not define is stale.
#[test]
fn the_real_mockup_defines_only_declared_menus() {
    let repo = crate::repo_root();
    let fewer: Vec<(&str, &str)> = EXCLUDED_MOCK_MENUS.iter().copied().filter(|(id, _)| *id != "repo").collect();
    assert_eq!(fewer.len() + 1, EXCLUDED_MOCK_MENUS.len(), "EXCLUDED_MOCK_MENUS lists repo");
    let e = format!("{:#}", renders(&repo, &Lists { excluded_mock_menus: &fewer, ..LISTS }).expect_err("repo undeclared"));
    assert_eq!(e, "menu drift:\n  mockup menu repo is in neither PAGE_MAP nor EXCLUDED_MOCK_MENUS");
    let more: Vec<(&str, &str)> = EXCLUDED_MOCK_MENUS.iter().copied().chain([("zzz", "why")]).collect();
    let e = format!("{:#}", renders(&repo, &Lists { excluded_mock_menus: &more, ..LISTS }).expect_err("zzz is stale"));
    assert_eq!(e, "menu drift:\n  EXCLUDED_MOCK_MENUS lists zzz, which the mockup registry does not define");
}

/// Menus are inventoried on every compared page and tab: the real player page draws its user
/// menu on every tab, so a tab entry for the wallet tab fails without a
/// `player.wallet.menu.user` entry and passes with one. The player page's and its user menu's
/// `EXCLUDED_MOCK_MODALS` entries are hosted by the wallet tab and its menu too, as those draw
/// the same openers.
#[test]
fn a_real_mockup_tab_entry_needs_its_menu_entries() {
    let repo = crate::repo_root();
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists player");
    let user = PAGE_MAP.iter().find(|p| p.id() == "player.menu.user").expect("PAGE_MAP lists player.menu.user");
    let wallet = crate::pages::Page { tab: Some("wallet"), ..*player };
    let wallet_user = crate::pages::Page { tab: Some("wallet"), ..*user };
    let tabs: Vec<(&str, &str)> = EXCLUDED_TABS.iter().copied().filter(|(id, _)| *id != "player.wallet").collect();
    let rehost = |host: &str| match host {
        "player" => Some("player.wallet"),
        "player.menu.user" => Some("player.wallet.menu.user"),
        _ => None,
    };
    let on_tab: Vec<MockModalExclusion> = EXCLUDED_MOCK_MODALS.iter().filter_map(|x| rehost(x.host).map(|host| MockModalExclusion { host, ..*x })).collect();
    let mock_modals = mock_modals(&on_tab);
    let bare: Vec<Page> = PAGE_MAP.iter().copied().chain([wallet]).collect();
    let lists = Lists { page_map: &bare, excluded_tabs: &tabs, excluded_mock_modals: &mock_modals, ..LISTS };
    let e = format!("{:#}", renders(&repo, &lists).expect_err("the wallet tab draws the user menu undeclared"));
    assert_eq!(e, "menu drift:\n  mockup menu player.wallet.menu.user is not a PAGE_MAP menu entry");
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([wallet, wallet_user]).collect();
    renders(&repo, &Lists { page_map: &page_map, ..lists }).expect("the wallet tab's user menu is a menu entry");
}

/// The portal side of the stand-in's `players.menu.user`: the same menu body, unlabelled, in the
/// portal's popover.
fn players_user_menu(extra: &str) -> String {
    let same = crate::mock::mock_tests::MOCK_MENU.replace("{id}", "user").replace("{arg}", "players").replace("{run}", "");
    let same = same.replace(r#"<div class="frag"><span class="frag-label">user</span>"#, "").replacen("</div></div>", "</div>", 1);
    format!(r#"<div class="menu" data-menu-popup="">{same}{extra}</div>"#)
}

/// A menu target compares the two `.menu` popovers, not the pages, and needs no panel: the
/// stand-in menu against the same menu is clean, and against a portal menu with one more item
/// it is not.
#[test]
fn run_diff_compares_a_menu_target_by_its_menu() {
    let repo = FakeRepo::new("diff-menu", &mock_html(&body(true)));
    let dir = ScratchDir::new("diff-menu-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    write(&dir.path().join("players.menu.user.html"), &players_user_menu(""));
    assert!(run_diff(repo.path(), &LISTS, "players.menu.user", dir.path(), false).expect("runs"), "the same menu is clean");
    let more = players_user_menu(r#"<div class="menu-item"><span>more</span></div>"#);
    write(&dir.path().join("players.menu.user.html"), &more);
    assert!(!run_diff(repo.path(), &LISTS, "players.menu.user", dir.path(), false).expect("runs"), "a portal-only item");
}

/// The production page map with its modal entries replaced by one stand-in modal of `index`,
/// `index.modal.test`, so a modal diff test reads no production modal entry.
fn modal_map() -> Vec<Page> {
    let index = PAGE_MAP.iter().find(|p| p.id() == "index").expect("PAGE_MAP lists index");
    let modal = Modal { id: "test", opener: "testOpener", args: &["a"], module: "test", action: "do" };
    PAGE_MAP.iter().copied().filter(|p| p.modal.is_none()).chain([Page { modal: Some(modal), ..*index }]).collect()
}

/// A modal target compares the two `.modal` dialogs, not the pages, and needs no panel: the
/// stand-in modal against a portal `.scrim` holding the same dialog is clean, beside a
/// portal-only sibling of the dialog in the `.scrim` too, and against one whose dialog has one
/// more child it is not.
#[test]
fn run_diff_compares_a_modal_target_by_its_modal() {
    let map = modal_map();
    let repo = FakeRepo::with_map("diff-modal", &mock_html(&body(true)), &map);
    let modal = |extra: &str| {
        format!(
            concat!(
                r#"<div class="scrim" data-scrim=""><div class="modal"><div class="modal-head"><h2>x</h2></div>"#,
                r#"<div class="modal-body"></div><div class="modal-foot"></div>{}</div></div>"#,
            ),
            extra
        )
    };
    let dir = ScratchDir::new("diff-modal-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    write(&dir.path().join("index.modal.test.html"), &modal(""));
    let lists = Lists { page_map: &map, ..LISTS };
    assert!(run_diff(repo.path(), &lists, "index.modal.test", dir.path(), false).expect("runs"), "the same dialog is clean");
    let open = r#"<div class="scrim" data-scrim=""><div class="modal">"#;
    let beside = modal("").replacen(open, r#"<div class="scrim" data-scrim=""><div class="scrim-note"><span></span></div><div class="modal">"#, 1);
    assert!(beside != modal(""), "the sibling is added: {beside}");
    write(&dir.path().join("index.modal.test.html"), &beside);
    assert!(run_diff(repo.path(), &lists, "index.modal.test", dir.path(), false).expect("runs"), "a portal-only sibling of the .modal");
    write(&dir.path().join("index.modal.test.html"), &modal(r#"<div class="modal-note"></div>"#));
    assert!(!run_diff(repo.path(), &lists, "index.modal.test", dir.path(), false).expect("runs"), "a portal-only child");
}

/// A clean crawl with no output for a menu or modal target skipped it as EMPTY: the diff names
/// that, the target's host and what it did not draw, instead of a missing-file error; the twin
/// with the modal written runs.
#[test]
fn run_diff_names_a_menu_or_modal_target_the_crawl_skipped() {
    let map = modal_map();
    let repo = FakeRepo::with_map("diff-skipped", &mock_html(&body(true)), &map);
    let dir = ScratchDir::new("diff-skipped-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    let shown = dir.path().display();
    let lists = Lists { page_map: &map, ..LISTS };
    let e = format!("{:#}", run_diff(repo.path(), &lists, "index.modal.test", dir.path(), false).expect_err("no modal"));
    let want = format!(
        "{shown} has no index.modal.test modal: the crawl skipped it as EMPTY (index was skipped or drew no test/do opener) or predates the entry"
    );
    assert_eq!(e, want);
    let e = format!("{:#}", run_diff(repo.path(), &lists, "players.menu.user", dir.path(), false).expect_err("no menu"));
    let want = format!(
        "{shown} has no players.menu.user menu: the crawl skipped it as EMPTY (players was skipped or drew no player menu) or predates the entry"
    );
    assert_eq!(e, want);
    let modal = r#"<div class="scrim" data-scrim=""><div class="modal"></div></div>"#;
    write(&dir.path().join("index.modal.test.html"), modal);
    run_diff(repo.path(), &lists, "index.modal.test", dir.path(), false).expect("the written modal is diffed");
}

/// A menu entry opens its menu on its host page rendered as the host entry is: the stand-in
/// draws the players user menu button only on a render forced ok, so with the players entry
/// forcing, the menu target renders and diffs clean against the same menu; the same repo
/// diffed with the unforced production entry draws no user menu on players and fails.
#[test]
fn run_diff_opens_a_menu_of_a_forced_host_on_the_forced_render() {
    let forced: Vec<Page> = PAGE_MAP.iter().map(|p| Page { force: p.id() == "players", ..*p }).collect();
    assert_eq!(forced.iter().filter(|p| p.force).count(), 1, "PAGE_MAP lists players once");
    let repo = FakeRepo::with_map("diff-forced-menu", &mock_html(&body(true)), &forced);
    let dir = ScratchDir::new("diff-forced-menu-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    write(&dir.path().join("players.menu.user.html"), &players_user_menu(""));
    assert!(run_diff(repo.path(), &Lists { page_map: &forced, ..LISTS }, "players.menu.user", dir.path(), false).expect("runs"), "the forced host's menu is clean");
    let e = format!("{:#}", run_diff(repo.path(), &LISTS, "players.menu.user", dir.path(), false).expect_err("unforced"));
    assert_eq!(e, "P.players draws no user menu; the mockup has no such menu");
}

// ---- the mockup's modal inventory ---------------------------------------------------

/// Stand-in openers: `extraModal` appends a `.scrim` holding a preview-labelled `.modal`, as `UI.confirm` does, `toastOnly` a
/// `.toast`, `boom` throws and `rerender` re-renders the page through `REG.render()`, which the stand-in registry
/// gets here as a mount; `armedModal` calls `extraModal` only once `window.armed` is set, and the
/// global `inlineScrim`, no `PAGES` function, appends a `.scrim` as an inline `UI.modal` call does;
/// `classListModal` and `wideModal` append one through `classList.add` and as `"scrim wide"`,
/// `deferredModal` opens `extraModal` from a timer and `forever` re-schedules itself without end.
const OPENERS: &str = concat!(
    "PAGES.extraModal = function () { const el = document.createElement(\"div\"); el.className = \"scrim\"; el.innerHTML = '<div class=\"modal\"><div class=\"frag\"><span class=\"frag-label\"></span></div></div>'; document.body.appendChild(el); };\n",
    "PAGES.toastOnly = function () { const el = document.createElement(\"div\"); el.className = \"toast\"; document.body.appendChild(el); };\n",
    "PAGES.boom = function () { throw new Error(\"kaboom\"); };\n",
    "PAGES.testOpener = PAGES.extraModal;\n",
    "REG.render = function () { REG.mount({}, function () { return \"\"; }); };\n",
    "PAGES.rerender = function () { REG.render(); };\n",
    "PAGES.armedModal = function () { if (window.armed) PAGES.extraModal(); };\n",
    "PAGES.classListModal = function () { const el = document.createElement(\"div\"); el.classList.add(\"scrim\"); document.body.appendChild(el); };\n",
    "PAGES.wideModal = function () { const el = document.createElement(\"div\"); el.className = \"scrim wide\"; document.body.appendChild(el); };\n",
    "PAGES.deferredModal = function () { setTimeout(function () { PAGES.extraModal(); }, 10); };\n",
    "PAGES.forever = function tick() { setTimeout(tick, 10); };\n",
    "window.inlineScrim = function () { const el = document.createElement(\"div\"); el.className = \"scrim\"; document.body.appendChild(el); };\n",
);

/// The stand-in mockup of the production page map that also draws `drawn`, each body in its own
/// button, and defines [`OPENERS`].
fn opener_repo(name: &str, drawn: &[(&str, &str)]) -> FakeRepo {
    FakeRepo::with_openers(name, &mock_html(&body(true)), PAGE_MAP, drawn, OPENERS)
}

/// An exclusion of `PAGES.<name>` on `host` whose portal side is absent.
fn pages_modal<'a>(host: &'a str, name: &'a str, why: &'a str) -> MockModalExclusion<'a> {
    MockModalExclusion { host, opener: MockOpener::Pages(name), portal: PortalModal::Absent, why }
}

/// `EXCLUDED_MOCK_MODALS` with `more` added.
fn mock_modals<'a>(more: &[MockModalExclusion<'a>]) -> Vec<MockModalExclusion<'a>> {
    EXCLUDED_MOCK_MODALS.iter().copied().chain(more.iter().copied()).collect()
}

/// A modal opener the mockup draws on a compared page or in a compared menu fails unless
/// `EXCLUDED_MOCK_MODALS` lists it for that host; listed, it passes.
#[test]
fn an_unlisted_mockup_modal_fails_and_a_listed_one_passes() {
    let repo = opener_repo("modal-unlisted", &[("index", "PAGES.extraModal('x')"), ("players.menu.user", "PAGES.extraModal('y')")]);
    let listed = mock_modals(&[pages_modal("index", "extraModal", "why"), pages_modal("players.menu.user", "extraModal", "why")]);
    renders(repo.path(), &Lists { excluded_mock_modals: &listed, ..LISTS }).expect("both listed");
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("both unlisted"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  mockup modal players.menu.user PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS",
    );
    assert_eq!(e, want);
    let both = mock_modals(&[pages_modal("index", "extraModal", "why"), pages_modal("players.menu.user", "extraModal", "why"), pages_modal("index", "extraModal", " ")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &both, ..LISTS }).expect_err("twice"));
    let want = concat!(
        "modal drift:\n",
        "  index PAGES.extraModal is listed in EXCLUDED_MOCK_MODALS and again in EXCLUDED_MOCK_MODALS\n",
        "  EXCLUDED_MOCK_MODALS index PAGES.extraModal has no reason",
    );
    assert_eq!(e, want);
    let mapped = mock_modals(&[pages_modal("events", "skipPoison", "why")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &mapped, ..LISTS }).expect_err("mapped too"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  mockup modal events PAGES.skipPoison is listed in PAGE_MAP and again in EXCLUDED_MOCK_MODALS\n",
        "  mockup modal players.menu.user PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS",
    );
    assert_eq!(e, want);
}

/// An `EXCLUDED_MOCK_MODALS` entry its host does not draw is stale, as is one hosted by a
/// pending page, whose render is not inventoried; the clean twin draws it.
#[test]
fn a_stale_mock_modal_exclusion_fails() {
    let repo = opener_repo("modal-stale", &[("index", "PAGES.extraModal('x')")]);
    renders(repo.path(), &Lists { excluded_mock_modals: &mock_modals(&[pages_modal("index", "extraModal", "why")]), ..LISTS }).expect("drawn");
    let stale = mock_modals(&[pages_modal("index", "extraModal", "why"), pages_modal("index", "ghost", "why"), pages_modal("apikeys", "createKey", "why")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &stale, ..LISTS }).expect_err("stale"));
    let want = concat!(
        "modal drift:\n",
        "  EXCLUDED_MOCK_MODALS lists index PAGES.ghost, which no compared index mockup render draws as a modal\n",
        "  EXCLUDED_MOCK_MODALS lists apikeys PAGES.createKey, which no compared apikeys mockup render draws as a modal",
    );
    assert_eq!(e, want);
}

/// An opener that appends no `.scrim` is no modal: drawn unlisted it passes, and listed it is
/// stale; its modal twin drawn unlisted fails.
#[test]
fn an_opener_that_appends_no_scrim_is_no_modal() {
    let repo = opener_repo("modal-toast", &[("index", "PAGES.toastOnly('x')")]);
    renders(repo.path(), &LISTS).expect("a toast is no modal");
    let listed = mock_modals(&[pages_modal("index", "toastOnly", "why")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &listed, ..LISTS }).expect_err("stale"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MOCK_MODALS lists index PAGES.toastOnly, which no compared index mockup render draws as a modal");
    let modal = opener_repo("modal-toast-twin", &[("index", "PAGES.extraModal('x')")]);
    let e = format!("{:#}", renders(modal.path(), &LISTS).expect_err("a modal"));
    assert_eq!(e, "modal drift:\n  mockup modal index PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
}

/// A body that re-renders the page (`REG.render()`, whose body assignment the harness stops)
/// neither throws nor is a modal, whether a `PAGES` function re-renders or the body does after
/// an assignment (a tab switch, no `PAGES` call): drawn unlisted it passes, and listed it is
/// stale.
#[test]
fn an_onclick_that_re_renders_is_no_error_and_no_modal() {
    let switch = "PAGES.mailTab='x';REG.render()";
    let repo = opener_repo("modal-rerender", &[("index", "PAGES.rerender('x')"), ("index", switch)]);
    renders(repo.path(), &LISTS).expect("a re-render is no error and no modal");
    let switched = MockModalExclusion { opener: MockOpener::Onclick(switch), ..pages_modal("index", "", "why") };
    let listed = mock_modals(&[pages_modal("index", "rerender", "why"), switched]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &listed, ..LISTS }).expect_err("stale"));
    let want = concat!(
        "modal drift:\n",
        "  EXCLUDED_MOCK_MODALS lists index PAGES.rerender, which no compared index mockup render draws as a modal\n",
        "  EXCLUDED_MOCK_MODALS lists index onclick \"PAGES.mailTab='x';REG.render()\", which no compared index mockup render draws as a modal",
    );
    assert_eq!(e, want);
}

/// A body that re-renders the page and then opens a modal has that modal; a body that assigns the
/// page body outside every `REG` call fails by name, as its modal would go unseen.
#[test]
fn an_onclick_that_opens_after_a_re_render_or_assigns_the_body_is_seen() {
    let repo = opener_repo("modal-rerender-open", &[("index", "REG.render();PAGES.extraModal()")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("opens after the re-render"));
    assert_eq!(e, "modal drift:\n  mockup modal index PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
    let bare = opener_repo("modal-bare-body", &[("index", "document.body.innerHTML = ''")]);
    let e = format!("{:#}", renders(bare.path(), &LISTS).expect_err("a bare body assignment"));
    assert_eq!(e, "mockup onclick failed:\n  onclick \"document.body.innerHTML = ''\" drawn on index threw in the harness: assigned document.body.innerHTML outside every REG call");
}

/// A `.scrim` is any element with the `scrim` class: one added through `classList.add` or
/// written beside another class is a modal, drawn unlisted it fails.
#[test]
fn a_scrim_by_class_token_is_a_modal() {
    let repo = opener_repo("modal-class-token", &[("index", "PAGES.classListModal()"), ("index", "PAGES.wideModal()")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("two modals"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index PAGES.classListModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  mockup modal index PAGES.wideModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS",
    );
    assert_eq!(e, want);
}

/// A modal or menu target renders the `.scrim` or `.menu` its opener appends whichever way the
/// element gets its class: through `classList.add` or beside another class. Only the
/// "beside another class" cases (`wideModal`, menu `wide`) fail an exact-`className` check
/// (mock.rs modal and menu render paths): the stub's `classList.add` on an empty class writes
/// exactly `scrim` or `menu`. Keep those two cases.
#[test]
fn a_modal_or_menu_target_renders_its_element_by_class_token() {
    let repo = opener_repo("render-class-token", &[]);
    for opener in ["classListModal", "wideModal"] {
        let html = crate::mock::render_modal(repo.path(), opener, &["a"]).unwrap_or_else(|e| panic!("PAGES.{opener}: {e:#}"));
        assert_eq!(html, r#"<div class="scrim"></div>"#, "PAGES.{opener}");
    }
    let script = concat!(
        "UI.openMenu = function (ev, id) { const el = document.createElement(\"div\");",
        " if (id === \"wide\") el.className = \"menu wide\"; else el.classList.add(\"menu\");",
        " el.innerHTML = '<div class=\"menu-item\"></div>'; document.body.appendChild(el); };\n",
    );
    let menus = FakeRepo::with_openers("render-menu-class-token", &mock_html(&body(true)), PAGE_MAP, &[], script);
    for id in ["listed", "wide"] {
        let html = crate::mock::render_menu(menus.path(), id, Some("players")).unwrap_or_else(|e| panic!("menu {id}: {e:#}"));
        assert_eq!(html, r#"<div class="menu"><div class="menu-item"></div></div>"#, "menu {id}");
    }
}

/// A timer callback runs after the body, its modal named by the `PAGES` call that scheduled it;
/// one scheduled by the body itself (an interval, run once) is named by the `PAGES` call it
/// makes. Timers that never stop fail by name.
#[test]
fn a_deferred_opener_is_a_modal_and_endless_timers_fail() {
    let repo = opener_repo("modal-deferred", &[("index", "PAGES.deferredModal()"), ("index", "setInterval(function () { PAGES.testOpener(); }, 10)")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("deferred modals"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index PAGES.deferredModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  mockup modal index PAGES.testOpener is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS",
    );
    assert_eq!(e, want);
    let endless = opener_repo("modal-endless", &[("index", "PAGES.forever()")]);
    let e = format!("{:#}", renders(endless.path(), &LISTS).expect_err("endless timers"));
    assert_eq!(e, "mockup onclick failed:\n  onclick \"PAGES.forever()\" drawn on index threw in the harness: timers still pending after 10 rounds");
}

/// A body guarded on an ancestor (`closest`) or a typed value takes the path a user reaches, so
/// the modal behind the guard is seen.
#[test]
fn a_guarded_opener_is_a_modal() {
    let repo = opener_repo("modal-guarded", &[("index", "if (this.closest('tr')) PAGES.extraModal()"), ("index", "if (this.querySelector('input').value) PAGES.testOpener()")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("guarded modals"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index PAGES.extraModal is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  mockup modal index PAGES.testOpener is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS",
    );
    assert_eq!(e, want);
}

/// A body that throws in the harness fails with its host and its text, as drawn; its
/// non-throwing twin passes.
#[test]
fn an_onclick_that_throws_fails_by_name() {
    let repo = opener_repo("modal-boom", &[("index", "PAGES.boom('x')")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("throws"));
    assert_eq!(e, "mockup onclick failed:\n  onclick \"PAGES.boom('x')\" drawn on index threw in the harness: kaboom");
    let twin = opener_repo("modal-boom-twin", &[("index", "PAGES.toastOnly('x')")]);
    renders(twin.path(), &LISTS).expect("no throw");
}

/// A `.scrim` a body appends outside every `PAGES` call is a modal of the body itself: drawn
/// unlisted it fails by its text, listed as that `onclick` it passes, and listed as a `PAGES`
/// function of that name it is no listing of it.
#[test]
fn a_scrim_outside_every_pages_call_is_a_modal_of_its_onclick() {
    let repo = opener_repo("modal-inline", &[("index", "inlineScrim()")]);
    let e = format!("{:#}", renders(repo.path(), &LISTS).expect_err("unlisted"));
    assert_eq!(e, "modal drift:\n  mockup modal index onclick \"inlineScrim()\" is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
    let inline = MockModalExclusion { opener: MockOpener::Onclick("inlineScrim()"), ..pages_modal("index", "", "why") };
    renders(repo.path(), &Lists { excluded_mock_modals: &mock_modals(&[inline]), ..LISTS }).expect("listed as its onclick");
    let named = mock_modals(&[pages_modal("index", "inlineScrim", "why")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &named, ..LISTS }).expect_err("no PAGES function"));
    let want = concat!(
        "modal drift:\n",
        "  mockup modal index onclick \"inlineScrim()\" is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  EXCLUDED_MOCK_MODALS lists index PAGES.inlineScrim, which no compared index mockup render draws as a modal",
    );
    assert_eq!(e, want);
}

/// A body that calls two `PAGES` functions has a modal of each one whose call appended a
/// `.scrim`, and of no other: a toast before the modal leaves the toast no modal, and two modal
/// openers are two modals.
#[test]
fn a_body_calling_two_pages_functions_has_a_modal_of_each_that_appends_one() {
    let repo = opener_repo("modal-two", &[("index", "PAGES.toastOnly();PAGES.extraModal()")]);
    renders(repo.path(), &Lists { excluded_mock_modals: &mock_modals(&[pages_modal("index", "extraModal", "why")]), ..LISTS }).expect("one modal");
    let toast = mock_modals(&[pages_modal("index", "extraModal", "why"), pages_modal("index", "toastOnly", "why")]);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &toast, ..LISTS }).expect_err("the toast is no modal"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MOCK_MODALS lists index PAGES.toastOnly, which no compared index mockup render draws as a modal");
    let two = opener_repo("modal-two-modals", &[("index", "PAGES.extraModal();PAGES.testOpener()")]);
    let e = format!("{:#}", renders(two.path(), &Lists { excluded_mock_modals: &mock_modals(&[pages_modal("index", "extraModal", "why")]), ..LISTS }).expect_err("two"));
    assert_eq!(e, "modal drift:\n  mockup modal index PAGES.testOpener is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
}

/// Each body runs verbatim on its own fresh page: the statement before its `PAGES` call runs
/// (`window.armed=true` makes `armedModal` a modal opener, named by the function the body called,
/// not the `extraModal` it calls), and nothing it set reaches the next body, which would open an
/// unlisted `extraModal` if it did. Without the statement `armedModal` is no modal.
#[test]
fn each_onclick_runs_verbatim_on_its_own_fresh_page() {
    let armed = mock_modals(&[pages_modal("index", "armedModal", "why")]);
    let repo = opener_repo("modal-armed", &[("index", "window.armed=true;PAGES.armedModal()"), ("index", "if (window.armed) PAGES.extraModal()")]);
    renders(repo.path(), &Lists { excluded_mock_modals: &armed, ..LISTS }).expect("armed, and alone");
    let unarmed = opener_repo("modal-unarmed", &[("index", "PAGES.armedModal()")]);
    let e = format!("{:#}", renders(unarmed.path(), &Lists { excluded_mock_modals: &armed, ..LISTS }).expect_err("unarmed"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MOCK_MODALS lists index PAGES.armedModal, which no compared index mockup render draws as a modal");
}

/// The onclick probe runs on the page forced as its render is: with the index entry forcing, a
/// body whose opener appends its `.scrim` only in the ok state is a mockup modal of index, unlisted
/// failing and listed passing; with the unforced production entry the same listing is stale.
#[test]
fn the_onclick_probe_of_a_forced_page_sees_a_modal_that_opens_only_forced() {
    let ok_only = concat!(
        "PAGES.okOnly = function () {\n",
        "  if (UI.state.force !== \"ok\") return;\n",
        "  const el = document.createElement(\"div\");\n",
        "  el.className = \"scrim\";\n",
        "  document.body.appendChild(el);\n",
        "};\n",
    );
    let forced: Vec<Page> = PAGE_MAP.iter().map(|p| Page { force: p.id() == "index", ..*p }).collect();
    assert_eq!(forced.iter().filter(|p| p.force).count(), 1, "PAGE_MAP lists index once");
    let repo = FakeRepo::with_openers("modal-forced", &mock_html(&body(true)), &forced, &[("index", "PAGES.okOnly()")], ok_only);
    let e = format!("{:#}", renders(repo.path(), &Lists { page_map: &forced, ..LISTS }).expect_err("forced, okOnly opens"));
    assert_eq!(e, "modal drift:\n  mockup modal index PAGES.okOnly is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
    let listed = mock_modals(&[pages_modal("index", "okOnly", "why")]);
    renders(repo.path(), &Lists { page_map: &forced, excluded_mock_modals: &listed, ..LISTS }).expect("forced and listed");
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &listed, ..LISTS }).expect_err("unforced, okOnly opens nothing"));
    assert_eq!(e, "modal drift:\n  EXCLUDED_MOCK_MODALS lists index PAGES.okOnly, which no compared index mockup render draws as a modal");
}

/// A menu's onclick probe runs on its host forced as the host entry is (a menu entry never
/// forces): with the players entry forcing, an opener drawn in the players user menu that appends
/// its `.scrim` only in the ok state is a mockup modal of that menu, unlisted failing and listed
/// passing; probed with the menu entry's own unforced flag it would open nothing.
#[test]
fn the_onclick_probe_of_a_forced_hosts_menu_sees_a_modal_that_opens_only_forced() {
    let ok_only = concat!(
        "PAGES.okOnly = function () {\n",
        "  if (UI.state.force !== \"ok\") return;\n",
        "  const el = document.createElement(\"div\");\n",
        "  el.className = \"scrim\";\n",
        "  document.body.appendChild(el);\n",
        "};\n",
    );
    let forced: Vec<Page> = PAGE_MAP.iter().map(|p| Page { force: p.id() == "players", ..*p }).collect();
    assert_eq!(forced.iter().filter(|p| p.force).count(), 1, "PAGE_MAP lists players once");
    let menu = forced.iter().find(|p| p.id() == "players.menu.user").expect("PAGE_MAP lists players.menu.user");
    assert!(!menu.force, "the menu entry itself does not force");
    let drawn = [("players.menu.user", "PAGES.okOnly()")];
    let repo = FakeRepo::with_openers("modal-forced-menu", &mock_html(&body(true)), &forced, &drawn, ok_only);
    let e = format!("{:#}", renders(repo.path(), &Lists { page_map: &forced, ..LISTS }).expect_err("forced host, okOnly opens"));
    assert_eq!(e, "modal drift:\n  mockup modal players.menu.user PAGES.okOnly is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS");
    let listed = mock_modals(&[pages_modal("players.menu.user", "okOnly", "why")]);
    renders(repo.path(), &Lists { page_map: &forced, excluded_mock_modals: &listed, ..LISTS }).expect("forced and listed");
}

/// A `PAGE_MAP` modal entry whose opener its compared host does not draw fails, on a stand-in of
/// the page map without modal entries; the stand-in that draws it passes.
#[test]
fn a_modal_entry_its_host_does_not_draw_fails() {
    let map = modal_map();
    let lists = Lists { page_map: &map, ..LISTS };
    let bare: Vec<Page> = PAGE_MAP.iter().copied().filter(|p| p.modal.is_none()).collect();
    let undrawn = FakeRepo::with_openers("modal-undrawn", &mock_html(&body(true)), &bare, &[], OPENERS);
    let e = format!("{:#}", renders(undrawn.path(), &lists).expect_err("index draws no testOpener"));
    assert_eq!(e, "modal drift:\n  PAGE_MAP index.modal.test opens PAGES.testOpener, which the index mockup render does not draw as a modal");
    let drawn = FakeRepo::with_map("modal-drawn", &mock_html(&body(true)), &map);
    renders(drawn.path(), &lists).expect("index draws testOpener");
}

/// An `EXCLUDED_MODALS` claim that the mockup has no opener of an action fails when its mockup
/// host draws a modal opener excluded with that same portal action or with no portal side, on a
/// page and on the page of an excluded menu host; excluded with another action or the palette, it
/// passes.
#[test]
fn a_false_no_mock_opener_claim_fails() {
    let repo = opener_repo("modal-claim", &[("mail", "PAGES.extraModal('x')"), ("events", "PAGES.extraModal('y')")]);
    let with = |mail: PortalModal<'static>, events: PortalModal<'static>| {
        mock_modals(&[
            MockModalExclusion { portal: mail, ..pages_modal("mail", "extraModal", "why") },
            MockModalExclusion { portal: events, ..pages_modal("events", "extraModal", "why") },
        ])
    };
    let same = with(PortalModal::Action("mail/test-send"), PortalModal::Action("core.events/sub.pause"));
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &same, ..LISTS }).expect_err("claims false"));
    let want = concat!(
        "modal drift:\n",
        "  EXCLUDED_MODALS mail modal mail/test-send says the mockup has no opener of it, but mail draws PAGES.extraModal, ",
        "which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there\n",
        "  EXCLUDED_MODALS events.excluded-menu.subscription modal core.events/sub.pause says the mockup has no opener of it, ",
        "but events draws PAGES.extraModal, which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there",
    );
    assert_eq!(e, want);
    let absent = with(PortalModal::Absent, PortalModal::Absent);
    let e = format!("{:#}", renders(repo.path(), &Lists { excluded_mock_modals: &absent, ..LISTS }).expect_err("absent holds no claim"));
    assert_eq!(e, want);
    for (mail, events) in [(PortalModal::Action("mail/other"), PortalModal::Action("core.events/other")), (PortalModal::Palette, PortalModal::Palette)] {
        renders(repo.path(), &Lists { excluded_mock_modals: &with(mail, events), ..LISTS }).expect("another action");
    }
}

/// The inventory a clean [`crate::pages::validate_mock_modals`] call of the production lists sees,
/// without the palette: `mail` draws its excluded `cancelMail`, `events` its mapped `skipPoison`.
fn claim_inventory() -> Vec<(String, Vec<MockOpener<String>>)> {
    let pages = |name: &str| vec![MockOpener::Pages(name.to_string())];
    vec![("mail".to_string(), pages("cancelMail")), ("events".to_string(), pages("skipPoison"))]
}

/// The production lists with only the `mail` `cancelMail` mockup exclusion, which [`claim_inventory`]
/// draws.
fn claim_exclusions() -> Vec<MockModalExclusion<'static>> {
    EXCLUDED_MOCK_MODALS.iter().copied().filter(|x| (x.host, x.opener) == ("mail", MockOpener::Pages("cancelMail"))).collect()
}

/// An `EXCLUDED_MODALS` no-opener claim whose mockup host has no compared render fails, as
/// nothing checked it; the inventory with the host passes.
#[test]
fn a_no_mock_opener_claim_on_an_unrendered_host_fails() {
    let excluded = claim_exclusions();
    let lists = Lists { excluded_mock_modals: &excluded, ..LISTS };
    crate::pages::validate_mock_modals(&claim_inventory(), &lists).expect("both hosts rendered");
    let mail_only: Vec<_> = claim_inventory().into_iter().filter(|(h, _)| h == "mail").collect();
    let e = format!("{:#}", crate::pages::validate_mock_modals(&mail_only, &lists).expect_err("events unrendered"));
    let want = concat!(
        "modal drift:\n  EXCLUDED_MODALS events.excluded-menu.subscription modal core.events/sub.pause says the mockup has no opener of it, ",
        "but no compared events mockup render was checked",
    );
    assert_eq!(e, want);
}

/// An `EXCLUDED_MODALS` no-opener claim fails when a `PAGE_MAP` modal entry maps an opener its
/// mockup host draws to the claimed action; the production claims pass.
#[test]
fn a_no_mock_opener_claim_of_a_mapped_action_fails() {
    let excluded = claim_exclusions();
    let lists = Lists { excluded_mock_modals: &excluded, ..LISTS };
    crate::pages::validate_mock_modals(&claim_inventory(), &lists).expect("the production claims");
    let claims = [("events", "core.events/poison.skip", ModalWhy { why: "why" })];
    let e = format!("{:#}", crate::pages::validate_mock_modals(&claim_inventory(), &Lists { excluded_modals: &claims, ..lists }).expect_err("mapped"));
    let want = concat!(
        "modal drift:\n  EXCLUDED_MODALS events modal core.events/poison.skip says the mockup has no opener of it, but events draws PAGES.skipPoison, ",
        "which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there",
    );
    assert_eq!(e, want);
}

/// An `EXCLUDED_MOCK_MODALS` portal action that is no `<module>/<action>` fails; the production
/// one passes.
#[test]
fn a_malformed_mock_modal_portal_action_fails() {
    let excluded = claim_exclusions();
    crate::pages::validate_mock_modals(&claim_inventory(), &Lists { excluded_mock_modals: &excluded, ..LISTS }).expect("mail/cancel");
    let bad: Vec<_> = excluded.iter().map(|x| MockModalExclusion { portal: PortalModal::Action("Mail cancel"), ..*x }).collect();
    let e = format!("{:#}", crate::pages::validate_mock_modals(&claim_inventory(), &Lists { excluded_mock_modals: &bad, ..LISTS }).expect_err("malformed"));
    let want = "modal drift:\n  EXCLUDED_MOCK_MODALS mail PAGES.cancelMail names portal action \"Mail cancel\", which is not a <module>/<action> of [a-z0-9._-]+ ids";
    assert_eq!(e, want);
}

/// The real mockup, rendered through node: every modal opener it draws on a compared page, tab
/// or menu is declared; dropping the exclusion of `mail`'s `cancelMail` or of its palette (each
/// also leaving the `mail/test-send` claim unproven), is caught, and so is the `mail/test-send` claim when `cancelMail` is excluded as that action.
#[test]
fn the_real_mockup_draws_only_declared_modals() {
    let repo = crate::repo_root();
    renders(&repo, &LISTS).expect("every real mockup modal is declared");
    let cancel = ("mail", MockOpener::Pages("cancelMail"));
    let fewer: Vec<_> = EXCLUDED_MOCK_MODALS.iter().copied().filter(|x| (x.host, x.opener) != cancel).collect();
    assert_eq!(fewer.len() + 1, EXCLUDED_MOCK_MODALS.len(), "EXCLUDED_MOCK_MODALS lists mail cancelMail");
    let e = format!("{:#}", renders(&repo, &Lists { excluded_mock_modals: &fewer, ..LISTS }).expect_err("cancelMail undeclared"));
    let want = concat!(
        "modal drift:\n  mockup modal mail PAGES.cancelMail is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n",
        "  EXCLUDED_MODALS mail modal mail/test-send says the mockup has no opener of it, but mail draws PAGES.cancelMail, ",
        "which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there",
    );
    assert_eq!(e, want);
    let search = ("mail", MockOpener::Onclick(PALETTE_ONCLICK));
    let fewer: Vec<_> = EXCLUDED_MOCK_MODALS.iter().copied().filter(|x| (x.host, x.opener) != search).collect();
    assert_eq!(fewer.len() + 1, EXCLUDED_MOCK_MODALS.len(), "EXCLUDED_MOCK_MODALS lists the mail palette");
    let e = format!("{:#}", renders(&repo, &Lists { excluded_mock_modals: &fewer, ..LISTS }).expect_err("the palette undeclared"));
    let want = format!(
        "modal drift:\n  mockup modal mail onclick {PALETTE_ONCLICK:?} is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS\n  {}{PALETTE_ONCLICK:?}, {}",
        "EXCLUDED_MODALS mail modal mail/test-send says the mockup has no opener of it, but mail draws onclick ",
        "which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there",
    );
    assert_eq!(e, want);
    let claimed: Vec<_> = EXCLUDED_MOCK_MODALS
        .iter()
        .map(|x| if (x.host, x.opener) == cancel { MockModalExclusion { portal: PortalModal::Action("mail/test-send"), ..*x } } else { *x })
        .collect();
    let e = format!("{:#}", renders(&repo, &Lists { excluded_mock_modals: &claimed, ..LISTS }).expect_err("cancelMail as test-send"));
    let want = concat!(
        "modal drift:\n  EXCLUDED_MODALS mail modal mail/test-send says the mockup has no opener of it, but mail draws PAGES.cancelMail, ",
        "which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there",
    );
    assert_eq!(e, want);
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::mock::mock_tests::FakeRepo;
use crate::pages::{EXCLUDED_MOCK_MENUS, EXCLUDED_TABS, PAGE_MAP};
use crate::{crawl, parse_args, run_diff, validate_renders, Command, DEFAULT_BASE_URL, DEFAULT_USER};

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
    assert!(!run_diff(tiles_only.path(), "index", portal.path(), false).expect("runs"), "no panel matched");

    let with_panel = FakeRepo::new("diff-one", &mock_html(&body(true)));
    let portal = crawl_dir("diff-one-crawl", &portal_html(&body(true)), true);
    let clean = run_diff(with_panel.path(), "index", portal.path(), false).expect("runs");
    assert!(clean, "the same page with a panel is clean");
}

#[test]
fn run_diff_refuses_a_crawl_without_its_clean_marker() {
    let repo = FakeRepo::new("diff-clean", &mock_html(&body(true)));
    let dir = crawl_dir("diff-unclean-crawl", &portal_html(&body(true)), false);
    let e = format!("{:#}", run_diff(repo.path(), "index", dir.path(), false).expect_err("no CLEAN marker"));
    assert!(e.contains("has no CLEAN marker: its crawl failed, did not finish or ran with --allow-errors"), "{e}");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    assert!(run_diff(repo.path(), "index", dir.path(), false).expect("runs once marked clean"));
}

#[test]
fn run_diff_refuses_a_pending_page() {
    let repo = FakeRepo::new("diff-pending", &mock_html(&body(true)));
    let dir = crawl_dir("diff-pending-crawl", &portal_html(&body(true)), true);
    let e = format!("{:#}", run_diff(repo.path(), "apikeys", dir.path(), false).expect_err("pending"));
    assert!(e.contains("page apikeys is pending"), "{e}");
}

#[test]
fn run_diff_fails_on_an_unaccepted_difference_and_on_a_stale_accept_entry() {
    let repo = FakeRepo::new("diff-accept", &mock_html(&body(true)));
    let extra = format!("{}<footer></footer>", body(true));
    let dir = crawl_dir("diff-accept-crawl", &portal_html(&extra), true);
    assert!(!run_diff(repo.path(), "index", dir.path(), false).expect("runs"), "an unaccepted portal-only footer");

    let accept = repo.path().join("tools/admindiff/accept/index.txt");
    write(&accept, "frame div.app: portal-only footer | template choice: test\n");
    assert!(run_diff(repo.path(), "index", dir.path(), false).expect("runs"), "the accepted difference passes");

    write(&accept, "frame div.app: portal-only footer | template choice: test\nframe div.app: mock-only nav | gone\n");
    assert!(!run_diff(repo.path(), "index", dir.path(), false).expect("runs"), "a stale entry fails");

    write(&accept, "frame div.app: portal-only footer\n");
    assert!(run_diff(repo.path(), "index", dir.path(), false).is_err(), "a malformed accept file is an error");
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
    assert!(run_diff(live.path(), "index", portal.path(), false).expect("runs"), "the clean twin");

    let bars = r#"<div class="uptime"><i></i></div>"#;
    assert!(body(true).contains(bars), "the fixture holds the bars it removes");
    let dead = FakeRepo::new("diff-repeat-dead", &mock_html(&body(true).replace(bars, "")));
    let e = format!("{:#}", run_diff(dead.path(), "index", portal.path(), false).expect_err("a dead REPEAT entry"));
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
    assert!(!run_diff(repo.path(), "index", dir.path(), false).expect("runs"), "the extra mock row is unaccepted");

    let wrapped = crate::html::parse(&format!(r#"<div class="app">{row}</div>"#)).expect("parses");
    let root = crate::skeleton::prepare(&wrapped, &[], "t").expect("has .app");
    let label = crate::skeleton::page(&root, false).frame.children[0].label();
    let key = format!("frame div.app > table > tbody > tr*: mock-only {label}");
    let accept = repo.path().join("tools/admindiff/accept/index.txt");
    write(&accept, &format!("{key} (\"done · m-0\") | data difference: test\n"));
    assert!(run_diff(repo.path(), "index", dir.path(), false).expect("runs"), "the hinted entry accepts the row");
}

/// The production `validate_renders` call in `run_diff`: the clean twin's player render
/// drawing one more tab, which neither a page map tab entry nor `EXCLUDED_TABS` declares, is
/// an error before anything is compared. The stand-in renders the same body for every page.
#[test]
fn run_diff_fails_on_a_mockup_tab_no_list_declares() {
    let portal = crawl_dir("diff-tab-crawl", &portal_html(&body(true)), true);
    let live = FakeRepo::new("diff-tab-live", &mock_html(&body(true)));
    assert!(run_diff(live.path(), "index", portal.path(), false).expect("runs"), "the clean twin");

    let zzz = r#"<button class="tab" onclick="PAGES.tab='zzz';REG.render()">zzz</button>"#;
    let undeclared = FakeRepo::new("diff-tab-undeclared", &mock_html(&format!("{}{zzz}", body(true))));
    let e = format!("{:#}", run_diff(undeclared.path(), "index", portal.path(), false).expect_err("an undeclared tab"));
    assert_eq!(e, "tab drift:\n  mockup tab player.zzz is in neither PAGE_MAP nor EXCLUDED_TABS");
}

/// The real mockup, rendered through node: every tab it draws on every `FirstRow` page is
/// declared, and dropping one exclusion is caught.
#[test]
fn the_real_mockup_draws_only_declared_tabs() {
    let repo = crate::repo_root();
    validate_renders(&repo, PAGE_MAP, EXCLUDED_TABS, EXCLUDED_MOCK_MENUS).expect("every real mockup tab is declared");
    let fewer: Vec<(&str, &str)> = EXCLUDED_TABS.iter().copied().filter(|(id, _)| *id != "player.wallet").collect();
    let e = format!("{:#}", validate_renders(&repo, PAGE_MAP, &fewer, EXCLUDED_MOCK_MENUS).expect_err("wallet undeclared"));
    assert_eq!(e, "tab drift:\n  mockup tab player.wallet is in neither PAGE_MAP nor EXCLUDED_TABS");
}

/// The real mockup, rendered through node: every menu it draws on a compared page is a menu
/// entry, a menu entry whose page draws no such menu fails, and dropping one entry is caught.
#[test]
fn the_real_mockup_draws_only_declared_menus() {
    let repo = crate::repo_root();
    let fewer: Vec<crate::pages::Page> = PAGE_MAP.iter().copied().filter(|p| p.id() != "players.menu.user").collect();
    assert_eq!(fewer.len() + 1, PAGE_MAP.len(), "PAGE_MAP lists players.menu.user");
    let e = format!("{:#}", validate_renders(&repo, &fewer, EXCLUDED_TABS, EXCLUDED_MOCK_MENUS).expect_err("players' user menu undeclared"));
    assert_eq!(e, "menu drift:\n  mockup menu players.menu.user is not a PAGE_MAP menu entry");
    let events = PAGE_MAP.iter().find(|p| p.page == "events").expect("PAGE_MAP lists events");
    let menu = crate::pages::Menu { mock: "user", kind: "player" };
    let mut more = PAGE_MAP.to_vec();
    more.push(crate::pages::Page { menu: Some(menu), ..*events });
    let e = format!("{:#}", validate_renders(&repo, &more, EXCLUDED_TABS, EXCLUDED_MOCK_MENUS).expect_err("events draws no user menu"));
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
    let e = format!("{:#}", validate_renders(&repo, PAGE_MAP, EXCLUDED_TABS, &fewer).expect_err("repo undeclared"));
    assert_eq!(e, "menu drift:\n  mockup menu repo is in neither PAGE_MAP nor EXCLUDED_MOCK_MENUS");
    let more: Vec<(&str, &str)> = EXCLUDED_MOCK_MENUS.iter().copied().chain([("zzz", "why")]).collect();
    let e = format!("{:#}", validate_renders(&repo, PAGE_MAP, EXCLUDED_TABS, &more).expect_err("zzz is stale"));
    assert_eq!(e, "menu drift:\n  EXCLUDED_MOCK_MENUS lists zzz, which the mockup registry does not define");
}

/// Menus are inventoried on untabbed pages only: the real player page draws its user menu on
/// every tab, and a tab entry for the wallet tab needs no menu entry of its own.
#[test]
fn a_real_mockup_tab_entry_needs_no_menu_entry() {
    let repo = crate::repo_root();
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists player");
    let mut page_map = PAGE_MAP.to_vec();
    page_map.push(crate::pages::Page { tab: Some("wallet"), ..*player });
    let tabs: Vec<(&str, &str)> = EXCLUDED_TABS.iter().copied().filter(|(id, _)| *id != "player.wallet").collect();
    validate_renders(&repo, &page_map, &tabs, EXCLUDED_MOCK_MENUS).expect("the wallet tab draws the user menu undeclared");
}

/// A menu target compares the two `.menu` popovers, not the pages, and needs no panel: the
/// stand-in menu against the same menu is clean, and against a portal menu with one more item
/// it is not.
#[test]
fn run_diff_compares_a_menu_target_by_its_menu() {
    let repo = FakeRepo::new("diff-menu", &mock_html(&body(true)));
    let same = crate::mock::mock_tests::MOCK_MENU.replace("{id}", "user").replace("{arg}", "players");
    let same = same.replace(r#"<div class="frag"><span class="frag-label">user</span>"#, "").replacen("</div></div>", "</div>", 1);
    let menu = |inner: &str| format!(r#"<div class="menu" data-menu-popup="">{inner}</div>"#);
    let dir = ScratchDir::new("diff-menu-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    write(&dir.path().join("players.menu.user.html"), &menu(&same));
    assert!(run_diff(repo.path(), "players.menu.user", dir.path(), false).expect("runs"), "the same menu is clean");
    let more = format!(r#"{same}<div class="menu-item"><span>more</span></div>"#);
    write(&dir.path().join("players.menu.user.html"), &menu(&more));
    assert!(!run_diff(repo.path(), "players.menu.user", dir.path(), false).expect("runs"), "a portal-only item");
}

/// A modal target compares the two `.modal` dialogs, not the pages, and needs no panel: the
/// stand-in modal against a portal `.scrim` holding the same dialog is clean, beside a
/// portal-only sibling of the dialog in the `.scrim` too, and against one whose dialog has one
/// more child it is not.
#[test]
fn run_diff_compares_a_modal_target_by_its_modal() {
    let repo = FakeRepo::new("diff-modal", &mock_html(&body(true)));
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
    write(&dir.path().join("events.modal.skip-poison.html"), &modal(""));
    assert!(run_diff(repo.path(), "events.modal.skip-poison", dir.path(), false).expect("runs"), "the same dialog is clean");
    let open = r#"<div class="scrim" data-scrim=""><div class="modal">"#;
    let beside = modal("").replacen(open, r#"<div class="scrim" data-scrim=""><div class="scrim-note"><span></span></div><div class="modal">"#, 1);
    assert!(beside != modal(""), "the sibling is added: {beside}");
    write(&dir.path().join("events.modal.skip-poison.html"), &beside);
    assert!(run_diff(repo.path(), "events.modal.skip-poison", dir.path(), false).expect("runs"), "a portal-only sibling of the .modal");
    write(&dir.path().join("events.modal.skip-poison.html"), &modal(r#"<div class="modal-note"></div>"#));
    assert!(!run_diff(repo.path(), "events.modal.skip-poison", dir.path(), false).expect("runs"), "a portal-only child");
}

/// A clean crawl with no output for a menu or modal target skipped it as EMPTY: the diff names
/// that, the target's host and what it did not draw, instead of a missing-file error; the twin
/// with the modal written runs.
#[test]
fn run_diff_names_a_menu_or_modal_target_the_crawl_skipped() {
    let repo = FakeRepo::new("diff-skipped", &mock_html(&body(true)));
    let dir = ScratchDir::new("diff-skipped-crawl");
    write(&dir.path().join(crawl::CLEAN_MARKER), "");
    let shown = dir.path().display();
    let e = format!("{:#}", run_diff(repo.path(), "events.modal.skip-poison", dir.path(), false).expect_err("no modal"));
    let want = format!(
        "{shown} has no events.modal.skip-poison modal: the crawl skipped it as EMPTY (events was skipped or drew no core.events/poison.skip opener) or predates the entry"
    );
    assert_eq!(e, want);
    let e = format!("{:#}", run_diff(repo.path(), "players.menu.user", dir.path(), false).expect_err("no menu"));
    let want = format!(
        "{shown} has no players.menu.user menu: the crawl skipped it as EMPTY (players was skipped or drew no player menu) or predates the entry"
    );
    assert_eq!(e, want);
    let modal = r#"<div class="scrim" data-scrim=""><div class="modal"></div></div>"#;
    write(&dir.path().join("events.modal.skip-poison.html"), modal);
    run_diff(repo.path(), "events.modal.skip-poison", dir.path(), false).expect("the written modal is diffed");
}

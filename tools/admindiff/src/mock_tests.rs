//! These run `node`, a dev prerequisite (docs/reference/testing.md): without it they fail,
//! as `mock` does. `tests/mock_no_node.rs` covers the missing-node error.

use std::path::Path;

use super::render;
use crate::pages::{Route, EXCLUDED, EXCLUDED_TABS, PAGE_MAP};
use crate::tests::{write, ScratchDir};

/// A scratch repo whose mockup is a stand-in: every page the two lists name has its html
/// file, every script [`render`] loads exists, and `registry.js` defines `REG.mount` and a
/// `PAGES.<page>` for each page. `render()` returns the given html with `{search}` replaced by
/// `location.search`; after it, outside the html, a `FirstRow` page draws a tab bar as the real
/// mockup does: a first tab, then every `PAGE_MAP` tab entry and `EXCLUDED_TABS` tab of the
/// page, the one `location.search` names selected, else the first; then, as the real mockup
/// does, a `UI.openMenu` button for each `PAGE_MAP` menu entry whose host is the page with
/// the `tab` `location.search` names (none: an untabbed host), whose argument is the page.
/// `UI.openMenu` appends a `.menu` holding [`MOCK_MENU`] with `{arg}` and `{id}` replaced.
/// `REG.mount` assigns the body and then runs `meta.after`, as the real registry does; the
/// stand-in `after` prints [`AFTER_RAN`] and exits 0, so a harness that let it run would
/// return that instead of the page.
pub(crate) struct FakeRepo {
    dir: ScratchDir,
}

pub(crate) const AFTER_RAN: &str = "AFTER-RAN";

/// The stand-in menu body: one preview-labelled group, as the real `UI.openMenu` draws it.
pub(crate) const MOCK_MENU: &str = concat!(
    r#"<div class="frag"><span class="frag-label">{id}</span><div class="menu-group"><span>{id}</span></div>"#,
    r#"<div class="menu-item"><span>{arg}</span></div></div>"#,
);

impl FakeRepo {
    pub(crate) fn new(name: &str, html: &str) -> FakeRepo {
        let dir = ScratchDir::new(name);
        let redesign = dir.path().join("UILayout").join("redesign");
        let pages: Vec<&str> = PAGE_MAP.iter().map(|p| p.page).chain(EXCLUDED.iter().map(|(p, _)| *p)).collect();
        for page in &pages {
            write(&redesign.join(format!("{page}.html")), &format!("<script>PAGES.boot(\"{page}\")</script>"));
        }
        // Each page's tab ids: none, or the first tab and every declared one.
        let tabs: std::collections::BTreeMap<&str, Vec<&str>> = pages
            .iter()
            .map(|page| {
                let first_row = PAGE_MAP.iter().any(|p| p.page == *page && matches!(p.route, Route::FirstRow(_)));
                let mapped = PAGE_MAP.iter().filter(|p| p.page == *page && p.menu.is_none()).filter_map(|p| p.tab);
                let prefix = format!("{page}.");
                let excluded = EXCLUDED_TABS.iter().filter_map(|(id, _)| id.strip_prefix(prefix.as_str()));
                let ids = if first_row { std::iter::once("first").chain(mapped).chain(excluded).collect() } else { Vec::new() };
                (*page, ids)
            })
            .collect();
        // Each page's menus: [host tab or null, menu id].
        let mut menus: std::collections::BTreeMap<&str, Vec<(Option<&str>, &str)>> = std::collections::BTreeMap::new();
        for p in PAGE_MAP {
            if let Some(menu) = p.menu {
                menus.entry(p.page).or_default().push((p.tab, menu.mock));
            }
        }
        for script in super::SCRIPTS {
            write(&dir.path().join(script), "// stand-in\n");
        }
        let registry = format!(
            concat!(
                "var REG = {{ mount: function (meta, fn) {{ document.body.innerHTML = fn(); if (meta.after) meta.after(); }} }};\n",
                "var PAGES = {{}};\n",
                "const TABS = {tabs};\n",
                "const tabBar = function (ids) {{\n",
                "  if (!ids.length) return \"\";\n",
                "  const want = new URLSearchParams(location.search).get(\"tab\");\n",
                "  const on = ids.includes(want) ? want : ids[0];\n",
                "  const tab = id => '<button class=\"tab' + (id === on ? ' on' : '') + '\"'\n",
                "    + ' onclick=\"PAGES.tab=\\'' + id + '\\';REG.render()\">' + id + '</button>';\n",
                "  return '<div class=\"tabs\">' + ids.map(tab).join(\"\") + '</div>';\n",
                "}};\n",
                "const MENUS = {menus};\n",
                "const menuButtons = function (key) {{\n",
                "  const tab = new URLSearchParams(location.search).get(\"tab\");\n",
                "  return (MENUS[key] || []).filter(m => m[0] === tab).map(m =>\n",
                "    '<button class=\"btn\" onclick=\"UI.openMenu(event,\\'' + m[1] + '\\',\\'' + key + '\\')\"></button>').join(\"\");\n",
                "}};\n",
                "var UI = {{ openMenu: function (ev, id, arg) {{\n",
                "  ev.stopPropagation();\n",
                "  const el = document.createElement(\"div\");\n",
                "  el.className = \"menu\";\n",
                "  el.innerHTML = {menu}.split(\"{{arg}}\").join(arg).split(\"{{id}}\").join(id);\n",
                "  document.body.appendChild(el);\n",
                "}} }};\n",
                "for (const key of {keys}) PAGES[key] = {{\n",
                "  meta: {{ after: function () {{ process.stdout.write({after}); process.exit(0); }} }},\n",
                "  render: function () {{\n",
                "    return {html}.split(\"{{search}}\").join(location.search) + tabBar(TABS[key]) + menuButtons(key);\n",
                "  }},\n",
                "}};\n",
            ),
            keys = serde_json::to_string(&pages).expect("json"),
            after = serde_json::to_string(AFTER_RAN).expect("json"),
            html = serde_json::to_string(html).expect("json"),
            tabs = serde_json::to_string(&tabs).expect("json"),
            menus = serde_json::to_string(&menus).expect("json"),
            menu = serde_json::to_string(MOCK_MENU).expect("json"),
        );
        write(&dir.path().join("UILayout/redesign/assets/registry.js"), &registry);
        FakeRepo { dir }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }
}

fn real_repo() -> std::path::PathBuf {
    crate::repo_root()
}

#[test]
fn the_harness_captures_the_body_before_an_after_hook_can_run() {
    let repo = FakeRepo::new("mock-after", r#"<div class="app">search={search}</div>"#);
    let out = render(repo.path(), "index", "?u=aiko.tanaka").expect("renders");
    assert_eq!(out, r#"<div class="app">search=?u=aiko.tanaka</div>"#, "the page with its search, not {AFTER_RAN}");
}

#[test]
fn the_real_apikeys_reveal_hook_does_not_run() {
    // pages-platform.js: apikeys' `after` opens the reveal modal when `?reveal=1`; nothing
    // else reads `reveal`, so the body is the same with and without it.
    let plain = render(&real_repo(), "apikeys", "").expect("renders");
    let reveal = render(&real_repo(), "apikeys", "?reveal=1").expect("renders with ?reveal=1");
    assert!(plain.contains(r#"<div class="app">"#), "{plain}");
    assert_eq!(reveal, plain);
    assert!(!reveal.contains("Qm9yZWFsaXMt"), "the revealed secret is not in the page");
}

#[test]
fn a_missing_page_key_is_an_error() {
    let e = format!("{:#}", render(&real_repo(), "noSuchPage", "").expect_err("no P.noSuchPage"));
    assert!(e.contains("node failed rendering P.noSuchPage") && e.contains("PAGES.noSuchPage has no render()"), "{e}");
}

#[test]
fn a_missing_mockup_script_is_an_error() {
    let repo = FakeRepo::new("mock-missing-script", r#"<div class="app"></div>"#);
    render(repo.path(), "index", "").expect("the full stand-in renders");
    std::fs::remove_file(repo.path().join(super::SCRIPTS[1])).expect("remove bundle.js");
    let e = format!("{:#}", render(repo.path(), "index", "").expect_err("a script is missing"));
    assert!(e.starts_with("mockup script ") && e.ends_with("bundle.js is missing"), "{e}");
}

#[test]
fn a_render_that_never_assigns_the_body_is_an_error() {
    let repo = FakeRepo::new("mock-no-body", "");
    let registry = concat!(
        "var REG = { mount: function () {} };\n",
        "var PAGES = { index: { meta: {}, render: function () { return ''; } } };\n",
    );
    write(&repo.path().join("UILayout/redesign/assets/registry.js"), registry);
    let e = format!("{:#}", render(repo.path(), "index", "").expect_err("nothing captured"));
    assert!(e.contains("REG.mount returned without assigning document.body.innerHTML"), "{e}");
}

#[test]
fn check_tab_needs_the_selected_tab_button() {
    let doc = |html: &str| crate::html::parse(html).expect("parses");
    let on = doc(r#"<span><button class="tab on" onclick="PAGES.tab='items';REG.render()">Items</button></span>"#);
    super::check_tab(&on, "user", "items").expect("the selected tab");
    let off = doc(r#"<button class="tab" onclick="PAGES.tab='items';REG.render()">Items</button><button class="tab on" onclick="PAGES.tab='profile';REG.render()">Profile</button>"#);
    let e = format!("{:#}", super::check_tab(&off, "user", "items").expect_err("not selected"));
    assert_eq!(e, "P.user does not select tab items; the mockup has no such tab");
}

#[test]
fn drawn_tabs_reads_every_tab_id_and_the_selected_one() {
    let doc = |html: &str| crate::html::parse(html).expect("parses");
    let tabs = doc(concat!(
        r#"<div class="tabs"><span><button class="tab " onclick="PAGES.tab='profile';REG.render()">P</button></span>"#,
        r#"<span><button class="tab on" onclick="PAGES.tab='items';REG.render()">I</button></span>"#,
        r#"<button class="tab" onclick="PAGES.repoTab='files';REG.render()">F</button></div>"#,
    ));
    let drawn = super::drawn_tabs(&tabs).expect("reads");
    assert_eq!(drawn.tabs, ["profile", "items"], "another page's tab state is not a PAGES.tab tab");
    assert_eq!(drawn.selected.as_deref(), Some("items"));
    assert_eq!(super::drawn_tabs(&doc("<div></div>")).expect("reads"), crate::pages::DrawnTabs::default());
    let open = doc(r#"<button class="tab" onclick="PAGES.tab='items">I</button>"#);
    let e = format!("{:#}", super::drawn_tabs(&open).expect_err("an unterminated id"));
    assert!(e.contains("does not close its PAGES.tab id"), "{e}");
}

#[test]
fn menu_arg_reads_the_first_opener_of_that_menu() {
    let doc = |html: &str| crate::html::parse(html).expect("parses");
    let page = doc(concat!(
        r#"<button onclick="UI.openMenu(event,'lock','L-1')"></button>"#,
        r#"<button onclick="UI.openMenu(event,'user','aiko.tanaka')"></button>"#,
        r#"<button onclick="UI.openMenu(event,'user','bo')"></button>"#,
    ));
    assert_eq!(super::menu_arg(&page, "players", "user").expect("drawn"), "aiko.tanaka");
    let e = format!("{:#}", super::menu_arg(&page, "players", "use").expect_err("another id"));
    assert_eq!(e, "P.players draws no use menu; the mockup has no such menu");
    assert_eq!(super::drawn_menus(&page).expect("reads"), ["lock", "user"]);
    assert!(super::drawn_menus(&doc("<div></div>")).expect("reads").is_empty());
    let open = doc(r#"<button onclick="UI.openMenu(event,'user"></button>"#);
    assert!(format!("{:#}", super::drawn_menus(&open).expect_err("unterminated")).contains("does not close its menu id"));
}

/// The real mockup's user menu, opened through the harness: a `.menu` whose groups are the
/// registry's `entity.user.row_action` modules, with nothing of the page around it.
#[test]
fn the_harness_opens_the_real_user_menu() {
    let out = super::render_menu(&real_repo(), "user", "aiko.tanaka").expect("opens");
    assert!(out.starts_with(r#"<div class="menu">"#) && out.ends_with("</div>"), "{out}");
    assert!(out.contains(r#"class="menu-group""#) && out.contains("Suspend account"), "{out}");
    assert!(!out.contains(r#"class="app""#), "only the menu: {out}");
    let e = format!("{:#}", super::render_menu(&real_repo(), "nosuchmenu", "x").expect_err("no such menu"));
    assert!(e.contains("node failed rendering menu nosuchmenu"), "{e}");
}

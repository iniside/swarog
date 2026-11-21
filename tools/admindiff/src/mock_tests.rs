//! These run `node`, a dev prerequisite (docs/reference/testing.md): without it they fail,
//! as `mock` does. `tests/mock_no_node.rs` covers the missing-node error.

use std::path::Path;

use super::render;
use crate::pages::{validate_menus, MockOpener, Page, Route, EXCLUDED, EXCLUDED_MOCK_MENUS, EXCLUDED_MOCK_MODALS, EXCLUDED_TABS, PAGE_MAP};
use crate::tests::{write, ScratchDir};

/// A scratch repo whose mockup is a stand-in: every page the two lists name has its html
/// file, every script [`render`] loads exists, and `registry.js` defines `REG.mount` and a
/// `PAGES.<page>` for each page. `render()` returns the given html with `{search}` replaced by
/// `location.search`; after it, outside the html, a `FirstRow` page draws a tab bar as the real
/// mockup does: a first tab, then every `PAGE_MAP` tab entry and `EXCLUDED_TABS` tab of the
/// page, the one `location.search` names selected, else the first; then, as the real mockup
/// does, a `UI.openMenu` button for each `PAGE_MAP` menu entry whose host is the page with
/// the `tab` `location.search` names (none: an untabbed host), whose argument is the page;
/// the button of a menu whose host entry forces is drawn only on a render forced ok (`UI.state`
/// starts empty), as a mockup panel draws its row menus only in its ok state.
/// `UI.openMenu` appends a `.menu` holding [`MOCK_MENU`] with `{arg}` and `{id}` replaced;
/// `registry.js` defines, through `UI.defineMenu`, every menu `PAGE_MAP` and
/// `EXCLUDED_MOCK_MENUS` name, as the real registry defines its menus. Each `PAGE_MAP` modal
/// entry's opener is a `PAGES.<opener>` that appends a `.scrim` holding [`MOCK_MODAL`] with
/// `{opener}` and `{args}` (its arguments joined by `,`) replaced, as `UI.confirm` does, and so
/// is each `EXCLUDED_MOCK_MODALS` `PAGES` opener. Each modal entry's opener is drawn on its host,
/// with its arguments, and each `EXCLUDED_MOCK_MODALS` opener on its host, a `PAGES` opener with
/// none and an `onclick` opener as its body, as the real mockup draws them: on a page or tab host
/// each in its own `<button class="btn">` after the menu buttons, on a menu target in the
/// `onclick` of the menu's `.menu-item` (`{run}` in [`MOCK_MENU`]), joined by `;`, when the
/// menu opens with the host's page as its argument.
/// [`FakeRepo::with_openers`] draws more calls and appends a script defining their openers.
/// `REG.openSearch` appends an empty `.scrim`, as the real registry's palette does, and
/// `REG.render` assigns the body, as the real one re-renders the page.
/// `REG.mount` assigns the body and then runs `meta.after`, as the real registry does; the
/// stand-in `after` prints [`AFTER_RAN`] and exits 0, so a harness that let it run would
/// return that instead of the page. [`FakeRepo::with_map`] reads a given page map in place of
/// `PAGE_MAP`.
pub(crate) struct FakeRepo {
    dir: ScratchDir,
}

pub(crate) const AFTER_RAN: &str = "AFTER-RAN";

/// A stand-in menu button: its host's tab (none: an untabbed host), the menu id, and whether
/// its host entry forces, so the button is drawn only forced ok.
type MenuButton<'a> = (Option<&'a str>, &'a str, bool);

/// The stand-in menu body: one preview-labelled group, as the real `UI.openMenu` draws it.
pub(crate) const MOCK_MENU: &str = concat!(
    r#"<div class="frag"><span class="frag-label">{id}</span><div class="menu-group"><span>{id}</span></div>"#,
    r#"<div class="menu-item" onclick="{run}"><span>{arg}</span></div></div>"#,
);

/// The stand-in modal: a `.modal` whose head and body sit in a preview-labelled frag, as the
/// real `UI.confirm` draws them.
pub(crate) const MOCK_MODAL: &str = concat!(
    r#"<div class="modal"><div class="frag left-label"><span class="frag-label">{opener}</span>"#,
    r#"<div class="modal-head"><h2>{args}</h2></div><div class="modal-body"></div></div><div class="modal-foot"></div></div>"#,
);

impl FakeRepo {
    pub(crate) fn new(name: &str, html: &str) -> FakeRepo {
        FakeRepo::with_map(name, html, PAGE_MAP)
    }

    pub(crate) fn with_map(name: &str, html: &str, page_map: &[Page]) -> FakeRepo {
        FakeRepo::with_openers(name, html, page_map, &[], "")
    }

    /// [`FakeRepo::with_map`] that also draws each `(<host id>, <call>)` of `drawn` on its host,
    /// a page, tab or menu target of `page_map`, as a modal entry's opener is drawn, and appends
    /// `script` to `registry.js`, after `PAGES` and the stand-in modal openers are defined.
    pub(crate) fn with_openers(name: &str, html: &str, page_map: &[Page], drawn: &[(&str, &str)], script: &str) -> FakeRepo {
        let dir = ScratchDir::new(name);
        let redesign = dir.path().join("UILayout").join("redesign");
        let pages: Vec<&str> = page_map.iter().map(|p| p.page).chain(EXCLUDED.iter().map(|(p, _)| *p)).collect();
        for page in &pages {
            write(&redesign.join(format!("{page}.html")), &format!("<script>PAGES.boot(\"{page}\")</script>"));
        }
        // Each page's tab ids: none, or the first tab and every declared one.
        let tabs: std::collections::BTreeMap<&str, Vec<&str>> = pages
            .iter()
            .map(|page| {
                let first_row = page_map.iter().any(|p| p.page == *page && matches!(p.route, Route::FirstRow(_)));
                let mapped = page_map.iter().filter(|p| p.page == *page && p.is_page()).filter_map(|p| p.tab);
                let prefix = format!("{page}.");
                let excluded = EXCLUDED_TABS.iter().filter_map(|(id, _)| id.strip_prefix(prefix.as_str()));
                let ids = if first_row { std::iter::once("first").chain(mapped).chain(excluded).collect() } else { Vec::new() };
                (*page, ids)
            })
            .collect();
        // Each page's menus: [host tab or null, menu id, whether its host entry forces].
        let mut menus: std::collections::BTreeMap<&str, Vec<MenuButton>> = std::collections::BTreeMap::new();
        for p in page_map {
            if let Some(menu) = p.menu {
                let forced = page_map.iter().any(|q| q.hosts(&p.host()) && q.force);
                menus.entry(p.page).or_default().push((p.tab, menu.mock, forced));
            }
        }
        let mut defined: Vec<&str> = Vec::new();
        for id in page_map.iter().filter_map(|p| p.menu.map(|m| m.mock)).chain(EXCLUDED_MOCK_MENUS.iter().map(|(id, _)| *id)) {
            if !defined.contains(&id) {
                defined.push(id);
            }
        }
        let mut openers: Vec<&str> = Vec::new();
        let excluded_openers = EXCLUDED_MOCK_MODALS.iter().filter_map(|x| match x.opener {
            MockOpener::Pages(name) => Some(name),
            MockOpener::Onclick(_) => None,
        });
        for opener in page_map.iter().filter_map(|p| p.modal.map(|m| m.opener)).chain(excluded_openers) {
            if !openers.contains(&opener) {
                openers.push(opener);
            }
        }
        // The calls each host draws: a page or tab host keyed `<page>|<tab>`, a menu target keyed
        // `<menu id>|<page>`, as `UI.openMenu` gets its page as the argument.
        let mut calls: Vec<(String, String)> = Vec::new();
        for p in page_map {
            if let Some(m) = p.modal {
                let args: Vec<String> = m.args.iter().map(|a| format!("'{a}'")).collect();
                calls.push((p.host().id(), format!("PAGES.{}({})", m.opener, args.join(","))));
            }
        }
        calls.extend(EXCLUDED_MOCK_MODALS.iter().map(|x| {
            let call = match x.opener {
                MockOpener::Pages(name) => format!("PAGES.{name}()"),
                MockOpener::Onclick(body) => body.to_string(),
            };
            (x.host.to_string(), call)
        }));
        calls.extend(drawn.iter().map(|(host, call)| (host.to_string(), call.to_string())));
        let mut page_draws: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        let mut menu_draws: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for (host, call) in calls {
            let entry = page_map.iter().find(|q| q.id() == host && (q.is_page() || q.is_menu()));
            let entry = entry.unwrap_or_else(|| panic!("FakeRepo: {host} is no page, tab or menu target of the page map"));
            match entry.menu {
                Some(menu) => menu_draws.entry(format!("{}|{}", menu.mock, entry.page)).or_default().push(call),
                None => page_draws.entry(format!("{}|{}", entry.page, entry.tab.unwrap_or_default())).or_default().push(call),
            }
        }
        for script in super::SCRIPTS {
            write(&dir.path().join(script), "// stand-in\n");
        }
        let registry = format!(
            concat!(
                "var REG = {{ mount: function (meta, fn) {{ document.body.innerHTML = fn(); if (meta.after) meta.after(); }}, openSearch: function () {{\n",
                "  const el = document.createElement(\"div\");\n",
                "  el.className = \"scrim\";\n",
                "  document.body.appendChild(el);\n",
                "}}, render: function () {{ document.body.innerHTML = \"\"; }} }};\n",
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
                "  return (MENUS[key] || []).filter(m => m[0] === tab && (!m[2] || UI.state.force === \"ok\")).map(m =>\n",
                "    '<button class=\"btn\" onclick=\"UI.openMenu(event,\\'' + m[1] + '\\',\\'' + key + '\\')\"></button>').join(\"\");\n",
                "}};\n",
                "const PAGE_DRAWS = {page_draws};\n",
                "const MENU_DRAWS = {menu_draws};\n",
                "const pageOpeners = function (key) {{\n",
                "  const calls = PAGE_DRAWS[key + \"|\" + (new URLSearchParams(location.search).get(\"tab\") || \"\")] || [];\n",
                "  return calls.map(call => '<button class=\"btn\" onclick=\"' + call + '\"></button>').join(\"\");\n",
                "}};\n",
                "var UI = {{ state: {{}}, defineMenu: function () {{}}, openMenu: function (ev, id, arg) {{\n",
                "  ev.stopPropagation();\n",
                "  const el = document.createElement(\"div\");\n",
                "  el.className = \"menu\";\n",
                "  const run = (MENU_DRAWS[id + \"|\" + arg] || []).join(\";\");\n",
                "  el.innerHTML = {menu}.split(\"{{arg}}\").join(arg).split(\"{{id}}\").join(id).split(\"{{run}}\").join(run);\n",
                "  document.body.appendChild(el);\n",
                "}} }};\n",
                "for (const id of {defined}) UI.defineMenu(id, function () {{ return []; }});\n",
                "for (const opener of {openers}) PAGES[opener] = function (...args) {{\n",
                "  const el = document.createElement(\"div\");\n",
                "  el.className = \"scrim\";\n",
                "  el.innerHTML = {modal}.split(\"{{opener}}\").join(opener).split(\"{{args}}\").join(args.join(\",\"));\n",
                "  document.body.appendChild(el);\n",
                "}};\n",
                "for (const key of {keys}) PAGES[key] = {{\n",
                "  meta: {{ after: function () {{ process.stdout.write({after}); process.exit(0); }} }},\n",
                "  render: function () {{\n",
                "    return {html}.split(\"{{search}}\").join(location.search) + tabBar(TABS[key]) + menuButtons(key) + pageOpeners(key);\n",
                "  }},\n",
                "}};\n",
                "{script}\n",
            ),
            keys = serde_json::to_string(&pages).expect("json"),
            after = serde_json::to_string(AFTER_RAN).expect("json"),
            html = serde_json::to_string(html).expect("json"),
            tabs = serde_json::to_string(&tabs).expect("json"),
            menus = serde_json::to_string(&menus).expect("json"),
            menu = serde_json::to_string(MOCK_MENU).expect("json"),
            defined = serde_json::to_string(&defined).expect("json"),
            openers = serde_json::to_string(&openers).expect("json"),
            modal = serde_json::to_string(MOCK_MODAL).expect("json"),
            page_draws = serde_json::to_string(&page_draws).expect("json"),
            menu_draws = serde_json::to_string(&menu_draws).expect("json"),
            script = script,
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
    let out = render(repo.path(), "index", "?u=aiko.tanaka", false).expect("renders");
    let want = r#"<div class="app">search=?u=aiko.tanaka</div><button class="btn" onclick="REG.openSearch()"></button>"#;
    assert_eq!(out, want, "the page with its search and its palette opener, not {AFTER_RAN}");
}

#[test]
fn the_real_apikeys_reveal_hook_does_not_run() {
    // pages-platform.js: apikeys' `after` opens the reveal modal when `?reveal=1`; nothing
    // else reads `reveal`, so the body is the same with and without it.
    let plain = render(&real_repo(), "apikeys", "", false).expect("renders");
    let reveal = render(&real_repo(), "apikeys", "?reveal=1", false).expect("renders with ?reveal=1");
    assert!(plain.contains(r#"<div class="app">"#), "{plain}");
    assert_eq!(reveal, plain);
    assert!(!reveal.contains("Qm9yZWFsaXMt"), "the revealed secret is not in the page");
}

#[test]
fn a_missing_page_key_is_an_error() {
    let e = format!("{:#}", render(&real_repo(), "noSuchPage", "", false).expect_err("no P.noSuchPage"));
    assert!(e.contains("node failed rendering P.noSuchPage") && e.contains("PAGES.noSuchPage has no render()"), "{e}");
}

#[test]
fn a_missing_mockup_script_is_an_error() {
    let repo = FakeRepo::new("mock-missing-script", r#"<div class="app"></div>"#);
    render(repo.path(), "index", "", false).expect("the full stand-in renders");
    std::fs::remove_file(repo.path().join(super::SCRIPTS[1])).expect("remove bundle.js");
    let e = format!("{:#}", render(repo.path(), "index", "", false).expect_err("a script is missing"));
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
    let e = format!("{:#}", render(repo.path(), "index", "", false).expect_err("nothing captured"));
    assert!(e.contains("REG.mount returned without assigning document.body.innerHTML"), "{e}");
}

/// The stand-in page every [`probe`](super::probe) test runs its bodies on: `index`, untabbed.
fn index_page() -> &'static Page {
    PAGE_MAP.iter().find(|p| p.page == "index" && p.is_page() && p.tab.is_none()).expect("PAGE_MAP has the index page")
}

/// Probes every onclick of `html` on the stand-in `index` page.
fn probe_html(repo: &Path, html: &str) -> anyhow::Result<super::Probe> {
    probe_listed(repo, html, &[])
}

/// [`probe_html`] with `non_tab_keys` the `PAGES` keys a `.tab` that is not a tab may assign.
fn probe_listed(repo: &Path, html: &str, non_tab_keys: &[&str]) -> anyhow::Result<super::Probe> {
    let doc = crate::html::parse(html).expect("parses");
    super::probe(repo, "index", "index", index_page(), &doc, super::Onclicks::All, non_tab_keys)
}

/// The tabs and menus a render draws are what its onclicks do in the harness: each `.tab` whose
/// body assigns `PAGES.tab`, the first drawn `.on` selected, a tab of another page's state
/// (`PAGES.repoTab`, listed) not one; each menu `UI.openMenu` opens, with the argument of its first
/// opener. The calls are written as the mockup writes them; [`a_call_is_seen_whatever_its_spelling`]
/// is the twin with other spellings.
#[test]
fn the_probe_records_the_tabs_and_menus_the_onclicks_open() {
    let repo = FakeRepo::new("mock-probe-calls", r#"<div class="app"></div>"#);
    let probe = probe_listed(repo.path(), concat!(
        r#"<div class="tabs"><span><button class="tab " onclick="PAGES.tab='profile';REG.render()">P</button></span>"#,
        r#"<span><button class="tab on" onclick="PAGES.tab='items';REG.render()">I</button></span>"#,
        r#"<button class="tab" onclick="PAGES.repoTab='files';REG.render()">F</button></div>"#,
        r#"<button onclick="UI.openMenu(event,'lock','L-1')"></button>"#,
        r#"<button onclick="UI.openMenu(event,'user','aiko.tanaka')"></button>"#,
        r#"<button onclick="UI.openMenu(event,'user','bo')"></button>"#,
    ), &["repoTab"])
    .expect("probes");
    assert_eq!(probe.tabs.tabs, ["profile", "items"], "another page's tab state is not a PAGES.tab tab");
    assert_eq!(probe.tabs.selected.as_deref(), Some("items"));
    probe.check_tab("items").expect("the selected tab");
    let e = format!("{:#}", probe.check_tab("profile").expect_err("not selected"));
    assert_eq!(e, "P.index does not select tab profile; the mockup has no such tab");
    assert_eq!(probe.menu_ids(), ["lock", "user"]);
    assert_eq!(probe.menu_arg("user").expect("drawn"), Some("aiko.tanaka"));
    let e = format!("{:#}", probe.menu_arg("use").expect_err("another id"));
    assert_eq!(e, "P.index draws no use menu; the mockup has no such menu");

    let none = probe_html(repo.path(), "<div></div>").expect("probes nothing");
    assert_eq!(none.tabs, crate::pages::DrawnTabs::default());
    assert!(none.menus.is_empty() && none.modals.is_empty());
    let e = format!("{:#}", none.check_tab("items").expect_err("no tab"));
    assert_eq!(e, "P.index does not select tab items; the mockup has no such tab");
}

/// A call spelled with spaces and double quotes, which no text match on the mockup's own
/// spelling sees, opens the same tab and menu: the harness records the call, not its text.
/// A menu opened with no argument passes none.
#[test]
fn a_call_is_seen_whatever_its_spelling() {
    let repo = FakeRepo::new("mock-probe-spelling", r#"<div class="app"></div>"#);
    let probe = probe_html(repo.path(), concat!(
        r#"<button class="tab" onclick='PAGES.tab = "profile"; REG.render()'>P</button>"#,
        r#"<button class="tab on" onclick="window.PAGES.tab = &quot;items&quot;;REG.render()">I</button>"#,
        r#"<button onclick='UI.openMenu(event, "user", "aiko")'></button>"#,
        r#"<button onclick="UI.openMenu( event , 'lock' )"></button>"#,
    ))
    .expect("probes");
    assert_eq!(probe.tabs.tabs, ["profile", "items"]);
    probe.check_tab("items").expect("the selected tab, set with double quotes");
    assert_eq!(probe.menus, [("user".to_string(), Some("aiko".to_string())), ("lock".to_string(), None)]);
    assert_eq!(probe.menu_arg("user").expect("drawn with spaces and double quotes"), Some("aiko"));
}

/// A tab entry's render probes only its `.tab` onclicks: a throwing body elsewhere on the page is
/// not run, and one on a tab fails by name.
#[test]
fn a_tab_probe_runs_only_the_tab_onclicks() {
    let repo = FakeRepo::new("mock-probe-tabs", r#"<div class="app"></div>"#);
    let doc = crate::html::parse(r#"<button class="tab on" onclick="PAGES.tab='items'"></button><button onclick="nosuch()"></button>"#).expect("parses");
    let probe = super::probe(repo.path(), "index", "index", index_page(), &doc, super::Onclicks::Tabs, &[]).expect("the tab body only");
    probe.check_tab("items").expect("selected");
    let e = format!("{:#}", super::probe(repo.path(), "index", "index", index_page(), &doc, super::Onclicks::All, &[]).expect_err("nosuch throws"));
    assert!(e.contains(r#"onclick "nosuch()" drawn on index threw in the harness"#), "{e}");
}

/// The menu drift [`validate_menus`] reports for the menus `probe` recorded on the stand-in
/// `index` page, against the stand-in registry; empty when there is none.
fn menu_drift(repo: &Path, probe: &super::Probe) -> String {
    let defined = super::defined_menus(repo).expect("the stand-in registry");
    let drawn = [(index_page().id(), probe.menu_ids())];
    match validate_menus(&drawn, &defined, PAGE_MAP, EXCLUDED_MOCK_MENUS) {
        Ok(()) => String::new(),
        Err(e) => format!("{e:#}"),
    }
}

/// The probe records the calls that run, not their text: a call behind a guard that never holds
/// and a string that only names a call open no menu, and `PAGES.tab` assigned through a computed
/// key is a tab. A parser of the `onclick` text beside the probe would report the first two and
/// miss the third.
#[test]
fn only_the_calls_that_run_are_recorded() {
    let repo = FakeRepo::new("mock-probe-run", r#"<div class="app"></div>"#);
    let probe = probe_html(repo.path(), concat!(
        r#"<button onclick="if (false) UI.openMenu(event,'ghost','x')"></button>"#,
        r#"<button onclick="const s = &quot;UI.openMenu(event,'shade','y')&quot;; s.length"></button>"#,
        r#"<button class="tab on" onclick="const k = 'tab'; PAGES[k] = 'items'"></button>"#,
    ))
    .expect("probes");
    assert!(probe.menus.is_empty(), "no call ran: {:?}", probe.menus);
    assert_eq!(probe.tabs.tabs, ["items"]);
    assert_eq!(probe.tabs.selected.as_deref(), Some("items"));
}

/// Each tab is the first value its body assigns to `PAGES.tab`, and the selected tab is the first
/// `.tab.on` whose body assigns one, not a later one: a `.tab.on` of another page's state
/// (`PAGES.repoTab`), its key listed, is no tab and selects none, and a body that assigns twice
/// is the tab of its first assignment. With the key not listed, the same `.tab` fails by host
/// and onclick.
#[test]
fn the_selected_tab_is_the_first_on_tab_that_assigns_one() {
    let repo = FakeRepo::new("mock-probe-selected", r#"<div class="app"></div>"#);
    let html = concat!(
        r#"<button class="tab on" onclick="PAGES.repoTab='f'"></button>"#,
        r#"<button class="tab on" onclick="PAGES.tab='a'"></button>"#,
        r#"<button class="tab" onclick="PAGES.tab='b'; PAGES.tab='c'"></button>"#,
        r#"<button class="tab on" onclick="PAGES.tab='d'"></button>"#,
    );
    let probe = probe_listed(repo.path(), html, &["repoTab"]).expect("probes");
    assert_eq!(probe.tabs.selected.as_deref(), Some("a"));
    assert_eq!(probe.tabs.tabs, ["a", "b", "d"]);
    assert_eq!(probe.non_tab_keys, ["repoTab"]);
    let e = format!("{:#}", probe_html(repo.path(), html).expect_err("repoTab not listed"));
    let want = r#"mockup onclick failed:
  onclick "PAGES.repoTab='f'" drawn on index is a .tab that assigns no PAGES.tab and no NON_TAB_KEYS key"#;
    assert_eq!(e, want);
}

/// A `.tab` whose body assigns nothing, `.on` or not, fails by host and onclick, once per body;
/// so does one whose assignment sits behind a guard the stubs make false, as the probe records the
/// assignments that run. A listed key another tab assigns does not excuse it.
#[test]
fn a_tab_that_assigns_no_tab_fails() {
    let repo = FakeRepo::new("mock-probe-no-tab", r#"<div class="app"></div>"#);
    let html = concat!(
        r#"<button class="tab on" onclick="PAGES.tab='a'"></button>"#,
        r#"<button class="tab" onclick="REG.render()"></button>"#,
        r#"<button class="tab on" onclick="REG.render()"></button>"#,
        r#"<button class="tab" onclick="if (false) PAGES.tab='x'"></button>"#,
        r#"<button class="tab" onclick="PAGES.repoTab='f'"></button>"#,
    );
    let e = format!("{:#}", probe_listed(repo.path(), html, &["repoTab"]).expect_err("two bodies assign no tab"));
    let want = concat!(
        "mockup onclick failed:\n",
        r#"  onclick "REG.render()" drawn on index is a .tab that assigns no PAGES.tab and no NON_TAB_KEYS key"#,
        "\n",
        r#"  onclick "if (false) PAGES.tab='x'" drawn on index is a .tab that assigns no PAGES.tab and no NON_TAB_KEYS key"#,
    );
    assert_eq!(e, want);
    let tabs = super::probe(repo.path(), "index", "index", index_page(), &crate::html::parse(html).expect("parses"), super::Onclicks::Tabs, &["repoTab"]);
    assert_eq!(format!("{:#}", tabs.expect_err("a tab probe checks too")), want);
}

/// A `.tab` with no `onclick` (a link, say) fails by host and element on every probed render, a
/// tab probe among them, and on a render that draws no `onclick` at all.
#[test]
fn a_tab_with_no_onclick_fails() {
    let repo = FakeRepo::new("mock-probe-bare-tab", r#"<div class="app"></div>"#);
    let want = r##"mockup onclick failed:
  <a class="tab" href="#items"> ("Items") drawn on index is a .tab with no onclick"##;
    let with = concat!(r#"<button class="tab on" onclick="PAGES.tab='a'"></button>"#, r##"<a class="tab" href="#items">Items</a>"##);
    assert_eq!(format!("{:#}", probe_html(repo.path(), with).expect_err("a bare tab")), want);
    let doc = crate::html::parse(with).expect("parses");
    let tabs = super::probe(repo.path(), "index", "index", index_page(), &doc, super::Onclicks::Tabs, &[]);
    assert_eq!(format!("{:#}", tabs.expect_err("a tab probe checks too")), want);
    let alone = probe_html(repo.path(), r##"<a class="tab" href="#items">Items</a>"##);
    assert_eq!(format!("{:#}", alone.expect_err("no onclick at all")), want);
}

/// Only a body's own assignments say which tab it is: one the mount makes, or a re-render the body
/// calls, or a timer that re-render schedules or a timer such a timer schedules, fails the probe by
/// host, for `PAGES.tab` and for a listed key alike, assigned through an alias of `PAGES`; the
/// twin with no assigning render is clean, a listed key a `PAGES` function assigns through an
/// alias the scripts took while loading recorded as the body's. Without the check the `.tab` whose
/// body only re-renders would pass as the render's tab.
#[test]
fn a_render_that_assigns_a_tab_key_fails() {
    let html = r#"<button class="tab on" onclick="PAGES.tab='a'"></button><button class="tab" onclick="REG.render()"></button>"#;
    let rerender = concat!(
        "REG.render = function () { const P = window.PAGES; P.tab = P.tab || 'profile'; ",
        "setTimeout(function () { setTimeout(function () { P.repoTab = 'f'; }, 1); }, 1); ",
        "document.body.innerHTML = ''; };\n",
    );
    let repo = FakeRepo::with_openers("mock-probe-render-tab", r#"<div class="app"></div>"#, PAGE_MAP, &[], rerender);
    let e = format!("{:#}", probe_listed(repo.path(), html, &["repoTab"]).expect_err("a re-render assigns"));
    let want = concat!(
        "mockup onclick failed:\n",
        "  index: a render assigns PAGES.tab; tab ownership is unattributable\n",
        "  index: a render assigns PAGES.repoTab; tab ownership is unattributable\n",
        r#"  onclick "REG.render()" drawn on index is a .tab that assigns no PAGES.tab and no NON_TAB_KEYS key"#,
    );
    assert_eq!(e, want);
    let mounted = "const r0 = PAGES.index.render; PAGES.index.render = function () { const P = window.PAGES; P.tab = 'x'; return r0.apply(this, arguments); };\n";
    let repo = FakeRepo::with_openers("mock-probe-mount-tab", r#"<div class="app"></div>"#, PAGE_MAP, &[], mounted);
    let e = format!("{:#}", probe_html(repo.path(), r#"<button class="tab on" onclick="PAGES.tab='a'"></button>"#).expect_err("the mount assigns"));
    assert_eq!(e, "mockup onclick failed:\n  index: a render assigns PAGES.tab; tab ownership is unattributable");
    let alias = "const P = window.PAGES; PAGES.viaAlias = function () { P.repoTab = 'f'; };\n";
    let clean = FakeRepo::with_openers("mock-probe-render-clean", r#"<div class="app"></div>"#, PAGE_MAP, &[], alias);
    let twin = r#"<button class="tab on" onclick="const P = window.PAGES; P.tab='a'; REG.render()"></button><button class="tab" onclick="PAGES.viaAlias();REG.render()"></button>"#;
    let probe = probe_listed(clean.path(), twin, &["repoTab"]).expect("only the bodies assign");
    assert_eq!(probe.tabs.tabs, ["a"]);
    assert_eq!(probe.non_tab_keys, ["repoTab"]);
}

/// A menu a body opens through a re-render (`REG.render` opens it), a timer, or a guard the stubs
/// make true (`closest` finds a stub) is recorded, so each, undeclared, is menu drift.
#[test]
fn menus_opened_through_a_re_render_a_timer_or_a_guard_are_recorded() {
    let script = "REG.render = function () { UI.openMenu({ stopPropagation: function () {} }, 'm1', 'a'); document.body.innerHTML = ''; };\n";
    let repo = FakeRepo::with_openers("mock-probe-paths", r#"<div class="app"></div>"#, PAGE_MAP, &[], script);
    let probe = probe_html(repo.path(), concat!(
        r#"<button onclick="REG.render()"></button>"#,
        r#"<button onclick="setTimeout(function () { UI.openMenu(event,'m2','b') }, 50)"></button>"#,
        r#"<button onclick="if (this.closest('.row')) UI.openMenu(event,'m3','c')"></button>"#,
    ))
    .expect("probes");
    assert_eq!(probe.menu_ids(), ["m1", "m2", "m3"]);
    let e = menu_drift(repo.path(), &probe);
    for id in ["m1", "m2", "m3"] {
        assert!(e.contains(&format!("mockup menu index.menu.{id} is not a PAGE_MAP menu entry")), "{id}: {e}");
    }
}

/// A menu opened through an alias of `UI.openMenu` taken as the scripts load, as `bundle.js`
/// aliases it as `MenuAction`, is recorded: the harness wraps `openMenu` as `UI` is set, before
/// any script can take the alias, so the menu, undeclared, is menu drift.
#[test]
fn a_menu_opened_through_an_alias_is_recorded() {
    let script = "window.Swarog = Object.assign(window.UI, { MenuAction: UI.openMenu });\n";
    let repo = FakeRepo::with_openers("mock-probe-alias", r#"<div class="app"></div>"#, PAGE_MAP, &[], script);
    let probe = probe_html(repo.path(), r#"<button onclick="UI.MenuAction(event,'m4','a')"></button>"#).expect("probes");
    assert_eq!(probe.menus, [("m4".to_string(), Some("a".to_string()))]);
    let e = menu_drift(repo.path(), &probe);
    assert!(e.contains("mockup menu index.menu.m4 is not a PAGE_MAP menu entry"), "{e}");
}

/// A body that appends a `.menu` outside every `UI.openMenu` call, which no call recorded, fails
/// naming its host and its text; one appended inside a call is the clean twin.
#[test]
fn a_menu_appended_outside_every_open_menu_call_is_an_error() {
    let repo = FakeRepo::new("mock-probe-loose", r#"<div class="app"></div>"#);
    let body = "const el = document.createElement('div'); el.className = 'menu'; document.body.appendChild(el)";
    let e = format!("{:#}", probe_html(repo.path(), &format!(r#"<button onclick="{body}"></button>"#)).expect_err("a loose .menu"));
    assert!(e.contains(&format!("onclick {body:?} drawn on index appended a .menu outside every UI.openMenu call")), "{e}");
    probe_html(repo.path(), r#"<button onclick="UI.openMenu(event,'user','a')"></button>"#).expect("a recorded menu");
}

/// A mockup whose `UI.openMenu` is not the harness's recording wrapper, missing or replaced after
/// `UI` was set, cannot be probed, so no menu it draws goes unrecorded.
#[test]
fn probing_a_mockup_whose_open_menu_is_not_recorded_is_an_error() {
    let cases = [
        ("mock-probe-no-open-menu", "UI = { state: {}, defineMenu: function () {} };\n"),
        ("mock-probe-replaced-open-menu", "UI.openMenu = function () {};\n"),
    ];
    for (name, script) in cases {
        let repo = FakeRepo::with_openers(name, r#"<div class="app"></div>"#, PAGE_MAP, &[], script);
        let e = format!("{:#}", probe_html(repo.path(), r#"<button onclick="PAGES.tab='a'"></button>"#).expect_err("not recorded"));
        assert!(e.contains("the mockup's UI.openMenu is not the harness's recording wrapper"), "{name}: {e}");
    }
}

/// The real mockup's user menu, opened through the harness: a `.menu` whose groups are the
/// registry's `entity.user.row_action` modules, with nothing of the page around it.
#[test]
fn the_harness_opens_the_real_user_menu() {
    let out = super::render_menu(&real_repo(), "user", Some("aiko.tanaka")).expect("opens");
    assert!(out.starts_with(r#"<div class="menu">"#) && out.ends_with("</div>"), "{out}");
    assert!(out.contains(r#"class="menu-group""#) && out.contains("Suspend account"), "{out}");
    assert!(!out.contains(r#"class="app""#), "only the menu: {out}");
    let e = format!("{:#}", super::render_menu(&real_repo(), "nosuchmenu", Some("x")).expect_err("no such menu"));
    assert!(e.contains("node failed rendering menu nosuchmenu"), "{e}");
}

/// The real mockup's registry, read through the harness: every `UI.defineMenu` id, whether
/// or not a compared page draws it.
#[test]
fn the_harness_records_every_real_registry_menu() {
    let mut ids = super::defined_menus(&real_repo()).expect("reads the registry");
    ids.sort();
    assert_eq!(ids, ["apikey", "lock", "repo", "user"]);
}

/// The harness wraps `defineMenu` when a script assigns `UI`, so a menu defined through a
/// destructured `defineMenu` is recorded, once; a registry that defines none is an error.
#[test]
fn the_harness_records_menus_defined_before_any_page_mounts() {
    let repo = FakeRepo::new("mock-registry", r#"<div class="app"></div>"#);
    let registry = concat!(
        "window.UI = { defineMenu: function () {} };\n",
        "const { defineMenu } = UI;\n",
        "defineMenu(\"alpha\", () => []);\n",
        "UI.defineMenu(\"beta\", () => []);\n",
        "UI.defineMenu(\"alpha\", () => []);\n",
    );
    write(&repo.path().join("UILayout/redesign/assets/registry.js"), registry);
    assert_eq!(super::defined_menus(repo.path()).expect("reads"), ["alpha", "beta"]);
    write(&repo.path().join("UILayout/redesign/assets/registry.js"), "window.UI = { defineMenu: function () {} };\n");
    let e = format!("{:#}", super::defined_menus(repo.path()).expect_err("no menu"));
    assert_eq!(e, "the mockup defines no menu: the harness recorded no UI.defineMenu call");
}

/// The real mockup's poison skip, opened through the harness with no page: the `.scrim` its
/// `UI.confirm` appends, the `.modal` inside it, and nothing of the page around it.
#[test]
fn the_harness_opens_the_real_skip_poison_modal() {
    let out = super::render_modal(&real_repo(), "skipPoison", &[]).expect("opens");
    assert!(out.starts_with(r#"<div class="scrim"><div class="modal "#) && out.ends_with("</div></div>"), "{out}");
    assert!(out.contains("Skip event #12,905?") && out.contains(r#"class="frag-label""#), "{out}");
    assert!(!out.contains(r#"class="app""#), "only the modal: {out}");
}

/// The arguments reach the opener: `cancelMail` looks its message up in `D.mail` by the id it
/// is given, so the recipient of `m-88213` is drawn only when the id arrives.
#[test]
fn the_harness_passes_the_entry_arguments_to_the_real_opener() {
    let out = super::render_modal(&real_repo(), "cancelMail", &["m-88213"]).expect("opens");
    assert!(out.contains("Cancel m-88213?") && out.contains("MiraVoss"), "{out}");
    let e = format!("{:#}", super::render_modal(&real_repo(), "cancelMail", &[]).expect_err("no message id"));
    assert!(e.contains("node failed rendering PAGES.cancelMail"), "{e}");
}

/// An opener the mockup does not define, and a function that appends no `.scrim` (`P.actor`
/// returns a cell's html), each fail beside the clean case.
#[test]
fn a_missing_opener_or_one_that_appends_no_scrim_is_an_error() {
    super::render_modal(&real_repo(), "skipPoison", &[]).expect("the clean case");
    let e = format!("{:#}", super::render_modal(&real_repo(), "noSuchOpener", &[]).expect_err("not defined"));
    assert!(e.contains("node failed rendering PAGES.noSuchOpener") && e.contains("PAGES.noSuchOpener is not a function"), "{e}");
    let e = format!("{:#}", super::render_modal(&real_repo(), "actor", &["x"]).expect_err("no .scrim"));
    assert!(e.contains("PAGES.actor() appended no .scrim element"), "{e}");
}

/// The stand-in opens every `PAGE_MAP` modal entry's opener with its arguments.
#[test]
fn the_fake_repo_defines_every_page_map_opener() {
    let repo = FakeRepo::new("mock-modal", r#"<div class="app"></div>"#);
    let modals: Vec<_> = PAGE_MAP.iter().filter_map(|p| p.modal).collect();
    assert!(!modals.is_empty(), "PAGE_MAP names a modal");
    for m in modals {
        let out = super::render_modal(repo.path(), m.opener, m.args).expect("opens");
        let want = MOCK_MODAL.replace("{opener}", m.opener).replace("{args}", &m.args.join(","));
        assert_eq!(out, format!(r#"<div class="scrim">{want}</div>"#));
    }
}

/// The real player page, probed as its untabbed page map entry renders it: its tab bar's tabs
/// with profile selected, and the user menu its header opens on the profile's handle.
#[test]
fn the_probe_reads_the_real_player_tabs_and_menu() {
    let page = PAGE_MAP.iter().find(|p| p.page == "player" && p.is_page() && p.tab.is_none()).expect("PAGE_MAP has the player page");
    let html = render(&real_repo(), "player", &page.mock_search(), page.force).expect("renders");
    let doc = crate::html::parse(&html).expect("parses");
    let probe = super::probe(&real_repo(), "player", "player", page, &doc, super::Onclicks::All, &[]).expect("probes");
    assert_eq!(probe.tabs.selected.as_deref(), Some("profile"));
    assert!(probe.tabs.tabs.len() > 1 && probe.tabs.tabs.iter().any(|t| t == "wallet"), "{:?}", probe.tabs);
    assert_eq!(probe.menu_arg("user").expect("the header menu"), Some("aiko.tanaka"));
}

/// The real player profile draws its Personal data panel denied as designed; forced ok it
/// draws no denied state, so the force reaches the render.
#[test]
fn the_harness_forces_the_ok_state_on_the_real_mockup() {
    let designed = render(&real_repo(), "player", "?u=aiko.tanaka", false).expect("renders");
    assert!(designed.contains("Permission denied"), "the designed profile draws a denied panel");
    let ok = render(&real_repo(), "player", "?u=aiko.tanaka", true).expect("renders forced ok");
    assert!(ok.contains("Personal data") && !ok.contains("Permission denied"), "forced ok draws no denied panel");
}

/// A mockup with no `UI.state` cannot be forced; its unforced render is the clean twin.
#[test]
fn forcing_a_mockup_without_ui_state_is_an_error() {
    let repo = FakeRepo::new("mock-force", "");
    let registry = concat!(
        "var REG = { mount: function (meta, fn) { document.body.innerHTML = fn(); } };\n",
        "var UI = {};\n",
        "var PAGES = { index: { meta: {}, render: function () { return '<div class=\"app\"></div>'; } } };\n",
    );
    write(&repo.path().join("UILayout/redesign/assets/registry.js"), registry);
    render(repo.path(), "index", "", false).expect("the unforced render");
    let e = format!("{:#}", render(repo.path(), "index", "", true).expect_err("no UI.state"));
    assert!(e.contains("node failed rendering P.index forced ok") && e.contains("the mockup has no UI.state to force ok on"), "{e}");
}

/// The onclick probe runs each body on the page mounted as the render that drew it: forced ok,
/// a body whose opener appends its `.scrim` only in the ok state is a modal of that opener;
/// unforced, the same body appends nothing.
#[test]
fn the_onclick_probe_mounts_the_page_forced_ok_when_its_render_forces() {
    let script = concat!(
        "PAGES.okOnly = function () {\n",
        "  if (UI.state.force !== \"ok\") return;\n",
        "  const el = document.createElement(\"div\");\n",
        "  el.className = \"scrim\";\n",
        "  document.body.appendChild(el);\n",
        "};\n",
    );
    let repo = FakeRepo::with_openers("mock-probe-forced", r#"<div class="app"></div>"#, PAGE_MAP, &[], script);
    let doc = crate::html::parse(r#"<button onclick="PAGES.okOnly()"></button>"#).expect("parses");
    let forced = Page { force: true, ..*index_page() };
    let forced = super::probe(repo.path(), "index", "index", &forced, &doc, super::Onclicks::All, &[]).expect("the forced probe").modals;
    assert_eq!(forced, [MockOpener::Pages("okOnly".to_string())]);
    let designed = Page { force: false, ..*index_page() };
    let designed = super::probe(repo.path(), "index", "index", &designed, &doc, super::Onclicks::All, &[]).expect("the unforced probe").modals;
    assert!(designed.is_empty(), "unforced, the opener appends no .scrim: {designed:?}");
}

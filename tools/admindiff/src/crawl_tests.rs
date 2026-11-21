//! `crawl` against a loopback fake portal: no Postgres, no fleet. Each failing site differs
//! from [`Site::healthy`], which crawls clean, by the one defect it names. The healthy site
//! is built from [`PAGE_MAP`], [`EXCLUDED_MENUS`] and [`EXCLUDED_MODALS`], so a page or exclusion
//! added there is served too and every failing site still has only its own defect.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::{run_with, Config, CLEAN_MARKER, MAX_ROUNDS, PANEL_FAIL, PANEL_PASS};
use crate::mock::mock_tests::FakeRepo;
use crate::pages::{
    excluded_menu_id, with_tab, Lists, MockModalExclusion, MockOpener, Modal, ModalWhy, Page, PortalModal, Route, EXCLUDED_MENUS, EXCLUDED_MODALS,
    EXCLUDED_MOCK_MODALS, LISTS, PAGE_MAP,
};
use crate::tests::{body, mock_html, ScratchDir};

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    hx: bool,
}

#[derive(Clone)]
struct Response {
    status: u16,
    location: Option<&'static str>,
    body: String,
}

fn ok(body: impl Into<String>) -> Response {
    Response { status: 200, location: None, body: body.into() }
}

fn status(code: u16) -> Response {
    Response { status: code, location: None, body: String::new() }
}

/// The fake portal: login, exact routes, and an optional chain of fragments that each hold
/// the next pending slot.
#[derive(Clone)]
struct Site {
    login: Response,
    routes: BTreeMap<String, Response>,
    /// `/admin/_panel/chain/<n>` holds a slot for `<n + 1>` until `n` reaches this depth.
    chain_depth: usize,
}

/// The dashboard: where login lands, so the healthy site serves it whatever the page map says.
const DASHBOARD: &str = "/admin";
/// A page only the nav links.
const EXTRA_ROUTE: &str = "/admin/p/extra/things";
const EXTRA_SLOT: &str = "/admin/_panel/extra";

/// A page the crawl fetches, with the one slot the healthy site gives it.
struct Target {
    page: &'static str,
    route: &'static str,
    slot: String,
}

fn panel_url(page: &str) -> String {
    format!("/admin/_panel/page-{page}")
}

/// Every mapped page of [`PAGE_MAP`].
fn mapped() -> Vec<Target> {
    mapped_in(PAGE_MAP)
}

/// Every mapped page of `page_map`.
fn mapped_in(page_map: &[Page]) -> Vec<Target> {
    page_map
        .iter()
        .filter(|p| p.is_page())
        .filter_map(|p| match p.route {
            Route::Mapped(route) => Some(Target { page: p.page, route, slot: panel_url(p.page) }),
            Route::FirstRow(_) | Route::Pending => None,
        })
        .collect()
}

/// Every compared `FirstRow` page or tab entry of `page_map` ([`Page::is_compared_host`]) with
/// the list page it follows.
fn followers_in(page_map: &[Page]) -> Vec<(Page, &'static str)> {
    page_map
        .iter()
        .filter(|p| p.is_compared_host())
        .filter_map(|p| match p.route {
            Route::FirstRow(list) => Some((*p, list)),
            Route::Mapped(_) | Route::Pending => None,
        })
        .collect()
}

/// [`followers_in`] of [`PAGE_MAP`], each by its target id.
fn followers() -> Vec<(String, &'static str)> {
    followers_in(PAGE_MAP).into_iter().map(|(p, list)| (p.id(), list)).collect()
}

/// The route the healthy site serves follower `entry` at: its entity route, with `?tab=` for a
/// tab entry, as the crawl follows it.
fn entry_route(entry: &Page) -> String {
    let route = follower_route(entry.page);
    entry.tab.map_or_else(|| route.clone(), |tab| with_tab(&route, tab))
}

/// Every menu target of `page_map` on a compared page or tab ([`Page::is_compared_target`]):
/// its target id, host id and kind.
fn menus_in(page_map: &[Page]) -> Vec<(String, String, &'static str)> {
    page_map.iter().filter(|p| p.is_menu() && p.is_compared_target()).filter_map(|p| p.menu.map(|m| (p.id(), p.host().id(), m.kind))).collect()
}

/// [`menus_in`] of [`PAGE_MAP`].
fn menus() -> Vec<(String, String, &'static str)> {
    menus_in(PAGE_MAP)
}

/// [`MENU_BODY`] drawing only the modal openers `host`, a menu target's id or an excluded
/// menu's ([`excluded_menu_id`]), declares ([`declared_menu`]).
fn menu_body(host: &str) -> String {
    declared_menu(MENU_BODY, host)
}

/// [`menu_body`] against `page_map` instead of [`PAGE_MAP`].
fn menu_body_in(page_map: &[Page], host: &str) -> String {
    declared_menu_in(page_map, MENU_BODY, host)
}

/// The menu golden `golden` drawing only the modal openers `host` declares: the golden's own
/// `hx-get` openers are drawn as `hx-post` (`Invoke`) buttons, and one opener is added for
/// each `PAGE_MAP` modal entry and [`EXCLUDED_MODALS`] entry `host` hosts, at the end of the
/// `.menu` root, after the not implemented item of each [`EXCLUDED_MOCK_MODALS`] `Disabled`
/// claim of `host` ([`disabled_item`]). A host no entry names, a status-only menu's, gets no opener at all.
fn declared_menu(golden: &str, host: &str) -> String {
    declared_menu_in(PAGE_MAP, golden, host)
}

/// [`declared_menu`] against `page_map` instead of [`PAGE_MAP`].
fn declared_menu_in(page_map: &[Page], golden: &str, host: &str) -> String {
    let body = golden.replace(r#"hx-get="&#x2f;admin&#x2f;_action"#, r#"hx-post="&#x2f;admin&#x2f;_action"#);
    let mapped = page_map.iter().filter(|p| p.host().id() == host).filter_map(|p| p.modal.map(|m| m.action_path()));
    let excluded = EXCLUDED_MODALS.iter().filter(|(h, _, _)| *h == host).map(|(_, action, _)| action.to_string());
    let openers: String = mapped.chain(excluded).map(|a| modal_button(&modal_url(&a, host))).collect();
    let disabled: String = disabled_actions(host).map(|a| disabled_item(a, "not-implemented")).collect();
    in_menu(&body, &format!("{disabled}{openers}"))
}

/// The action of each production [`EXCLUDED_MOCK_MODALS`] `Disabled` claim of `host`.
fn disabled_actions(host: &str) -> impl Iterator<Item = &'static str> + '_ {
    EXCLUDED_MOCK_MODALS.iter().filter(move |x| x.host == host).filter_map(|x| match x.portal {
        PortalModal::Disabled(action) => Some(action),
        _ => None,
    })
}

/// Row menu action `action` (`<module>/<action>`) drawn disabled with refusal `kind`: the
/// portal's disabled `menu_item` in [`MENU_BODY`] (`economy/grant`, `denied`) refused as that.
fn disabled_item(action: &str, kind: &str) -> String {
    refused_as(golden_element(MENU_BODY, r#"data-refused-action="economy/grant""#), ("economy/grant", "denied"), action, kind)
}

/// Page head action `action` (`<module>/<action>`) drawn disabled with refusal `kind`: the
/// portal's disabled `action_button` in [`TOOL_BODY`] (`golden/pause`, `not-implemented`)
/// refused as that.
fn disabled_button(action: &str, kind: &str) -> String {
    let from = ("golden/pause", "not-implemented");
    refused_as(golden_element(TOOL_BODY, r#"data-refused-action="golden/pause""#), from, action, kind)
}

/// The disabled `control`, drawn refusing `from` (its action and refusal kind), refusing
/// `action` as `kind` instead.
fn refused_as(control: &str, from: (&str, &str), action: &str, kind: &str) -> String {
    let (was_action, was_kind) = (format!(r#"data-refused-action="{}""#, from.0), format!(r#"data-refusal="{}""#, from.1));
    assert!(control.contains(&was_action) && control.contains(&was_kind), "the control refuses {from:?}: {control}");
    control
        .replacen(&was_action, &format!(r#"data-refused-action="{action}""#), 1)
        .replacen(&was_kind, &format!(r#"data-refusal="{kind}""#), 1)
}

/// The element of `html` whose opening tag holds `marker`, the first if several do, to its
/// matching close tag.
fn golden_element<'a>(html: &'a str, marker: &str) -> &'a str {
    let at = html.find(marker).unwrap_or_else(|| panic!("the golden draws {marker}"));
    let start = html[..=at].rfind('<').expect("the marker is inside a tag");
    let tag: String = html[start + 1..].chars().take_while(char::is_ascii_alphanumeric).collect();
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let opens_here = |i: usize| html[i + open.len()..].starts_with(|c: char| c == '>' || c.is_whitespace());
    let (mut depth, mut from) = (0usize, start);
    loop {
        let next_close = from + html[from..].find(&close).unwrap_or_else(|| panic!("<{tag}> closes"));
        let next_open = html[from..next_close].match_indices(&open).map(|(i, _)| from + i).find(|i| opens_here(*i));
        match next_open {
            Some(o) => {
                depth += 1;
                from = o + open.len();
            }
            None => {
                depth -= 1;
                from = next_close + close.len();
                if depth == 0 {
                    return &html[start..from];
                }
            }
        }
    }
}

/// `html` with the content of the element `golden_element` finds by `marker` replaced by
/// `inner`.
fn with_inner(html: &str, marker: &str, inner: &str) -> String {
    let element = golden_element(html, marker);
    let open = &element[..=element.find('>').expect("the element opens")];
    let close = &element[element.rfind("</").expect("the element closes")..];
    html.replacen(element, &format!("{open}{inner}{close}"), 1)
}

/// The menu `body` with `extra` added at the end of its `.menu` root.
fn in_menu(body: &str, extra: &str) -> String {
    let end = body.rfind("</div>").expect("a menu body closes its .menu root");
    format!("{}{extra}{}", &body[..end], &body[end..])
}

/// The menu the healthy site's page `page` opens for its `kind` menu entry.
fn menu_url(kind: &str, page: &str) -> String {
    format!("/admin/_menu/{kind}/{page}")
}

/// The modal body the healthy site serves at every [`modal_url`]: the portal's own confirm
/// modal golden.
const MODAL_BODY: &str = include_str!("../../../core/admin/src/testdata/modal_confirm_danger_impact_reason.html");

/// The opener URL the healthy site's page `page` draws for modal action `action`
/// (`<module>/<action>`): the item is the page, as a fleet's item ids are its own.
fn modal_url(action: &str, page: &str) -> String {
    format!("/admin/_action/{action}?item={page}")
}

/// The modal actions the portal draws as a page head action (`ActionPoint::PageHead`), in the
/// shell's `.page-actions`: mail's test send (`modules/mail/src/admin.rs`). Every other modal
/// action is drawn in a panel, on a queue row or as a panel tool.
const HEAD_ACTIONS: &[&str] = &["mail/test-send"];

/// The modal openers page or tab `page` draws, as the portal's `hx_action` writes them: one for
/// each `PAGE_MAP` modal entry and each [`EXCLUDED_MODALS`] entry it hosts whose action is a
/// page head action ([`HEAD_ACTIONS`]) when `head` holds, a panel's otherwise; the head draws
/// the not implemented button of each [`EXCLUDED_MOCK_MODALS`] `Disabled` claim of `page` first
/// ([`disabled_button`]).
fn modal_buttons(page: &str, head: bool) -> String {
    modal_buttons_in(PAGE_MAP, page, head)
}

/// [`modal_buttons`] against `page_map` instead of [`PAGE_MAP`].
fn modal_buttons_in(page_map: &[Page], page: &str, head: bool) -> String {
    let mapped = page_map.iter().filter(|p| p.host().id() == page).filter_map(|p| p.modal.map(|m| m.action_path()));
    let excluded = EXCLUDED_MODALS.iter().filter(|(host, _, _)| *host == page).map(|(_, action, _)| action.to_string());
    let openers: String =
        mapped.chain(excluded).filter(|a| HEAD_ACTIONS.contains(&a.as_str()) == head).map(|a| modal_button(&modal_url(&a, page))).collect();
    let disabled: String =
        if head { disabled_actions(page).map(|a| disabled_button(a, "not-implemented")).collect() } else { String::new() };
    format!("{disabled}{openers}")
}

/// A modal opener GETting `url`, as the portal's `hx_action` writes a `Form` or `Confirm` button.
fn modal_button(url: &str) -> String {
    format!(r##"<button type="button" class="btn sm" hx-get="{}" hx-target="#modal-root" hx-swap="innerHTML"></button>"##, escaped(url))
}

/// The portal's answer to an action that needs no modal: its result toast golden.
const MODAL_TOAST: &str = include_str!("../../../core/admin/src/testdata/modal_result_toast.html");

/// A panel golden whose head draws a not implemented tool disabled: the source of
/// [`disabled_button`].
const TOOL_BODY: &str = include_str!("../../../core/admin/src/testdata/queue_panel_tool.html");

/// The menu body the healthy site serves at every [`menu_url`]: the portal's own row menu
/// golden, each module up.
const MENU_BODY: &str = include_str!("../../../core/admin/src/testdata/menu_row_actions.html");

/// The same row menu golden with the economy module down.
const MENU_DOWN: &str = include_str!("../../../core/admin/src/testdata/menu_row_actions_down.html");

/// The same row menu golden while a manifest source has never answered.
const MENU_INCOMPLETE: &str = include_str!("../../../core/admin/src/testdata/menu_row_actions_incomplete.html");

/// The portal's whole page golden, document head, popovers and palette included: every page of
/// the healthy site is built from it ([`shell`]).
const SHELL_PAGE: &str = include_str!("../../../core/admin/src/testdata/screen_shell_page.html");

/// A dashboard golden while a manifest source has never answered: its `.app` root is the
/// error-state root.
const PAGE_INCOMPLETE: &str = include_str!("../../../core/admin/src/testdata/screen_dashboard_incomplete.html");

/// The opening tag of the root element `golden` starts with.
fn root_tag(golden: &str) -> &str {
    &golden[..=golden.find('>').expect("the golden opens an element")]
}

/// A menu button opening `url`, as the portal's template writes it.
fn menu_button(url: &str) -> String {
    format!(r#"<button class="btn sm icon ghost" data-menu-url="{}"></button>"#, escaped(url))
}

/// The buttons page or tab `page` draws for its menu entries: in each table row on a list page
/// ([`rows`]), as the portal draws a row menu, else in the page.
fn menu_buttons(page: &str) -> String {
    menu_buttons_in(PAGE_MAP, page)
}

/// [`menu_buttons`] against `page_map` instead of [`PAGE_MAP`].
fn menu_buttons_in(page_map: &[Page], page: &str) -> String {
    menus_in(page_map).iter().filter(|(_, host, _)| host == page).map(|(_, _, kind)| menu_button(&menu_url(kind, page))).collect()
}

/// The buttons page `page` draws for its [`EXCLUDED_MENUS`] kinds.
fn excluded_buttons(page: &str) -> String {
    EXCLUDED_MENUS.iter().filter(|(host, _, _)| *host == page).map(|(_, kind, _)| menu_button(&menu_url(kind, page))).collect()
}

/// The entity route the healthy site's list page links first for follower `page`.
fn follower_route(page: &str) -> String {
    format!("/admin/e/{page}/x")
}

/// `route` as the portal's template writes it in an attribute, every `/` escaped.
fn escaped(route: &str) -> String {
    route.replace('/', "&#x2f;")
}

/// A table whose first `tr.clickable` opens `first`: a plain row with a `data-href` before
/// it, and a second clickable row after it; each clickable row draws `menu` in its last cell,
/// as the portal's `table.rs` row menu does.
fn rows(first: &str, menu: &str) -> String {
    format!(
        concat!(
            r#"<table class="tbl"><tbody><tr data-href="{plain}"><td>header</td></tr>"#,
            r#"<tr class="clickable" data-href="{first}"><td>first</td><td class="act">{menu}</td></tr>"#,
            r#"<tr class="clickable" data-href="{second}"><td>second</td><td class="act">{menu}</td></tr></tbody></table>"#,
        ),
        plain = escaped("/admin/e/plain/row"),
        first = escaped(first),
        second = escaped("/admin/e/second/row"),
        menu = menu,
    )
}

/// The dashboard's slot: the mapped page's at [`DASHBOARD`], else one of its own.
fn dashboard_slot() -> String {
    mapped().into_iter().find(|t| t.route == DASHBOARD).map_or_else(|| panel_url("dashboard"), |t| t.slot)
}

/// A mapped page other than the dashboard, which the page and fragment defects are put on.
fn other_mapped() -> Target {
    mapped().into_iter().find(|t| t.route != DASHBOARD).expect("PAGE_MAP maps a page besides the dashboard")
}

/// A fragment for [`other_mapped`]'s slot in `state` holding `extra`, with the modal openers
/// its healthy fragment draws, so a fragment defect put on it drops none of them.
fn other_fragment(state: Option<&str>, extra: &str) -> String {
    fragment(state, "Subscriptions", &format!("{extra}{}", modal_buttons(other_mapped().page, false)))
}

fn slot(url: &str) -> String {
    format!(r#"<div class="slot" hx-get="{url}" hx-trigger="load" hx-swap="outerHTML"><div class="sk"></div></div>"#)
}

fn fragment(state: Option<&str>, title: &str, extra: &str) -> String {
    let state = state.map(|s| format!(r#" data-state="{s}""#)).unwrap_or_default();
    format!(
        r#"<div class="slot" data-panel-url="/x"{state}><section class="panel"><span class="panel-title">{title}</span>{extra}</section></div>"#
    )
}

fn nav(links: &[&str]) -> String {
    links.iter().map(|href| format!(r#"<a class="nav-item" href="{href}">n</a>"#)).collect()
}

/// The shell's search palette in [`SHELL_PAGE`]: the one hidden `.scrim[data-palette]`, its
/// input and its list.
fn palette_scrim() -> &'static str {
    golden_element(SHELL_PAGE, r#"class="scrim" data-palette hidden"#)
}

/// The shell's palette opener in [`SHELL_PAGE`]'s topbar.
fn palette_opener() -> &'static str {
    golden_element(SHELL_PAGE, r#"data-action="palette""#)
}

/// A portal page: [`SHELL_PAGE`] with `nav_links` in its nav, `h1` as its title, `head` in its
/// `.page-actions` and `content` after its page head.
fn shell(nav_links: &str, h1: &str, head: &str, content: &str) -> String {
    let page = with_inner(SHELL_PAGE, r#"<nav class="nav">"#, nav_links);
    let page = with_inner(&page, "<h1>", h1);
    let page = with_inner(&page, r#"<div class="page-actions">"#, head);
    let page_head = golden_element(&page, r#"<div class="page-head">"#);
    page.replacen(page_head, &format!("{page_head}{content}"), 1)
}

impl Site {
    /// Clean: the dashboard, every mapped page of [`PAGE_MAP`] and one page only the nav
    /// links, each with one slot whose fragment is in the ok state; a list page's fragment
    /// lists rows whose first opens its follower's entity page, which has one slot too. A page
    /// hosting menu entries draws their buttons ([`menu_buttons`]), in its rows if it lists
    /// any, and a page an [`EXCLUDED_MENUS`] entry names draws its buttons
    /// ([`excluded_buttons`]); each menu answers [`MENU_BODY`]. A page's modal openers
    /// ([`modal_buttons`]) are drawn where the portal draws them: a page head action's in the
    /// shell's `.page-actions`, a queue row's or a panel tool's in its slot's fragment; each
    /// modal answers [`MODAL_BODY`].
    fn healthy() -> Site {
        Site::with_content(&slot)
    }

    /// The healthy site's pages and nav with each page's content `content(<its slot url>)`;
    /// every slot url answers its ok fragment.
    fn with_content(content: &dyn Fn(&str) -> String) -> Site {
        Site::with_map(PAGE_MAP, content)
    }

    /// [`Site::with_content`] serving `page_map` instead of [`PAGE_MAP`]: each compared page or
    /// tab ([`Page::is_compared_host`]) and menu target ([`Page::is_compared_target`]) of it is
    /// served with the menus and modals it hosts; a tab entry at its `?tab=` route, drawing its
    /// tab selected.
    fn with_map(page_map: &[Page], content: &dyn Fn(&str) -> String) -> Site {
        let menu_buttons = |page: &str| menu_buttons_in(page_map, page);
        let modal_buttons = |page: &str, head: bool| modal_buttons_in(page_map, page, head);
        let followers = followers_in(page_map);
        let mut pages: Vec<(&str, &str, String)> = mapped_in(page_map).into_iter().map(|t| (t.route, t.page, t.slot)).collect();
        if !pages.iter().any(|(route, _, _)| *route == DASHBOARD) {
            pages.push((DASHBOARD, "dashboard", dashboard_slot()));
        }
        pages.push((EXTRA_ROUTE, "things", EXTRA_SLOT.to_string()));
        let links = nav(&pages.iter().map(|(route, _, _)| *route).collect::<Vec<_>>());
        let mut routes = BTreeMap::new();
        for (route, page, slot_url) in &pages {
            let title = if *route == DASHBOARD { "Dashboard" } else { page };
            // One table per entity page that follows this list, whose tab entries follow the same row.
            let mut entities: Vec<&str> = Vec::new();
            for (entry, list) in &followers {
                if list == page && !entities.contains(&entry.page) {
                    entities.push(entry.page);
                }
            }
            let listed: String = entities.iter().map(|f| rows(&follower_route(f), &menu_buttons(page))).collect();
            let in_page = if listed.is_empty() { menu_buttons(page) } else { String::new() };
            let html = format!("{}{in_page}{}", content(slot_url), excluded_buttons(page));
            routes.insert(route.to_string(), ok(shell(&links, title, &modal_buttons(page, true), &html)));
            let fragment_html = format!("{listed}{}", modal_buttons(page, false));
            routes.insert(slot_url.clone(), ok(fragment(Some("ok"), &format!("{page} panel"), &fragment_html)));
        }
        for (entry, _) in &followers {
            let (page, route) = (entry.id(), entry_route(entry));
            let tab = match entry.tab {
                Some(tab) => format!(r#"<button class="tab on" data-href="{}">{tab}</button>"#, escaped(&route)),
                None => String::new(),
            };
            let slot_url = panel_url(&page);
            let html = format!("{tab}{}{}{}", content(&slot_url), menu_buttons(&page), excluded_buttons(&page));
            routes.insert(route, ok(shell(&links, &page, &modal_buttons(&page, true), &html)));
            routes.insert(slot_url, ok(fragment(Some("ok"), &format!("{page} panel"), &modal_buttons(&page, false))));
        }
        for (id, host, kind) in menus_in(page_map) {
            routes.insert(menu_url(kind, &host), ok(menu_body_in(page_map, &id)));
        }
        for (page, kind, _) in EXCLUDED_MENUS {
            routes.insert(menu_url(kind, page), ok(menu_body_in(page_map, &excluded_menu_id(page, kind))));
        }
        for (_, host, action) in modals_in(page_map) {
            routes.insert(modal_url(&action, &host), ok(MODAL_BODY));
        }
        for (page, action, _) in EXCLUDED_MODALS {
            routes.insert(modal_url(action, page), ok(MODAL_BODY));
        }
        Site { login: Response { status: 303, location: Some(DASHBOARD), body: String::new() }, routes, chain_depth: 0 }
    }

    fn with(mut self, path: &str, response: Response) -> Site {
        self.routes.insert(path.to_string(), response);
        self
    }

    fn answer(&self, req: &Request) -> Response {
        if req.method == "POST" && req.path == "/admin/login" {
            return self.login.clone();
        }
        if let Some(n) = req.path.strip_prefix("/admin/_panel/chain/").and_then(|n| n.parse::<usize>().ok()) {
            let next = if n < self.chain_depth { slot(&format!("/admin/_panel/chain/{}", n + 1)) } else { String::new() };
            return ok(fragment(Some("ok"), &format!("Chain {n}"), &next));
        }
        self.routes.get(&req.path).cloned().unwrap_or_else(|| status(404))
    }
}

/// A loopback HTTP/1.1 server answering one request per connection on its own threads.
struct Server {
    base_url: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    fn start(site: Site) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (site, log) = (Arc::new(site), Arc::clone(&seen));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (site, log) = (Arc::clone(&site), Arc::clone(&log));
                std::thread::spawn(move || serve(stream, &site, &log));
            }
        });
        Server { base_url, seen }
    }

    fn requests(&self) -> Vec<Request> {
        self.seen.lock().expect("log").clone()
    }
}

fn serve(stream: TcpStream, site: &Site, log: &Mutex<Vec<Request>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
    let (mut length, mut hx) = (0usize, false);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap_or((header.as_str(), ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "hx-request" => hx = value.trim() == "true",
            _ => {}
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let req = Request { method, path, hx };
    let resp = site.answer(&req);
    log.lock().expect("log").push(req);
    let location = resp.location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{location}Connection: close\r\n\r\n",
        resp.status,
        resp.body.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(resp.body.as_bytes());
    let _ = stream.flush();
}

struct Crawl {
    result: anyhow::Result<bool>,
    /// The failures the crawl reported; none unless `result` is `Ok(false)`.
    failures: Vec<String>,
    /// The targets the crawl reported EMPTY.
    empty: Vec<String>,
    /// The `EXCLUDED_MOCK_MODALS` claims the crawl printed UNCHECKED.
    unchecked: Vec<String>,
    out: ScratchDir,
    server: Server,
}

impl Crawl {
    fn clean_marker(&self) -> bool {
        self.out.path().join("run").join(CLEAN_MARKER).is_file()
    }

    /// Asserts the crawl finished with failures and wrote no CLEAN marker.
    fn assert_failed(&self, why: &str) {
        match &self.result {
            Ok(false) => {}
            other => panic!("{why}: expected Ok(false), got {other:?}"),
        }
        assert!(!self.clean_marker(), "{why}: a failed crawl wrote {CLEAN_MARKER}");
    }

    /// [`Crawl::assert_failed`], with a reported failure containing `text`.
    fn assert_failed_with(&self, why: &str, text: &str) {
        self.assert_failed(why);
        assert!(self.failures.iter().any(|f| f.contains(text)), "{why}: expected {text:?} in a failure: {:#?}", self.failures);
    }

    /// Asserts the crawl, run without `--allow-errors`, finished clean and wrote the CLEAN marker.
    fn assert_clean(&self, why: &str) {
        assert!(matches!(self.result, Ok(true)) && self.clean_marker(), "{why}: {:?}", self.result);
    }

    /// Asserts the crawl, run with `--allow-errors`, finished without failures and wrote no
    /// CLEAN marker: it is a preview.
    fn assert_preview(&self, why: &str) {
        assert!(matches!(self.result, Ok(true)), "{why}: {:?}", self.result);
        assert!(!self.clean_marker(), "{why}: a crawl with --allow-errors wrote {CLEAN_MARKER}");
    }

    /// Asserts the crawl stopped with an error containing `text` and wrote no CLEAN marker.
    fn assert_error(&self, text: &str) {
        match &self.result {
            Err(e) => assert!(format!("{e:#}").contains(text), "expected {text:?} in {e:#}"),
            other => panic!("expected an error containing {text:?}, got {other:?}"),
        }
        assert!(!self.clean_marker(), "an aborted crawl wrote {CLEAN_MARKER}");
    }
}

async fn crawl(site: Site, allow_errors: bool) -> Crawl {
    crawl_with(site, allow_errors, false).await
}

/// `page`'s html on `site` with `extra` added at the end of its main content.
fn add_to_page(site: &Site, route: &str, extra: &str) -> Response {
    let html = site.routes.get(route).expect("the site serves the page").body.clone();
    assert!(html.contains("</main>"), "the page has a main: {html}");
    ok(html.replacen("</main>", &format!("{extra}</main>"), 1))
}

async fn crawl_with(site: Site, allow_errors: bool, allow_empty: bool) -> Crawl {
    crawl_run(site, PAGE_MAP, EXCLUDED_MENUS, allow_errors, allow_empty).await
}

/// [`crawl`] of `page_map` instead of [`PAGE_MAP`].
async fn crawl_map(site: Site, page_map: &[Page]) -> Crawl {
    crawl_lists(site, page_map, EXCLUDED_MENUS, false).await
}

/// [`crawl_with`] of `page_map` and `excluded_menus` instead of the production lists.
async fn crawl_lists(site: Site, page_map: &[Page], excluded_menus: &[(&str, &str, &str)], allow_empty: bool) -> Crawl {
    crawl_run(site, page_map, excluded_menus, false, allow_empty).await
}

async fn crawl_run(
    site: Site,
    page_map: &[Page],
    excluded_menus: &[(&str, &str, &str)],
    allow_errors: bool,
    allow_empty: bool,
) -> Crawl {
    crawl_into(ScratchDir::new("crawl"), site, page_map, excluded_menus, EXCLUDED_MODALS, allow_errors, allow_empty).await
}

/// [`crawl_with`] of `page_map` and `excluded_modals` instead of the production lists.
async fn crawl_modals(site: Site, page_map: &[Page], excluded_modals: &[(&str, &str, ModalWhy<'_>)], allow_empty: bool) -> Crawl {
    crawl_into(ScratchDir::new("crawl"), site, page_map, EXCLUDED_MENUS, excluded_modals, false, allow_empty).await
}

/// [`crawl_run`] into `out`, which may already hold an earlier run.
async fn crawl_into(
    out: ScratchDir,
    site: Site,
    page_map: &[Page],
    excluded_menus: &[(&str, &str, &str)],
    excluded_modals: &[(&str, &str, ModalWhy<'_>)],
    allow_errors: bool,
    allow_empty: bool,
) -> Crawl {
    let claims = unmapped_claims(page_map);
    let lists = Lists { page_map, excluded_menus, excluded_modals, excluded_mock_modals: &claims, ..LISTS };
    crawl_lists_into(out, site, &lists, allow_errors, allow_empty).await
}

/// The production [`EXCLUDED_MOCK_MODALS`] less each entry whose opener a modal entry of
/// `page_map` opens on its host, as a map adding that modal entry drops the exclusion
/// (`validate_mock_modals` fails an opener listed in both).
fn unmapped_claims(page_map: &[Page]) -> Vec<MockModalExclusion<'static>> {
    let mapped = |x: &MockModalExclusion| {
        page_map.iter().any(|p| p.host().id() == x.host && p.modal.is_some_and(|m| MockOpener::Pages(m.opener) == x.opener))
    };
    EXCLUDED_MOCK_MODALS.iter().copied().filter(|x| !mapped(x)).collect()
}

/// [`crawl_into`] of `lists` as given.
async fn crawl_lists_into(out: ScratchDir, site: Site, lists: &Lists<'_>, allow_errors: bool, allow_empty: bool) -> Crawl {
    let server = Server::start(site);
    let cfg = Config {
        base_url: server.base_url.clone(),
        username: "admin".to_string(),
        password: "admin".to_string(),
        allow_errors,
        allow_empty,
        out: out.path().join("run"),
    };
    let (result, failures, empty, unchecked) = match run_with(&cfg, lists).await {
        Ok(o) => (Ok(o.failures.is_empty()), o.failures, o.empty, o.unchecked),
        Err(e) => (Err(e), Vec::new(), Vec::new(), Vec::new()),
    };
    Crawl { result, failures, empty, unchecked, out, server }
}

#[tokio::test]
async fn a_stale_clean_marker_in_a_reused_run_dir_is_dropped() {
    let out = ScratchDir::new("crawl");
    std::fs::create_dir_all(out.path().join("run")).expect("run dir");
    std::fs::write(out.path().join("run").join(CLEAN_MARKER), "").expect("stale marker");
    let c = crawl_into(out, Site::healthy(), PAGE_MAP, EXCLUDED_MENUS, EXCLUDED_MODALS, true, false).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
    assert!(!c.clean_marker(), "a crawl with --allow-errors kept a stale {CLEAN_MARKER}");
}

#[tokio::test]
async fn a_healthy_portal_crawls_clean_and_writes_the_assembled_pages() {
    let c = crawl(Site::healthy(), false).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
    assert!(c.clean_marker(), "a clean crawl writes {CLEAN_MARKER}");
    let run = c.out.path().join("run");
    let pages = mapped();
    assert!(pages.iter().any(|t| t.route == DASHBOARD), "the page map maps the dashboard");
    for t in &pages {
        let page = std::fs::read_to_string(run.join(format!("{}.html", t.page))).expect("each mapped page is written");
        let title = format!("{} panel", t.page);
        assert!(page.contains(r#"data-state="ok""#) && page.contains(&title), "the slot is replaced: {page}");
        assert!(!page.contains("hx-trigger"), "no loading placeholder is left: {page}");
    }
    assert!(run.join("p-extra-things.html").is_file(), "the page only the nav links");
    for (page, _) in followers() {
        let html = std::fs::read_to_string(run.join(format!("{page}.html"))).expect("each followed page is written");
        assert!(html.contains(&format!("{page} panel")), "the followed page's slot is replaced: {html}");
    }
    let panels: Vec<Request> = c.server.requests().into_iter().filter(|r| r.path.starts_with("/admin/_panel/")).collect();
    let want = pages.len() + followers().len() + 1;
    assert_eq!(panels.len(), want, "one fragment per mapped page, followed page and the nav-only page: {panels:?}");
    assert!(panels.iter().all(|r| r.hx), "fragments are fetched with HX-Request: {panels:?}");
}

#[tokio::test]
async fn a_login_answered_401_fails() {
    let site = Site { login: status(401), ..Site::healthy() };
    crawl(site, false).await.assert_error("POST /admin/login answered 401 Unauthorized");
}

#[tokio::test]
async fn a_login_page_served_with_200_fails() {
    let site = Site { login: ok(r#"<form class="login"><input name="password"></form>"#), ..Site::healthy() };
    crawl(site, false).await.assert_error("answered 200 OK (location \"\"); expected 303 to /admin");
}

#[tokio::test]
async fn a_303_anywhere_but_the_dashboard_fails() {
    let elsewhere = Response { status: 303, location: Some("/admin/login?error=1"), body: String::new() };
    let site = Site { login: elsewhere, ..Site::healthy() };
    crawl(site, false).await.assert_error("(location \"/admin/login?error=1\"); expected 303 to /admin");
}

#[tokio::test]
async fn a_dashboard_without_the_shell_fails() {
    let no_sidebar = Site::healthy().with(DASHBOARD, ok(r#"<div class="page-head"><h1>Dashboard</h1></div>"#));
    crawl(no_sidebar, false).await.assert_error("did not land on the dashboard shell (sidebar: false");
    let login_title = Site::healthy().with(DASHBOARD, ok(shell("", "Sign in", "", "")));
    crawl(login_title, false).await.assert_error("page title: Some(\"Sign in\")");
    let not_ok = Site::healthy().with(DASHBOARD, status(500));
    crawl(not_ok, false).await.assert_error("GET /admin answered 500");
}

#[tokio::test]
async fn zero_nav_pages_fails() {
    let site = Site::healthy().with(DASHBOARD, ok(shell(&nav(&[DASHBOARD]), "Dashboard", "", &slot(&dashboard_slot()))));
    crawl(site, false).await.assert_failed("only the dashboard link");
}

#[tokio::test]
async fn zero_fragments_fails() {
    let site = Site::with_content(&|_| "<p>no slot</p>".to_string());
    crawl(site, false).await.assert_failed("no slot anywhere");
}

#[tokio::test]
async fn a_non_2xx_fragment_fails() {
    let failed = Response { status: 500, ..ok(other_fragment(Some("ok"), "")) };
    let site = Site::healthy().with(&other_mapped().slot, failed);
    crawl(site, false).await.assert_failed("a 500 fragment");
}

#[tokio::test]
async fn a_fragment_without_data_state_fails() {
    let site = Site::healthy().with(&other_mapped().slot, ok(other_fragment(None, "")));
    crawl(site, false).await.assert_failed("no data-state");
}

/// A fragment of a failed provider call, `error` or `stale` (a cached body), fails the crawl
/// unless `--allow-errors`.
#[tokio::test]
async fn an_error_or_stale_fragment_fails_unless_errors_are_allowed() {
    for state in ["error", "stale"] {
        let site = Site::healthy().with(&other_mapped().slot, ok(other_fragment(Some(state), "")));
        crawl(site.clone(), false).await.assert_failed(&format!("a {state} fragment"));
        crawl(site, true).await.assert_preview(&format!("--allow-errors: a {state} fragment"));
    }
}

/// A crawl with `--allow-errors` of the healthy site, with no failure, writes the pages but no
/// CLEAN marker, so the diff refuses it; the same crawl without the flag writes it and the diff
/// gets past that check.
#[tokio::test]
async fn a_crawl_with_errors_allowed_is_never_marked_clean_and_the_diff_refuses_it() {
    let repo = FakeRepo::new("crawl-preview-diff", &mock_html(&body(true)));
    let marker = format!("has no {CLEAN_MARKER} marker");
    let preview = crawl(Site::healthy(), true).await;
    preview.assert_preview("--allow-errors: the healthy site");
    let run = preview.out.path().join("run");
    assert!(run.join("index.html").is_file(), "a preview still writes its pages");
    let e = format!("{:#}", crate::run_diff(repo.path(), &LISTS, "index", &run, false).expect_err("a preview"));
    assert!(e.contains(&marker), "{e}");

    let clean = crawl(Site::healthy(), false).await;
    clean.assert_clean("the healthy site");
    let run = clean.out.path().join("run");
    let e = crate::run_diff(repo.path(), &LISTS, "index", &run, false).err().map(|e| format!("{e:#}")).unwrap_or_default();
    assert!(!e.contains(&marker), "{e}");
}

/// The panel states of a provider that answered crawl clean beside the ok fragment.
#[tokio::test]
async fn an_empty_readonly_or_denied_fragment_crawls_clean() {
    for state in ["empty", "readonly", "denied"] {
        let site = Site::healthy().with(&other_mapped().slot, ok(other_fragment(Some(state), "")));
        crawl(site, false).await.assert_clean(&format!("a {state} fragment"));
    }
}

/// A fragment whose `data-state` is no panel state fails even with `--allow-errors` and
/// `--allow-empty`, under which the healthy site crawls without failures as a preview.
#[tokio::test]
async fn a_fragment_in_no_panel_state_fails_always() {
    let lax = |site: Site| crawl_run(site, PAGE_MAP, EXCLUDED_MENUS, true, true);
    lax(Site::healthy()).await.assert_preview("--allow-errors --allow-empty: the ok fragment");
    let site = Site::healthy().with(&other_mapped().slot, ok(other_fragment(Some("down"), "")));
    lax(site).await.assert_failed("--allow-errors --allow-empty: a fragment whose data-state is down");
}

/// [`PANEL_PASS`] and [`PANEL_FAIL`] split exactly the portal's panel states. admindiff takes no
/// workspace crate, so the states are read from `PanelState` and its `as_str` in the source.
#[test]
fn the_panel_states_are_the_portals() {
    const PANELS: &str = include_str!("../../../core/admin/src/engine/panels.rs");
    let variants = PANELS.split_once("enum PanelState {").expect("the portal declares PanelState").1;
    let variants = variants.split_once('}').expect("PanelState closes").0.split(',').filter(|v| !v.trim().is_empty()).count();
    let as_str = PANELS.split_once("fn as_str(self)").expect("PanelState::as_str").1;
    let mut states: Vec<&str> = as_str
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("PanelState::"))
        .map_while(|l| l.strip_prefix("PanelState::"))
        .filter_map(|arm| arm.split_once("=> \"").map(|(_, s)| s.trim_end_matches("\",")))
        .collect();
    assert_eq!(states.len(), variants, "as_str maps each of the {variants} PanelState variants: {states:?}");
    states.sort_unstable();
    let mut ours: Vec<&str> = PANEL_PASS.iter().chain(PANEL_FAIL).copied().collect();
    ours.sort_unstable();
    assert_eq!(ours, states, "PANEL_PASS and PANEL_FAIL hold each panel state once");
}

#[tokio::test]
async fn slots_load_for_at_most_max_rounds() {
    let first = slot("/admin/_panel/chain/1");
    let chained = |depth: usize| {
        let site = Site { chain_depth: depth, ..Site::healthy() };
        site.with(&other_mapped().slot, ok(other_fragment(Some("ok"), &first)))
    };
    // Round 1 loads the page's slot, whose fragment holds the chain's first link; each link
    // holds the next until `depth`, so a chain of `depth` links takes `depth + 1` rounds.
    let within = crawl(chained(MAX_ROUNDS - 1), false).await;
    assert!(matches!(within.result, Ok(true)), "{MAX_ROUNDS} rounds: {:?}", within.result);
    crawl(chained(MAX_ROUNDS), false).await.assert_failed("one round more than MAX_ROUNDS");
}

/// The `players.menu.user` target: the crawl fetches the first `player` menu the players
/// rows draw, with `HX-Request`, and writes the `.menu` it answers as `players.menu.user.html`;
/// a menu answering 500 and rows that draw no `player` menu each fail the crawl, the latter
/// with `--allow-empty` too, since players lists rows. An excluded kind's menu before it in
/// the row is fetched for its status but is not the target's menu.
#[tokio::test]
async fn a_menu_target_writes_its_menu_and_fails_unless_reached() {
    let (id, host, kind) = menus().into_iter().find(|(id, _, _)| id == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let page = host.as_str();
    let healthy = Site::healthy();
    let list_url = panel_url(page);
    let list = healthy.routes.get(&list_url).expect("the healthy site serves the players rows").body.clone();
    let button = menu_buttons(page);
    assert!(list.contains(&format!(r#"<td class="act">{button}</td>"#)), "the players rows draw the menu button: {list}");
    let guild = "/admin/_menu/guild/g-1";
    let site = healthy
        .clone()
        .with(&list_url, ok(list.replace(&button, &format!("{}{button}", menu_button(guild)))))
        .with(guild, ok(menu_body(&excluded_menu_id(page, "guild")).replace("players", "guild")));
    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push((page, "guild", "another kind"));

    let fine = crawl_lists(site.clone(), PAGE_MAP, &excluded, false).await;
    assert!(matches!(fine.result, Ok(true)), "{:?}", fine.result);
    let fetched: Vec<Request> = fine.server.requests().into_iter().filter(|r| r.path.starts_with("/admin/_menu/")).collect();
    let url = menu_url(kind, page);
    assert_eq!(fetched.iter().filter(|r| r.path == url && r.hx).count(), 1, "{fetched:?}");
    assert_eq!(fetched.iter().filter(|r| r.path == guild && r.hx).count(), 1, "the excluded kind once: {fetched:?}");
    let written = std::fs::read_to_string(fine.out.path().join("run").join(format!("{id}.html"))).expect("the menu is written");
    assert_eq!(written, menu_body(&id), "the menu is written as the portal answers it");

    crawl_lists(site.with(&url, status(500)), PAGE_MAP, &excluded, false).await.assert_failed("a 500 menu");
    let bare = healthy.with(&list_url, ok(list.replace(&button, "")));
    crawl(bare.clone(), false).await.assert_failed("players draws no player menu");
    crawl_with(bare, false, true).await.assert_failed("--allow-empty: players lists rows but draws no player menu");
}

/// A menu answers 200 while a module is down, so its group's `data-state` is the state: the
/// portal's down golden fails the crawl unless `--allow-errors`, on a menu target, an
/// excluded menu and a status-only menu alike, each beside its passing twin, the ok golden. A
/// group without `data-state` or with one neither `ok` nor `error`, and a menu with no group
/// at all, fail even with `--allow-errors` and `--allow-empty`.
#[tokio::test]
async fn an_error_state_menu_group_fails_unless_errors_are_allowed() {
    let (id, host, kind) = menus().into_iter().find(|(id, _, _)| id == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let page = host.as_str();
    let url = menu_url(kind, page);
    let down = declared_menu(MENU_DOWN, &id);
    assert!(down.contains(r#"data-state="error""#) && down.contains(r#"data-state="ok""#), "one group down: {down}");
    assert!(!MENU_BODY.contains(r#"data-state="error""#), "every group up: {MENU_BODY}");
    let group = r#"<div class="menu-group" data-state="ok">"#;
    let up = menu_body(&id);
    let stateless = up.replace(group, r#"<div class="menu-group">"#);
    let unknown = up.replace(group, r#"<div class="menu-group" data-state="down">"#);
    let groupless = up.replace(r#"class="menu-group""#, r#"class="menu-head""#);
    assert!(stateless != up && unknown != up && groupless != up, "MENU_BODY carries a stated group");
    let healthy = Site::healthy();
    crawl(healthy.clone(), false).await.assert_clean("the healthy menu target");
    crawl(healthy.clone().with(&url, ok(down.clone())), false).await.assert_failed("an error-state menu target");
    crawl(healthy.clone().with(&url, ok(down.clone())), true).await.assert_preview("--allow-errors: an error-state menu target");
    let lax = |site: Site| crawl_run(site, PAGE_MAP, EXCLUDED_MENUS, true, true);
    lax(healthy.clone()).await.assert_preview("--allow-errors --allow-empty: the healthy menu target");
    lax(healthy.clone().with(&url, ok(down.clone()))).await.assert_preview("--allow-errors --allow-empty: an error-state menu target");
    lax(healthy.clone().with(&url, ok(stateless))).await.assert_failed("a menu group without data-state");
    lax(healthy.clone().with(&url, ok(unknown))).await.assert_failed("a menu group whose data-state is down");
    lax(healthy.clone().with(&url, ok(groupless.clone()))).await.assert_failed("a menu with no group");

    let target = other_mapped();
    let guild = "/admin/_menu/guild/g-1";
    let drawn = healthy.clone().with(target.route, add_to_page(&healthy, target.route, &menu_button(guild)));
    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push((target.page, "guild", "why"));
    let excluded_run = |site: Site, allow_errors: bool| crawl_run(site, PAGE_MAP, &excluded, allow_errors, false);
    let guild_id = excluded_menu_id(target.page, "guild");
    let (guild_up, guild_down) = (declared_menu(MENU_BODY, &guild_id), declared_menu(MENU_DOWN, &guild_id));
    excluded_run(drawn.clone().with(guild, ok(guild_up.clone())), false).await.assert_clean("the healthy excluded menu");
    excluded_run(drawn.clone().with(guild, ok(guild_down.clone())), false).await.assert_failed("an error-state excluded menu");
    excluded_run(drawn.with(guild, ok(guild_down.clone())), true).await.assert_preview("--allow-errors: an error-state excluded menu");

    // A status-only menu's modals are fetched for their status; guild_up draws none.
    let nav_only = healthy.clone().with(EXTRA_ROUTE, add_to_page(&healthy, EXTRA_ROUTE, &menu_button(guild)));
    crawl(nav_only.clone().with(guild, ok(guild_up.clone())), false).await.assert_clean("the healthy status-only menu");
    crawl(nav_only.clone().with(guild, ok(guild_down.clone())), false).await.assert_failed("an error-state status-only menu");
    crawl(nav_only.clone().with(guild, ok(guild_down)), true).await.assert_preview("--allow-errors: an error-state status-only menu");
    lax(nav_only.clone().with(guild, ok(guild_up))).await.assert_preview("--allow-errors --allow-empty: the healthy status-only menu");
    lax(nav_only.with(guild, ok(groupless))).await.assert_failed("--allow-errors --allow-empty: a status-only menu with no group");
}

/// A menu answers 200 while a manifest source has never answered, so its `.menu` root's
/// `data-state` is the state: the portal's incomplete golden, which differs from the ok golden
/// only by that root, fails the crawl unless `--allow-errors`, each beside the ok golden under
/// the same flags. A root without `data-state` or with one neither `ok` nor `error`, and a
/// body whose first element is no `.menu` root (bare groups), fail even with `--allow-errors`
/// and `--allow-empty`.
#[tokio::test]
async fn an_error_state_menu_root_fails_unless_errors_are_allowed() {
    let (id, host, kind) = menus().into_iter().find(|(id, _, _)| id == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let page = host.as_str();
    let url = menu_url(kind, page);
    let root = root_tag(MENU_BODY);
    assert_eq!(root, r#"<div class="menu" data-menu-popup="" data-state="ok">"#, "the ok golden's root");
    assert_eq!(MENU_INCOMPLETE.replacen(root_tag(MENU_INCOMPLETE), root, 1), MENU_BODY, "only the root differs");
    assert!(MENU_INCOMPLETE.starts_with(r#"<div class="menu" data-menu-popup="" data-state="error">"#), "{MENU_INCOMPLETE}");
    let with_root = |r: &str| menu_body(&id).replacen(root, r, 1);
    let bare = MENU_BODY.strip_prefix(root).and_then(|b| b.trim_end().strip_suffix("</div>")).expect("the root wraps the groups");
    assert!(bare.contains(r#"class="menu-group""#) && !bare.contains(r#"class="menu""#), "{bare}");
    let healthy = Site::healthy();
    crawl(healthy.clone(), false).await.assert_clean("the ok menu root");
    let incomplete = declared_menu(MENU_INCOMPLETE, &id);
    crawl(healthy.clone().with(&url, ok(incomplete.clone())), false).await.assert_failed("an error-state menu root");
    crawl(healthy.clone(), true).await.assert_preview("--allow-errors: the ok menu root");
    crawl(healthy.clone().with(&url, ok(incomplete)), true).await.assert_preview("--allow-errors: an error-state menu root");
    let lax = |site: Site| crawl_run(site, PAGE_MAP, EXCLUDED_MENUS, true, true);
    lax(healthy.clone()).await.assert_preview("--allow-errors --allow-empty: the ok menu root");
    let stateless = with_root(r#"<div class="menu" data-menu-popup="">"#);
    lax(healthy.clone().with(&url, ok(stateless))).await.assert_failed("a menu root without data-state");
    let unknown = with_root(r#"<div class="menu" data-menu-popup="" data-state="down">"#);
    lax(healthy.clone().with(&url, ok(unknown))).await.assert_failed("a menu root whose data-state is down");
    lax(healthy.with(&url, ok(bare))).await.assert_failed("a menu with no .menu root");
}

/// A page answers 200 while a manifest source has never answered, so its `.app` root's
/// `data-state` is the state: the portal's incomplete dashboard root fails the crawl unless
/// `--allow-errors`, each beside the ok root under the same flags. A root without
/// `data-state` or with one neither `ok` nor `error`, and a page with no `.app` root, fail
/// even with `--allow-errors` and `--allow-empty`.
#[tokio::test]
async fn an_error_state_page_root_fails_unless_errors_are_allowed() {
    let target = other_mapped();
    let healthy = Site::healthy();
    let html = healthy.routes.get(target.route).expect("the healthy site serves the page").body.clone();
    let root = root_tag(golden_element(SHELL_PAGE, r#"<div class="app""#));
    assert_eq!(root, r#"<div class="app" data-state="ok">"#, "the shell golden's root");
    assert_eq!(root_tag(PAGE_INCOMPLETE), r#"<div class="app" data-state="error">"#, "the incomplete golden's root");
    assert!(html.contains(root), "the healthy page carries the ok root: {html}");
    let with_root = |r: &str| healthy.clone().with(target.route, ok(html.replacen(root, r, 1)));
    let incomplete = with_root(root_tag(PAGE_INCOMPLETE));
    crawl(healthy.clone(), false).await.assert_clean("the ok page root");
    crawl(incomplete.clone(), false).await.assert_failed("an error-state page root");
    crawl(healthy.clone(), true).await.assert_preview("--allow-errors: the ok page root");
    crawl(incomplete, true).await.assert_preview("--allow-errors: an error-state page root");
    let lax = |site: Site| crawl_run(site, PAGE_MAP, EXCLUDED_MENUS, true, true);
    lax(healthy.clone()).await.assert_preview("--allow-errors --allow-empty: the ok page root");
    lax(with_root(r#"<div class="app">"#)).await.assert_failed("a page root without data-state");
    lax(with_root(r#"<div class="app" data-state="down">"#)).await.assert_failed("a page root whose data-state is down");
    lax(with_root(r#"<div class="shell">"#)).await.assert_failed("a page with no .app root");
}

/// A menu kind a compared, untabbed page draws must be declared: an undeclared `guild` menu on
/// a mapped page fails the crawl, and its `EXCLUDED_MENUS` twin crawls clean, fetching the
/// excluded menu once; an excluded menu answering 500 fails. The same menu on the page only
/// the nav links needs no declaration but is fetched once for its status: answering 200 it
/// crawls clean, answering 500 it fails.
#[tokio::test]
async fn every_portal_menu_kind_on_a_compared_page_is_declared() {
    let target = other_mapped();
    let guild = "/admin/_menu/guild/g-1";
    let healthy = Site::healthy();
    let body = menu_body(&excluded_menu_id(target.page, "guild"));
    let site = healthy.clone().with(target.route, add_to_page(&healthy, target.route, &menu_button(guild))).with(guild, ok(body.clone()));
    crawl(site.clone(), false).await.assert_failed("an undeclared guild menu");

    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push((target.page, "guild", "why"));
    let declared = crawl_lists(site.clone(), PAGE_MAP, &excluded, false).await;
    assert!(matches!(declared.result, Ok(true)), "{:?}", declared.result);
    assert_eq!(declared.server.requests().iter().filter(|r| r.path == guild && r.hx).count(), 1, "{:?}", declared.server.requests());
    let raw = declared.out.path().join("run").join("raw").join(format!("{}.excluded-menu.guild.html", target.page));
    assert!(raw.is_file(), "the excluded menu's body is kept: {}", raw.display());
    crawl_lists(site.with(guild, status(500)), PAGE_MAP, &excluded, false).await.assert_failed("an excluded menu answering 500");

    let nav_only = healthy.clone().with(EXTRA_ROUTE, add_to_page(&healthy, EXTRA_ROUTE, &menu_button(guild))).with(guild, ok(body));
    let c = crawl(nav_only.clone(), false).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
    assert_eq!(c.server.requests().iter().filter(|r| r.path == guild && r.hx).count(), 1, "{:?}", c.server.requests());
    crawl(nav_only.with(guild, status(500)), false).await.assert_failed("a menu on the nav-only page answering 500");
}

/// The production [`EXCLUDED_MENUS`] kinds are drawn by the healthy site and fetched; dropping
/// the page's button makes the exclusion stale, as does one naming a page the crawl does not
/// treat as a compared page or tab, or one listed twice or without a reason.
#[tokio::test]
async fn a_stale_or_malformed_menu_exclusion_fails() {
    let (host, kind, _) = *EXCLUDED_MENUS.first().expect("EXCLUDED_MENUS has an entry");
    let healthy = Site::healthy();
    let clean = crawl(healthy.clone(), false).await;
    assert!(matches!(clean.result, Ok(true)), "{:?}", clean.result);
    let url = menu_url(kind, host);
    assert_eq!(clean.server.requests().iter().filter(|r| r.path == url && r.hx).count(), 1, "the excluded kind is fetched");

    let route = mapped().into_iter().find(|t| t.page == host).expect("the exclusion's page is mapped").route;
    let html = healthy.routes.get(route).expect("served").body.clone();
    let gone = healthy.clone().with(route, ok(html.replace(&menu_button(&url), "")));
    crawl(gone, false).await.assert_failed("the page no longer draws the excluded kind");

    let others = || EXCLUDED_MENUS.iter().copied().filter(|e| (e.0, e.1) != (host, kind));
    let cases = [
        ("a page no compared entry names", EXCLUDED_MENUS.iter().copied().chain([("nosuchpage", "guild", "why")]).collect::<Vec<_>>()),
        ("listed twice", EXCLUDED_MENUS.iter().copied().chain([(host, kind, "again")]).collect()),
        ("no reason", others().chain([(host, kind, " ")]).collect()),
        ("mapped and excluded", EXCLUDED_MENUS.iter().copied().chain([("players", "player", "why")]).collect()),
    ];
    for (why, excluded) in cases {
        crawl_lists(healthy.clone(), PAGE_MAP, &excluded, false).await.assert_failed(why);
    }
}

/// Tab pages join the menu inventory on the page rules: the `player.items` tab page drawing
/// its user menu with no `player.items.menu.user` entry fails, and so does an undeclared
/// `guild` menu on it. Its `EXCLUDED_MENUS` twin hosted by the tab holds the `guild/kick`
/// modal its menu draws to the page rules: undeclared it fails, and with an `EXCLUDED_MODALS`
/// entry hosted by that excluded menu it crawls clean, the excluded menu and modal each fetched
/// once and the menu's body kept; the menu answering 500 fails, and it is stale when the tab
/// page does not draw it.
#[tokio::test]
async fn every_portal_menu_kind_on_a_tab_page_is_declared() {
    let guild = "/admin/_menu/guild/g-1";
    let host = excluded_menu_id("player.items", "guild");
    let kick = modal_url("guild/kick", &host);
    let site = items_site(&menu_button(guild)).with(guild, ok(in_menu(&menu_body(&host), &modal_button(&kick)))).with(&kick, ok(MODAL_BODY));
    let bare: Vec<Page> = PAGE_MAP.iter().copied().chain([items_tab()]).collect();
    let user = "player.items draws a player menu, which is in neither PAGE_MAP nor EXCLUDED_MENUS";
    crawl_map(items_site(""), &bare).await.assert_failed_with("the tab page's user menu undeclared", user);
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([items_tab(), items_menu()]).collect();
    crawl_map(items_site(""), &page_map).await.assert_clean("the tab page's user menu is a menu target");
    let undeclared = "player.items draws a guild menu, which is in neither PAGE_MAP nor EXCLUDED_MENUS";
    crawl_map(site.clone(), &page_map).await.assert_failed_with("an undeclared guild menu on the tab page", undeclared);

    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push(("player.items", "guild", "why"));
    let modal = format!("{host} draws a guild/kick modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS");
    let undeclared = crawl_lists(site.clone(), &page_map, &excluded, false).await;
    undeclared.assert_failed_with("an undeclared guild/kick modal in the tab page's excluded menu", &modal);
    let mut modals = EXCLUDED_MODALS.to_vec();
    modals.push((&host, "guild/kick", ModalWhy { why: "why" }));
    let (page_map, excluded, modals) = (&page_map, &excluded, &modals);
    let run = move |site: Site| crawl_into(ScratchDir::new("crawl"), site, page_map, excluded, modals, false, false);
    let declared = run(site.clone()).await;
    declared.assert_clean("the tab page's guild menu and its guild/kick modal are excluded");
    assert_eq!(fetched(&declared, guild), 1, "{:?}", declared.server.requests());
    assert_eq!(fetched(&declared, &kick), 1, "{:?}", declared.server.requests());
    let raw = declared.out.path().join("run").join("raw").join(format!("{host}.html"));
    assert!(raw.is_file(), "the excluded menu's body is kept: {}", raw.display());
    let down = format!("player.items excluded guild menu ({guild}): answered 500");
    run(site.with(guild, status(500))).await.assert_failed_with("the excluded menu answering 500", &down);
    let stale = "EXCLUDED_MENUS lists player.items menu guild, which player.items does not draw";
    run(items_site("")).await.assert_failed_with("the tab page no longer draws guild", stale);
}

/// The healthy site of a page map with a tab entry, `player.items`, a menu target on it and a
/// modal entry it hosts serves each from that map: the crawl reaches the tab at its `?tab=`
/// route, selected, and writes the tab page, its menu and its modal.
#[tokio::test]
async fn the_healthy_site_serves_a_tab_entry_its_menus_and_modals() {
    let tab = items_tab();
    let test = Modal { id: "test", opener: "testOpener", args: &["a"], module: "test", action: "do" };
    let entry = Page { modal: Some(test), ..tab };
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([tab, items_menu(), entry]).collect();
    let c = crawl_modals(Site::with_map(&page_map, &slot), &page_map, EXCLUDED_MODALS, false).await;
    c.assert_clean("the healthy site of a map with a tab entry");
    assert_eq!(fetched(&c, &panel_url("player.items")), 1, "{:?}", c.server.requests());
    let run = c.out.path().join("run");
    for id in ["player.items", "player.items.menu.user", "player.items.modal.test"] {
        assert!(run.join(format!("{id}.html")).is_file(), "{id} is written");
    }
}

/// `crawl_page`'s status check: a page answering 500 fails even when its body, a whole page
/// with a loadable slot or nothing at all, gives the crawl nothing else to fail on.
#[tokio::test]
async fn a_non_2xx_page_fails() {
    let mapped = other_mapped();
    let whole = Site::healthy().routes.get(mapped.route).expect("the healthy site serves it").body.clone();
    let site = Site::healthy().with(mapped.route, Response { status: 500, ..ok(whole) });
    crawl(site, false).await.assert_failed("a mapped page answering 500 with a whole page");
    let site = Site::healthy().with(mapped.route, status(500));
    crawl(site, false).await.assert_failed("a mapped page answering 500 with an empty body");
    let site = Site::healthy().with(EXTRA_ROUTE, status(500));
    crawl(site, false).await.assert_failed("a page only the nav links answering 500");
}

/// `site`'s page at `route` with its `drawn` (its [`palette_scrim`] or [`palette_opener`])
/// replaced by `with`.
fn replace_shell(site: &Site, route: &str, drawn: &str, with: &str) -> Response {
    let html = site.routes.get(route).expect("the site serves the page").body.clone();
    assert!(html.contains(drawn), "the page draws {drawn}: {html}");
    ok(html.replacen(drawn, with, 1))
}

/// `text` with its one `from` replaced by `to`.
fn swapped(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{text} holds one {from}");
    text.replacen(from, to, 1)
}

/// `crawl_page`'s palette check: a mapped page, a page only the nav links or an entity page
/// whose `[data-palette]` elements are none, two, one not hidden, one not a `.scrim` (the
/// element `admin.js` opens) or a hidden `.scrim` behind a stray one, whose palette holds no
/// `[data-palette-input]`, two, or no `[data-palette-list]` or two (`admin.js` reads one of
/// each), or whose `[data-action=palette]` openers are none or two, and a mapped page whose
/// fragment adds a second palette or opener, fails naming that page and nothing else; the
/// healthy site, each of whose pages holds the portal's own palette and opener, crawls clean,
/// and so does a page drawing a palette input and list outside the palette.
#[tokio::test]
async fn every_fetched_page_holds_one_hidden_palette() {
    crawl(Site::healthy(), false).await.assert_clean("one hidden palette and one opener on every page");
    let mapped = other_mapped();
    let (entity, _) = followers_in(PAGE_MAP).into_iter().next().expect("PAGE_MAP follows a list row");
    let extra = ("p-extra-things".to_string(), EXTRA_ROUTE.to_string());
    let mapped_at = (mapped.page.to_string(), mapped.route.to_string());
    let entity_at = (entity.id(), entry_route(&entity));
    let (scrim, opener) = (palette_scrim(), palette_opener());
    let shown = swapped(scrim, "data-palette hidden", "data-palette");
    let unscrimmed = swapped(scrim, r#"class="scrim""#, r#"class="overlay""#);
    let stray = format!(r#"<div data-palette></div>{scrim}"#);
    let twice = format!("{scrim}{scrim}");
    let (input, list) = (" data-palette-input", " data-palette-list");
    let no_input = swapped(scrim, input, "");
    let two_inputs = swapped(scrim, list, &format!("{list}{input}"));
    let no_list = swapped(scrim, list, "");
    let two_lists = swapped(scrim, input, &format!("{input}{list}"));
    let palette_held = |n: usize, scrims: usize| {
        format!("{n} [data-palette], {scrims} of them a hidden .scrim; the shell draws exactly one, a hidden .scrim")
    };
    let palette_holds = |inputs: usize, lists: usize| {
        format!("palette holds {inputs} [data-palette-input] and {lists} [data-palette-list]; admin.js reads exactly one of each")
    };
    let opener_held = |n: usize| format!("page holds {n} [data-action=palette] openers; the shell draws exactly one");
    let opened_twice = format!("{opener}{opener}");
    let cases = [
        (&mapped_at, scrim, "", format!("page holds {}", palette_held(0, 0))),
        (&extra, scrim, twice.as_str(), format!("page holds {}", palette_held(2, 2))),
        (&entity_at, scrim, shown.as_str(), format!("page holds {}", palette_held(1, 0))),
        (&mapped_at, scrim, unscrimmed.as_str(), format!("page holds {}", palette_held(1, 0))),
        (&extra, scrim, stray.as_str(), format!("page holds {}", palette_held(2, 1))),
        (&mapped_at, scrim, no_input.as_str(), palette_holds(0, 1)),
        (&entity_at, scrim, two_inputs.as_str(), palette_holds(2, 1)),
        (&extra, scrim, no_list.as_str(), palette_holds(1, 0)),
        (&mapped_at, scrim, two_lists.as_str(), palette_holds(1, 2)),
        (&entity_at, opener, "", opener_held(0)),
        (&mapped_at, opener, opened_twice.as_str(), opener_held(2)),
    ];
    for ((page, route), drawn, with, held) in cases {
        let site = Site::healthy();
        let site = site.clone().with(route, replace_shell(&site, route, drawn, with));
        let c = crawl(site, false).await;
        let want = format!("{page}: the {held}");
        c.assert_failed_with(&format!("{page} {held}"), &want);
        assert_eq!(c.failures, vec![want], "the palette is the only defect");
    }
    for (added, held) in [(scrim, format!("page holds {}", palette_held(2, 2))), (opener, opener_held(2))] {
        let site = Site::healthy().with(&mapped.slot, ok(other_fragment(Some("ok"), added)));
        let c = crawl(site, false).await;
        let want = format!("{}: the {held}", mapped.page);
        c.assert_failed_with(&format!("a fragment adds {added}"), &want);
        assert_eq!(c.failures, vec![want], "the palette is the only defect");
    }
    let outside = r#"<input data-palette-input><div data-palette-list></div>"#;
    let site = Site::healthy().with(&mapped.slot, ok(other_fragment(Some("ok"), outside)));
    crawl(site, false).await.assert_clean("a palette input and list outside the palette count for nothing");
}

/// The players page's first `tr.clickable` is followed: its escaped `data-href` is fetched as
/// a page and written as `player.html`; the plain row before it and the row after it are not.
#[tokio::test]
async fn an_entity_page_follows_the_first_clickable_row_of_its_list_page() {
    assert!(followers().iter().any(|(id, list)| id == "player" && *list == "players"), "PAGE_MAP: player follows players");
    let c = crawl(Site::healthy(), false).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
    let pages: Vec<String> =
        c.server.requests().into_iter().filter(|r| r.method == "GET" && !r.hx).map(|r| r.path).collect();
    assert_eq!(pages.iter().filter(|p| *p == "/admin/e/player/x").count(), 1, "the first row is followed: {pages:?}");
    assert!(!pages.iter().any(|p| p == "/admin/e/plain/row" || p == "/admin/e/second/row"), "{pages:?}");
    let player = std::fs::read_to_string(c.out.path().join("run").join("player.html")).expect("player.html");
    assert!(player.contains("player panel") && player.contains(r#"data-state="ok""#), "{player}");
}

/// A players page that lists no row fails the crawl; `--allow-empty` crawls it clean without
/// a `player.html`, reporting EMPTY the player page, the players row menu (players draws no
/// row to hang it on) and the player header menu (its page was skipped), and the diff of
/// player then refuses, naming the list page.
#[tokio::test]
async fn an_empty_list_page_fails_unless_empty_is_allowed_and_then_the_diff_refuses() {
    let empty = Site::healthy().with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let failed = crawl(empty.clone(), false).await;
    failed.assert_failed("players lists no row");
    assert!(!failed.out.path().join("run").join("player.html").exists(), "nothing was followed");

    let allowed = crawl_with(empty, false, true).await;
    assert!(matches!(allowed.result, Ok(true)), "{:?}", allowed.result);
    assert!(allowed.clean_marker(), "--allow-empty crawls clean");
    let run = allowed.out.path().join("run");
    assert!(run.join("players.html").is_file() && !run.join("player.html").exists());
    assert!(!allowed.server.requests().iter().any(|r| r.path.starts_with("/admin/e/")), "nothing was followed");
    assert_eq!(allowed.empty, ["player", "players.menu.user", "player.menu.user"]);
    assert!(!run.join("players.menu.user.html").exists() && !run.join("player.menu.user.html").exists());
    let menus: Vec<String> = allowed.server.requests().into_iter().map(|r| r.path).filter(|p| p.contains("/_menu/player/")).collect();
    assert!(menus.is_empty(), "no player menu was fetched: {menus:?}");

    let repo = FakeRepo::new("crawl-empty-diff", &mock_html(&body(true)));
    let e = format!("{:#}", crate::run_diff(repo.path(), &LISTS, "player", &run, false).expect_err("no player page"));
    assert_eq!(e, format!("{} has no player page: the crawl found no row of players to follow", run.display()));
}

/// With `--allow-empty`, an exclusion on a page that lists no row, reported EMPTY, or on a page
/// that was skipped, is not stale; on a players page that lists rows, the players one is.
#[tokio::test]
async fn an_empty_or_skipped_page_cannot_make_a_menu_exclusion_stale() {
    let empty = Site::healthy().with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push(("players", "guild", "drawn in the players rows"));
    excluded.push(("player", "guild", "drawn on the player page"));
    let allowed = crawl_lists(empty.clone(), PAGE_MAP, &excluded, true).await;
    assert!(matches!(allowed.result, Ok(true)), "{:?}", allowed.result);
    assert_eq!(allowed.empty, ["player", "players.menu.user", "player.menu.user", "players.excluded-menu.guild"]);
    let players_only: Vec<_> = excluded.iter().copied().filter(|e| e.0 != "player" || e.1 != "guild").collect();
    let healthy = crawl_lists(Site::healthy(), PAGE_MAP, &players_only, true).await;
    healthy.assert_failed("players lists rows and draws no guild menu");
}

/// A `FirstRow` page exists only because its list had a row, so `--allow-empty` never makes
/// its menus EMPTY: a player page without its `player` menu, or without a `guild` menu an
/// exclusion names, fails the crawl with `--allow-empty` too.
#[tokio::test]
async fn a_first_row_page_is_never_empty_for_its_menus() {
    let healthy = Site::healthy();
    let route = follower_route("player");
    let html = healthy.routes.get(&route).expect("the healthy site serves the player page").body.clone();
    let button = menu_buttons("player");
    assert!(!button.is_empty() && html.contains(&button), "the player page draws its menu: {html}");
    let bare = healthy.clone().with(&route, ok(html.replace(&button, "")));
    crawl_with(bare, false, true).await.assert_failed("--allow-empty: player draws no player menu");

    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push(("player", "guild", "why"));
    crawl_lists(healthy, PAGE_MAP, &excluded, true).await.assert_failed("--allow-empty: player draws no guild menu");
}

#[test]
fn selects_tab_needs_the_selected_tab_with_that_id() {
    let doc = |html: &str| crate::html::parse(html).expect("parses");
    let on = r#"<button class="tab on" data-href="&#x2f;admin&#x2f;e&#x2f;player&#x2f;p-1?tab=items">Items</button>"#;
    assert!(super::selects_tab(&doc(on), "items"));
    assert!(!super::selects_tab(&doc(on), "item"), "another id is not that tab");
    let fallback = r#"<button class="tab on" data-href="/admin/e/player/p-1?tab=profile"></button><button class="tab" data-href="/admin/e/player/p-1?tab=items"></button>"#;
    assert!(!super::selects_tab(&doc(fallback), "items"), "the portal fell back to its first tab");
}

/// A page map with a tab entry `player.items`: the crawl fetches the first row's page with
/// `?tab=items`; a page that falls back to its first tab fails the crawl, and one that draws
/// the tab selected crawls clean and writes `player.items.html`.
#[tokio::test]
async fn a_tab_target_fetches_its_tab_and_fails_unless_the_page_selects_it() {
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists player");
    let mut page_map = PAGE_MAP.to_vec();
    page_map.extend([Page { tab: Some("items"), ..*player }, items_menu()]);
    let route = follower_route("player");
    let tabbed = format!("{route}?tab=items");
    let healthy = Site::healthy();
    let untabbed = healthy.routes.get(&route).expect("the healthy site serves player").body.clone();
    let slot_start = r#"<div class="slot""#;
    assert!(untabbed.contains(slot_start), "the player page holds the slot the tabs go before");
    // The player page drawing its Profile and Items tabs, `selected` drawn `.on`.
    let with_tabs = |selected: &str| {
        let tab = |id: &str| {
            let on = if id == selected { " on" } else { "" };
            format!(r#"<button class="tab{on}" data-href="{}">{id}</button>"#, escaped(&format!("{route}?tab={id}")))
        };
        let tabs = format!(r#"<div class="tabs">{}{}</div>{slot_start}"#, tab("profile"), tab("items"));
        ok(untabbed.replacen(slot_start, &tabs, 1))
    };
    let fetched = |c: &Crawl| c.server.requests().iter().filter(|r| r.method == "GET" && r.path == tabbed).count();

    let fallback = crawl_map(healthy.clone().with(&tabbed, with_tabs("profile")), &page_map).await;
    fallback.assert_failed("the page fell back to its first tab");
    assert_eq!(fetched(&fallback), 1, "the tab target is fetched with ?tab=items: {:?}", fallback.server.requests());

    let selected = crawl_map(healthy.with(&tabbed, with_tabs("items")), &page_map).await;
    assert!(matches!(selected.result, Ok(true)), "{:?}", selected.result);
    assert!(selected.clean_marker(), "a crawl reaching the tab writes {CLEAN_MARKER}");
    assert_eq!(fetched(&selected), 1, "{:?}", selected.server.requests());
    let page = std::fs::read_to_string(selected.out.path().join("run").join("player.items.html")).expect("player.items.html");
    assert!(page.contains("player panel") && page.contains(r#"class="tab on""#), "{page}");
}

// ---- modal targets -------------------------------------------------------------------

/// Every modal entry of `page_map` hosted by a compared page, tab or menu target
/// ([`Page::is_compared_target`]): its target id, host id and `<module>/<action>`.
fn modals_in(page_map: &[Page]) -> Vec<(String, String, String)> {
    page_map.iter().filter(|p| p.host().is_compared_target()).filter_map(|p| p.modal.map(|m| (p.id(), p.host().id(), m.action_path()))).collect()
}

/// [`modals_in`] of [`PAGE_MAP`].
fn modals() -> Vec<(String, String, String)> {
    modals_in(PAGE_MAP)
}

/// How many times `c` GET `path` with `HX-Request`.
fn fetched(c: &Crawl, path: &str) -> usize {
    c.server.requests().iter().filter(|r| r.method == "GET" && r.path == path && r.hx).count()
}

/// The body of `route` on `site` with `button` removed, which it must draw.
fn without(site: &Site, route: &str, button: &str) -> Response {
    let html = site.routes.get(route).expect("the site serves the page").body.clone();
    assert!(html.contains(button), "the page draws {button}: {html}");
    ok(html.replace(button, ""))
}

/// A mail cancel modal entry, which [`PAGE_MAP`] does not list.
const CANCEL: Modal = Modal { id: "cancel", opener: "cancelMail", args: &["m-1"], module: "mail", action: "cancel" };

/// [`PAGE_MAP`] with [`CANCEL`] hanging off the mail page.
fn cancel_map() -> Vec<Page> {
    let mail = PAGE_MAP.iter().find(|p| p.page == "mail" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists mail");
    let mut page_map = PAGE_MAP.to_vec();
    page_map.push(Page { modal: Some(CANCEL), ..*mail });
    page_map
}

/// The healthy site whose mail panel draws a cancel opener, as the portal draws a queue row's,
/// answering [`MODAL_BODY`].
fn cancel_site() -> Site {
    drawing(Site::healthy(), "mail", "mail/cancel")
}

/// `site` whose page `page` draws an opener of `action` (`<module>/<action>`) in its panel, as the
/// portal draws a queue row's, answering [`MODAL_BODY`].
fn drawing(site: Site, page: &str, action: &str) -> Site {
    let slot_url = panel_url(page);
    let url = modal_url(action, page);
    let panel = site.routes.get(&slot_url).expect("the page's panel").body.clone();
    assert!(panel.contains("</section>"), "{panel}");
    let drawn = panel.replacen("</section>", &format!("{}</section>", modal_button(&url)), 1);
    site.with(&slot_url, ok(drawn)).with(&url, ok(MODAL_BODY))
}

/// The healthy crawl fetches the modal of each modal target once, with `HX-Request`, and
/// writes it as `<id>.html`; each production exclusion's modal is fetched once and kept under
/// `raw/`. On a mail cancel modal entry ([`cancel_map`], [`cancel_site`]): it crawls clean and
/// is written; a second cancel opener after the first is never fetched; a
/// modal answering 500, the portal's result toast (at 200 and at 500), a bare `form.modal` with
/// no `.scrim` around it and a `.scrim` with no `.modal` each fail, each for its own reason; a
/// mail panel drawing no cancel opener fails, and with `--allow-empty` crawls clean reporting the
/// target EMPTY.
#[tokio::test]
async fn a_modal_target_writes_its_modal_and_fails_unless_reached() {
    let targets = modals();
    let healthy = Site::healthy();
    let fine = crawl(healthy.clone(), false).await;
    fine.assert_clean("the healthy modal targets");
    let run = fine.out.path().join("run");
    for (id, page, action) in &targets {
        assert_eq!(fetched(&fine, &modal_url(action, page)), 1, "{id}: {:?}", fine.server.requests());
        let written = std::fs::read_to_string(run.join(format!("{id}.html"))).expect("the modal is written");
        assert_eq!(written, MODAL_BODY, "{id} is written as the portal answers it");
    }
    for (host, action, _) in EXCLUDED_MODALS {
        assert_eq!(fetched(&fine, &modal_url(action, host)), 1, "{host} {action}: {:?}", fine.server.requests());
    }
    assert!(run.join("raw").join("mail.excluded-modal.mail.test-send.html").is_file(), "the excluded modal's body is kept");

    let page_map = cancel_map();
    let page_map = &page_map;
    let cancel = cancel_site();
    let run_map = move |site: Site, allow_empty: bool| crawl_modals(site, page_map, EXCLUDED_MODALS, allow_empty);
    let (id, page) = ("mail.modal.cancel", "mail");
    let url = modal_url("mail/cancel", page);
    let c = run_map(cancel.clone(), false).await;
    c.assert_clean("the mail cancel modal target");
    let written = std::fs::read_to_string(c.out.path().join("run").join(format!("{id}.html"))).expect("the modal is written");
    assert_eq!(written, MODAL_BODY);
    let route = mapped().into_iter().find(|t| t.page == page).expect("mail is mapped").route;
    let second = "/admin/_action/mail/cancel?item=second";
    let twice = run_map(cancel.clone().with(route, add_to_page(&cancel, route, &modal_button(second))), false).await;
    twice.assert_clean("a second cancel opener after the first");
    assert_eq!((fetched(&twice, &url), fetched(&twice, second)), (1, 0), "the first opener wins: {:?}", twice.server.requests());

    let start = MODAL_BODY.find("<form").expect("the golden holds a form");
    let end = MODAL_BODY.rfind("</form>").expect("the form closes") + "</form>".len();
    let form = &MODAL_BODY[start..end];
    assert!(form.starts_with(r#"<form class="modal""#) && !form.contains("scrim"), "{form}");
    let no_modal = MODAL_BODY.replacen(r#"<form class="modal""#, r#"<form class="dialog""#, 1);
    assert!(no_modal != MODAL_BODY && MODAL_BODY.starts_with(r#"<div class="scrim""#), "the golden is a .scrim around a .modal");
    assert!(MODAL_TOAST.starts_with("<div data-toast"), "{MODAL_TOAST}");
    let cases = [
        ("a modal answering 500", status(500), "answered 500 Internal Server Error"),
        ("a toast at 200", ok(MODAL_TOAST), "the portal answers a toast, not a modal"),
        ("a toast at 500", Response { status: 500, ..ok(MODAL_TOAST) }, "answered 500 Internal Server Error, a toast"),
        ("a bare form.modal", ok(form), "the modal answers no .scrim root"),
        ("a .scrim with no .modal", ok(no_modal), "the .scrim holds no .modal"),
    ];
    for (why, response, reason) in cases {
        let c = run_map(cancel.clone().with(&url, response), false).await;
        c.assert_failed_with(why, &format!("{id} ({url}): {reason}"));
    }

    let slot_url = panel_url(page);
    let bare = cancel.clone().with(&slot_url, without(&cancel, &slot_url, &modal_button(&url)));
    let c = run_map(bare.clone(), false).await;
    c.assert_failed_with("mail draws no cancel opener", "mail.modal.cancel: mail draws no mail/cancel opener");
    let allowed = run_map(bare, true).await;
    allowed.assert_clean("--allow-empty: mail draws no cancel opener");
    assert_eq!(allowed.empty, [id]);
    assert!(!allowed.out.path().join("run").join(format!("{id}.html")).exists(), "nothing written for {id}");
    assert_eq!(fetched(&allowed, &url), 0);
}

/// A modal action a compared page or tab draws must be declared: an undeclared `guild/kick`
/// opener on a mapped page, or on the `player.items` tab page, fails the crawl, and its
/// `EXCLUDED_MODALS` twin hosted by that page or tab crawls clean, fetching the excluded modal
/// once and keeping its body; an excluded modal answering 500 fails. The same opener on the
/// page only the nav links needs no declaration, but the modal of its first opener is fetched
/// once for its status and a second opener of the action is not: answering the modal it crawls
/// clean, answering 500 it fails. An `hx-post` to the action (an `Invoke` button) is no modal
/// opener.
#[tokio::test]
async fn every_portal_modal_action_on_a_compared_page_is_declared() {
    let target = other_mapped();
    let kick = "/admin/_action/guild/kick?item=g-1";
    let healthy = Site::healthy();
    let site = healthy.clone().with(target.route, add_to_page(&healthy, target.route, &modal_button(kick))).with(kick, ok(MODAL_BODY));
    let undeclared = format!("{} draws a guild/kick modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS", target.page);
    crawl(site.clone(), false).await.assert_failed_with("an undeclared guild/kick modal", &undeclared);

    let mut excluded = EXCLUDED_MODALS.to_vec();
    excluded.push((target.page, "guild/kick", ModalWhy { why: "why" }));
    let declared = crawl_modals(site.clone(), PAGE_MAP, &excluded, false).await;
    declared.assert_clean("the excluded guild/kick modal");
    assert_eq!(fetched(&declared, kick), 1, "{:?}", declared.server.requests());
    let raw = declared.out.path().join("run").join("raw").join(format!("{}.excluded-modal.guild.kick.html", target.page));
    assert!(raw.is_file(), "the excluded modal's body is kept: {}", raw.display());
    let failed = crawl_modals(site.with(kick, status(500)), PAGE_MAP, &excluded, false).await;
    failed.assert_failed_with("an excluded modal answering 500", &format!("{} excluded guild/kick modal ({kick}): answered 500", target.page));

    let second = "/admin/_action/guild/kick?item=g-2";
    let openers = format!("{}{}", modal_button(kick), modal_button(second));
    let nav_only = healthy.clone().with(EXTRA_ROUTE, add_to_page(&healthy, EXTRA_ROUTE, &openers)).with(second, ok(MODAL_BODY));
    let c = crawl(nav_only.clone().with(kick, ok(MODAL_BODY)), false).await;
    c.assert_clean("the opener on the nav-only page");
    assert_eq!((fetched(&c, kick), fetched(&c, second)), (1, 0), "the first opener only: {:?}", c.server.requests());
    let raw = c.out.path().join("run").join("raw").join("p-extra-things.status-modal.guild.kick.html");
    assert!(raw.is_file(), "the status-only modal's body is kept: {}", raw.display());
    let failed = crawl(nav_only.with(kick, status(500)), false).await;
    failed.assert_failed_with("the opener on the nav-only page answering 500", &format!("p-extra-things guild/kick modal ({kick}): answered 500"));

    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([items_tab(), items_menu()]).collect();
    let tab_page = items_site(&modal_button(kick)).with(kick, ok(MODAL_BODY));
    let undeclared = "player.items draws a guild/kick modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS";
    crawl_map(tab_page.clone(), &page_map).await.assert_failed_with("an undeclared guild/kick modal on the tab page", undeclared);
    let mut on_tab = EXCLUDED_MODALS.to_vec();
    on_tab.push(("player.items", "guild/kick", ModalWhy { why: "why" }));
    let c = crawl_modals(tab_page.clone(), &page_map, &on_tab, false).await;
    c.assert_clean("the excluded guild/kick modal on the tab page");
    assert_eq!(fetched(&c, kick), 1, "{:?}", c.server.requests());
    let raw = c.out.path().join("run").join("raw").join("player.items.excluded-modal.guild.kick.html");
    assert!(raw.is_file(), "the excluded modal's body is kept: {}", raw.display());
    let failed = crawl_modals(tab_page.with(kick, status(500)), &page_map, &on_tab, false).await;
    let down = format!("player.items excluded guild/kick modal ({kick}): answered 500");
    failed.assert_failed_with("the excluded modal on the tab page answering 500", &down);

    let post = format!(r##"<button type="button" class="btn sm" hx-post="{}" hx-target="#modal-root"></button>"##, escaped(kick));
    let invoke = crawl(healthy.clone().with(target.route, add_to_page(&healthy, target.route, &post)), false).await;
    invoke.assert_clean("an hx-post button");
    assert!(!invoke.server.requests().iter().any(|r| r.path == kick), "{:?}", invoke.server.requests());
}

/// The production exclusion `mail/test-send` is drawn in the healthy mail page's head; a mail
/// page no longer drawing it makes the exclusion stale, a failure with `--allow-empty` too while
/// mail lists no row, since a page head action does not hang on a row. An exclusion list that
/// fails validation stops the crawl before it logs in.
#[tokio::test]
async fn a_stale_or_malformed_modal_exclusion_fails() {
    let (host, action, _) = *EXCLUDED_MODALS.first().expect("EXCLUDED_MODALS has an entry");
    assert_eq!((host, action), ("mail", "mail/test-send"));
    let healthy = Site::healthy();
    let route = mapped().into_iter().find(|t| t.page == host).expect("mail is mapped").route;
    let gone = healthy.clone().with(route, without(&healthy, route, &modal_button(&modal_url(action, host))));
    let panel = &gone.routes.get(&panel_url(host)).expect("the mail panel").body;
    assert!(!panel.contains("<tr"), "mail lists no row: {panel}");
    let stale = "EXCLUDED_MODALS lists mail modal mail/test-send, which mail does not draw";
    crawl(healthy.clone(), false).await.assert_clean("mail draws test-send");
    crawl(gone.clone(), false).await.assert_failed_with("mail no longer draws test-send", stale);
    crawl_with(healthy.clone(), false, true).await.assert_clean("--allow-empty: mail draws test-send");
    let allowed = crawl_with(gone, false, true).await;
    allowed.assert_failed_with("--allow-empty: mail lists no row and no longer draws test-send", stale);
    assert!(allowed.empty.is_empty(), "{:?}", allowed.empty);

    let twice: Vec<_> = EXCLUDED_MODALS.iter().copied().chain([(host, action, ModalWhy { why: "again" })]).collect();
    let c = crawl_modals(healthy.clone(), PAGE_MAP, &twice, false).await;
    c.assert_error("modal drift:\n  mail modal mail/test-send is listed in EXCLUDED_MODALS and again in EXCLUDED_MODALS");
    assert!(c.server.requests().is_empty(), "validated before anything is fetched: {:?}", c.server.requests());
    let cases = [
        ("mapped and excluded", ("mail", "mail/cancel", ModalWhy { why: "why" })),
        ("a host that is no page", ("z", "guild/kick", ModalWhy { why: "why" })),
        ("a malformed action", ("mail", "Mail/x", ModalWhy { why: "why" })),
    ];
    for (why, entry) in cases {
        let excluded: Vec<_> = EXCLUDED_MODALS.iter().copied().chain([entry]).collect();
        let c = crawl_modals(healthy.clone(), &cancel_map(), &excluded, false).await;
        c.assert_error("modal drift");
        assert!(c.server.requests().is_empty(), "{why}: {:?}", c.server.requests());
    }
}

/// A modal target on the `FirstRow` player page: drawn there it crawls clean and is written; with
/// players listing no row and `--allow-empty`, the skipped player page makes it EMPTY too, and
/// nothing is fetched for it; without `--allow-empty` that crawl fails.
#[tokio::test]
async fn a_modal_target_on_a_skipped_page_is_empty() {
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists player");
    let kick = Modal { id: "kick", opener: "kickPlayer", args: &[], module: "accounts", action: "kick" };
    let mut page_map = PAGE_MAP.to_vec();
    page_map.push(Page { modal: Some(kick), ..*player });
    let url = modal_url("accounts/kick", "player");
    let route = follower_route("player");
    let healthy = Site::healthy();
    let drawn = healthy.clone().with(&route, add_to_page(&healthy, &route, &modal_button(&url))).with(&url, ok(MODAL_BODY));
    let c = crawl_modals(drawn.clone(), &page_map, EXCLUDED_MODALS, false).await;
    c.assert_clean("player draws its kick opener");
    let written = std::fs::read_to_string(c.out.path().join("run").join("player.modal.kick.html")).expect("the modal is written");
    assert_eq!(written, MODAL_BODY);

    let empty = drawn.with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let allowed = crawl_modals(empty.clone(), &page_map, EXCLUDED_MODALS, true).await;
    allowed.assert_clean("--allow-empty: players lists no row");
    assert_eq!(allowed.empty, ["player", "players.menu.user", "player.menu.user", "player.modal.kick"]);
    assert_eq!(fetched(&allowed, &url), 0);
    crawl_modals(empty, &page_map, EXCLUDED_MODALS, false).await.assert_failed("players lists no row");
}

/// A portal modal whose skeleton matches the stand-in mockup's [`crate::mock::mock_tests::MOCK_MODAL`].
const DIALOG: &str = concat!(
    r#"<div class="scrim" data-scrim=""><div class="modal"><div class="modal-head"><h2>x</h2></div>"#,
    r#"<div class="modal-body"></div><div class="modal-foot"></div></div></div>"#,
);

/// A modal entry hosted by the `players.menu.user` menu target, `players.menu.user.modal.test`,
/// opens from the menu the crawl fetched, not from the players page, which draws no opener of
/// it: the menu drawing its opener crawls clean, fetches the modal once, writes it as
/// `<id>.html`, and `run_diff` compares it with the stand-in's dialog; the menu drawing no
/// opener fails, and with `--allow-empty` crawls clean reporting the target EMPTY.
#[tokio::test]
async fn a_menu_hosted_modal_target_opens_from_the_fetched_menu() {
    let menu = *PAGE_MAP.iter().find(|p| p.id() == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let test = Modal { id: "test", opener: "testOpener", args: &["a"], module: "test", action: "do" };
    let entry = Page { modal: Some(test), ..menu };
    let id = entry.id();
    assert_eq!(id, "players.menu.user.modal.test");
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([entry]).collect();
    let at = menu_url(menu.menu.expect("a menu entry").kind, menu.page);
    let url = modal_url("test/do", &menu.id());
    let dialog = DIALOG;
    let healthy = Site::healthy();
    let drawn = healthy.clone().with(&at, ok(in_menu(&menu_body(&menu.id()), &modal_button(&url)))).with(&url, ok(dialog));
    let c = crawl_modals(drawn.clone(), &page_map, EXCLUDED_MODALS, false).await;
    c.assert_clean("the players user menu draws the test opener");
    assert_eq!(fetched(&c, &url), 1, "{:?}", c.server.requests());
    let run = c.out.path().join("run");
    let written = std::fs::read_to_string(run.join(format!("{id}.html"))).expect("the modal is written");
    assert_eq!(written, dialog);
    // The stand-in defines no production modal opener, so the diff reads a map without them.
    let diff_map: Vec<Page> = PAGE_MAP.iter().copied().filter(|p| p.modal.is_none()).chain([entry]).collect();
    let repo = FakeRepo::with_map("crawl-menu-modal-diff", &mock_html(&body(true)), &diff_map);
    let lists = Lists { page_map: &diff_map, ..LISTS };
    assert!(crate::run_diff(repo.path(), &lists, &id, &run, false).expect("runs"), "the same dialog is clean");

    let bare = drawn.clone().with(&at, ok(menu_body(&menu.id())));
    let missing = "players.menu.user.modal.test: players.menu.user draws no test/do opener (--allow-empty skips it)";
    crawl_modals(bare.clone(), &page_map, EXCLUDED_MODALS, false).await.assert_failed_with("the menu draws no test opener", missing);
    let allowed = crawl_modals(bare, &page_map, EXCLUDED_MODALS, true).await;
    allowed.assert_clean("--allow-empty: the menu draws no test opener");
    assert_eq!(allowed.empty, [id.as_str()]);
    assert_eq!(fetched(&allowed, &url), 0);

    // players listing no row skips its user menu, so the modal that menu hosts is EMPTY too.
    let rowless = drawn.with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let skipped = crawl_modals(rowless, &page_map, EXCLUDED_MODALS, true).await;
    skipped.assert_clean("--allow-empty: players lists no row");
    let menu_id = menu.id();
    assert!(skipped.empty.contains(&menu_id) && skipped.empty.contains(&id), "{:?}", skipped.empty);
    assert_eq!(fetched(&skipped, &url), 0, "{:?}", skipped.server.requests());
}

/// A status-only menu, the guild menu of the nav-only extra page, declares none of the modal
/// actions it draws, but the first opener of each is fetched for its status: a guild/kick
/// opener in it crawls clean, fetched once with its body kept, and fails answering 500.
#[tokio::test]
async fn a_status_only_menu_fetches_its_modals_for_their_status() {
    let guild = "/admin/_menu/guild/g-1";
    let kick = "/admin/_action/guild/kick?item=g-1";
    let healthy = Site::healthy();
    let menu = in_menu(&menu_body("p-extra-things.status-menu.guild"), &modal_button(kick));
    let nav_only = healthy.clone().with(EXTRA_ROUTE, add_to_page(&healthy, EXTRA_ROUTE, &menu_button(guild))).with(guild, ok(menu));
    let c = crawl(nav_only.clone().with(kick, ok(MODAL_BODY)), false).await;
    c.assert_clean("the opener in the status-only menu");
    assert_eq!(fetched(&c, kick), 1, "{:?}", c.server.requests());
    let raw = c.out.path().join("run").join("raw").join("p-extra-things.status-menu.guild.status-modal.guild.kick.html");
    assert!(raw.is_file(), "the status-only modal's body is kept: {}", raw.display());
    let failed = crawl(nav_only.with(kick, status(500)), false).await;
    let down = format!("p-extra-things.status-menu.guild guild/kick modal ({kick}): answered 500");
    failed.assert_failed_with("the opener in the status-only menu answering 500", &down);
}

/// A compared menu holds the modal actions it draws to the page rules: a `guild/kick` opener
/// in the `players.menu.user` menu fails the crawl as undeclared, and its `EXCLUDED_MODALS`
/// twin hosted by that menu crawls clean, fetching the excluded modal once and keeping its body.
#[tokio::test]
async fn every_modal_action_a_compared_menu_draws_is_declared() {
    let menu = *PAGE_MAP.iter().find(|p| p.id() == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let host = menu.id();
    let at = menu_url(menu.menu.expect("a menu entry").kind, menu.page);
    let kick = modal_url("guild/kick", &host);
    let healthy = Site::healthy();
    let site = healthy.clone().with(&at, ok(in_menu(&menu_body(&host), &modal_button(&kick)))).with(&kick, ok(MODAL_BODY));
    let undeclared = "players.menu.user draws a guild/kick modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS";
    crawl(site.clone(), false).await.assert_failed_with("an undeclared guild/kick modal in a menu", undeclared);

    let mut excluded = EXCLUDED_MODALS.to_vec();
    excluded.push((&host, "guild/kick", ModalWhy { why: "why" }));
    let declared = crawl_modals(site, PAGE_MAP, &excluded, false).await;
    declared.assert_clean("the excluded guild/kick modal in a menu");
    assert_eq!(fetched(&declared, &kick), 1, "{:?}", declared.server.requests());
    let raw = declared.out.path().join("run").join("raw").join("players.menu.user.excluded-modal.guild.kick.html");
    assert!(raw.is_file(), "the excluded modal's body is kept: {}", raw.display());
}

/// The production exclusion `core.events/sub.pause` hangs off the excluded events
/// `subscription` menu: the healthy crawl fetches its modal once from that menu. The menu no
/// longer drawing it makes the exclusion stale, with `--allow-empty` too, each beside its clean
/// twin; the excluded modal answering 500 fails.
#[tokio::test]
async fn a_menu_modal_exclusion_is_fetched_and_fails_stale_or_down() {
    let host = excluded_menu_id("events", "subscription");
    let action = "core.events/sub.pause";
    assert!(EXCLUDED_MODALS.iter().any(|(h, a, _)| *h == host && *a == action), "EXCLUDED_MODALS: {host} {action}");
    let url = modal_url(action, &host);
    let healthy = Site::healthy();
    let fine = crawl(healthy.clone(), false).await;
    fine.assert_clean("the subscription menu draws sub.pause");
    assert_eq!(fetched(&fine, &url), 1, "{:?}", fine.server.requests());
    let raw = fine.out.path().join("run").join("raw").join(format!("{host}.excluded-modal.core.events.sub.pause.html"));
    assert!(raw.is_file(), "the excluded modal's body is kept: {}", raw.display());

    let gone = healthy.clone().with(&menu_url("subscription", "events"), ok(declared_menu(MENU_BODY, "")));
    let stale = format!("EXCLUDED_MODALS lists {host} modal {action}, which {host} does not draw");
    crawl(gone.clone(), false).await.assert_failed_with("the subscription menu no longer draws sub.pause", &stale);
    crawl_with(healthy.clone(), false, true).await.assert_clean("--allow-empty: the subscription menu draws sub.pause");
    crawl_with(gone, false, true).await.assert_failed_with("--allow-empty: the subscription menu no longer draws sub.pause", &stale);

    let down = format!("{host} excluded {action} modal ({url}): answered 500");
    crawl(healthy.with(&url, status(500)), false).await.assert_failed_with("the excluded sub.pause modal answering 500", &down);
}

// ---- modals on tab pages -------------------------------------------------------------

/// The `items` tab entry of the `FirstRow` player page, which [`PAGE_MAP`] does not list.
fn items_tab() -> Page {
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.is_page()).expect("PAGE_MAP lists player");
    Page { tab: Some("items"), ..*player }
}

/// The `player.items.menu.user` menu target, which [`PAGE_MAP`] does not list: the player page
/// draws its user menu on every tab, so a crawl comparing [`items_tab`] declares it there.
fn items_menu() -> Page {
    let user = *PAGE_MAP.iter().find(|p| p.id() == "player.menu.user").expect("PAGE_MAP: player.menu.user");
    Page { tab: Some("items"), ..user }
}

/// The player entity route with `?tab=items`, as the crawl follows [`items_tab`].
fn items_route() -> String {
    format!("{}?tab=items", follower_route("player"))
}

/// The healthy site answering [`items_route`] with the player page, its `items` tab drawn
/// selected and `extra` after it.
fn items_site(extra: &str) -> Site {
    let healthy = Site::healthy();
    let tab = format!(r#"<button class="tab on" data-href="{}">items</button>{extra}"#, escaped(&items_route()));
    healthy.clone().with(&items_route(), add_to_page(&healthy, &follower_route("player"), &tab))
}

/// A modal entry hosted by the `player.items` tab entry, `player.items.modal.test`, opens from
/// the tab page, not from the untabbed player page, which draws no opener of it: the tab page
/// drawing its opener crawls clean, fetches the modal once, writes it as `<id>.html`, and
/// `run_diff` compares it with the stand-in's dialog; the tab page drawing no opener fails.
#[tokio::test]
async fn a_tab_hosted_modal_target_opens_from_the_tab_page_and_is_diffed() {
    let tab = items_tab();
    let test = Modal { id: "test", opener: "testOpener", args: &["a"], module: "test", action: "do" };
    let entry = Page { modal: Some(test), ..tab };
    let id = entry.id();
    assert_eq!(id, "player.items.modal.test");
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([tab, items_menu(), entry]).collect();
    let url = modal_url("test/do", &tab.id());
    let c = crawl_modals(items_site(&modal_button(&url)).with(&url, ok(DIALOG)), &page_map, EXCLUDED_MODALS, false).await;
    c.assert_clean("the items tab draws the test opener");
    assert_eq!(fetched(&c, &url), 1, "{:?}", c.server.requests());
    let run = c.out.path().join("run");
    let written = std::fs::read_to_string(run.join(format!("{id}.html"))).expect("the modal is written");
    assert_eq!(written, DIALOG);
    // The stand-in defines no production modal opener, so the diff reads a map without them.
    let diff_map: Vec<Page> = PAGE_MAP.iter().copied().filter(|p| p.modal.is_none()).chain([tab, entry]).collect();
    let repo = FakeRepo::with_map("crawl-tab-modal-diff", &mock_html(&body(true)), &diff_map);
    let lists = Lists { page_map: &diff_map, ..LISTS };
    assert!(crate::run_diff(repo.path(), &lists, &id, &run, false).expect("runs"), "the same dialog is clean");

    let bare = items_site("").with(&url, ok(DIALOG));
    let missing = "player.items.modal.test: player.items draws no test/do opener (--allow-empty skips it)";
    crawl_modals(bare, &page_map, EXCLUDED_MODALS, false).await.assert_failed_with("the items tab draws no test opener", missing);
}

/// An `EXCLUDED_MODALS` entry hosted by the `player.items` tab its page no longer draws is
/// stale, with `--allow-empty` or not, each beside its clean twin drawing it.
#[tokio::test]
async fn a_stale_tab_modal_exclusion_fails_with_or_without_allow_empty() {
    let kick = modal_url("guild/kick", "player.items");
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([items_tab(), items_menu()]).collect();
    let mut excluded = EXCLUDED_MODALS.to_vec();
    excluded.push(("player.items", "guild/kick", ModalWhy { why: "why" }));
    let drawn = items_site(&modal_button(&kick)).with(&kick, ok(MODAL_BODY));
    let gone = items_site("");
    let stale = "EXCLUDED_MODALS lists player.items modal guild/kick, which player.items does not draw";
    for allow_empty in [false, true] {
        let c = crawl_modals(drawn.clone(), &page_map, &excluded, allow_empty).await;
        c.assert_clean(&format!("--allow-empty {allow_empty}: the items tab draws guild/kick"));
        assert_eq!(fetched(&c, &kick), 1, "{:?}", c.server.requests());
        let c = crawl_modals(gone.clone(), &page_map, &excluded, allow_empty).await;
        c.assert_failed_with(&format!("--allow-empty {allow_empty}: the items tab no longer draws guild/kick"), stale);
    }
}

/// A menu target on a tab page, `player.items.menu.user`, holds the modal actions its fetched
/// menu draws to the page rules, as on an untabbed page: an undeclared `guild/kick` opener in
/// it fails the crawl; its mapped twin, a modal entry hosted by that menu target, is written,
/// and its `EXCLUDED_MODALS` twin hosted by it crawls clean, the menu and the modal each
/// fetched once and the modal's body kept, and fails answering 500.
#[tokio::test]
async fn a_tab_page_menu_target_holds_its_modals_to_the_page_rules() {
    let tab = items_tab();
    let menu = items_menu();
    let host = menu.id();
    assert_eq!(host, "player.items.menu.user");
    let kind = menu.menu.expect("a menu entry").kind;
    let (own, at) = (menu_url(kind, "player"), menu_url(kind, &host));
    let kick = modal_url("guild/kick", &host);
    // The tab page opens its own menu, so the untabbed player.menu.user keeps the healthy one.
    let page = items_site("").routes.get(&items_route()).expect("the items tab page").body.clone();
    assert!(page.contains(&escaped(&own)), "the tab page draws the user menu: {page}");
    let site = Site::healthy()
        .with(&items_route(), ok(page.replace(&escaped(&own), &escaped(&at))))
        .with(&at, ok(in_menu(&menu_body(&host), &modal_button(&kick))))
        .with(&kick, ok(MODAL_BODY));
    let page_map: Vec<Page> = PAGE_MAP.iter().copied().chain([tab, menu]).collect();
    let undeclared = "player.items.menu.user draws a guild/kick modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS";
    crawl_map(site.clone(), &page_map).await.assert_failed_with("an undeclared guild/kick modal in the tab's menu", undeclared);

    let kick_modal = Modal { id: "kick", opener: "kickGuild", args: &[], module: "guild", action: "kick" };
    let entry = Page { modal: Some(kick_modal), ..menu };
    let mapped: Vec<Page> = page_map.iter().copied().chain([entry]).collect();
    let c = crawl_modals(site.clone(), &mapped, EXCLUDED_MODALS, false).await;
    c.assert_clean("the mapped guild/kick modal in the tab's menu");
    let written = std::fs::read_to_string(c.out.path().join("run").join(format!("{}.html", entry.id()))).expect("the modal is written");
    assert_eq!(written, MODAL_BODY);

    let mut excluded = EXCLUDED_MODALS.to_vec();
    excluded.push((&host, "guild/kick", ModalWhy { why: "why" }));
    let c = crawl_modals(site.clone(), &page_map, &excluded, false).await;
    c.assert_clean("the excluded guild/kick modal in the tab's menu");
    // The menu target is fetched once, as a target, not again as a status-only menu.
    assert_eq!(fetched(&c, &at), 1, "{:?}", c.server.requests());
    assert_eq!(fetched(&c, &kick), 1, "{:?}", c.server.requests());
    let raw = c.out.path().join("run").join("raw").join("player.items.menu.user.excluded-modal.guild.kick.html");
    assert!(raw.is_file(), "the excluded modal's body is kept: {}", raw.display());
    let down = format!("{host} excluded guild/kick modal ({kick}): answered 500");
    let failed = crawl_modals(site.with(&kick, status(500)), &page_map, &excluded, false).await;
    failed.assert_failed_with("the excluded modal in the tab's menu answering 500", &down);
}

/// Each production `EXCLUDED_MOCK_MODALS` claim holds on the healthy site, which draws the
/// `Disabled` claims' controls refused as not implemented and no opener of the `Unseeded` mail
/// cancel. A `Disabled` claim fails when its host, a page head or a menu, draws no disabled
/// control of its action, or draws an opener of its action; the `Unseeded` claim fails when
/// mail draws a cancel opener. Neither drawn opener fails as undeclared too: the claim names it.
///
/// The healthy site draws its disabled controls from the production `EXCLUDED_MOCK_MODALS`
/// actions ([`disabled_actions`]), so its clean crawl proves the checker, not that the claims
/// hold on the portal; a live crawl of the dev fleet proves that.
#[tokio::test]
async fn each_portal_claim_of_a_mockup_modal_exclusion_holds_on_its_host() {
    let c = crawl(Site::healthy(), false).await;
    c.assert_clean("the healthy site draws what each claim says");
    assert!(c.unchecked.is_empty(), "every claim's host was fetched: {:?}", c.unchecked);
    let undeclared = "no EXCLUDED_MOCK_MODALS claim names it";

    let healthy = Site::healthy();
    let head = follower_route("player");
    let suspend = disabled_button("accounts/suspend-header", "not-implemented");
    let c = crawl(healthy.clone().with(&head, without(&healthy, &head, &suspend)), false).await;
    c.assert_failed_with(
        "player draws no suspend-header",
        "EXCLUDED_MOCK_MODALS player PAGES.suspend says the portal draws accounts/suspend-header disabled as not-implemented, but player draws no disabled control of it refused so",
    );

    let menu = menu_url("player", "players");
    let delete = disabled_item("accounts/delete-account", "not-implemented");
    let c = crawl(healthy.clone().with(&menu, without(&healthy, &menu, &delete)), false).await;
    c.assert_failed_with(
        "the players menu draws no delete-account",
        "EXCLUDED_MOCK_MODALS players.menu.user PAGES.deleteAccount says the portal draws accounts/delete-account disabled as not-implemented, but players.menu.user draws no disabled control of it refused so",
    );

    let body = healthy.routes.get(&menu).expect("the players menu").body.clone();
    let url = modal_url("accounts/suspend", "players.menu.user");
    let enabled = healthy.clone().with(&menu, ok(in_menu(&body, &modal_button(&url)))).with(&url, ok(MODAL_BODY));
    let c = crawl(enabled, false).await;
    c.assert_failed_with(
        "the players menu draws a suspend opener",
        "EXCLUDED_MOCK_MODALS players.menu.user PAGES.suspend says the portal draws accounts/suspend disabled, but players.menu.user draws an opener of it",
    );
    assert!(!c.failures.iter().any(|f| f.contains(undeclared)), "{:#?}", c.failures);

    let c = crawl(cancel_site(), false).await;
    c.assert_failed_with(
        "mail draws a cancel opener",
        "EXCLUDED_MOCK_MODALS mail PAGES.cancelMail says the dev fleet seeds nothing that draws mail/cancel, but mail draws an opener of it",
    );
    assert!(!c.failures.iter().any(|f| f.contains(undeclared)), "{:#?}", c.failures);
}

/// A `Disabled` claim holds on a disabled control of its action refused as not implemented,
/// wherever the host draws it: the players menu with its delete-account item moved to the head
/// of the menu crawls clean.
#[tokio::test]
async fn a_disabled_claim_holds_on_a_not_implemented_control_of_its_action() {
    let healthy = Site::healthy();
    let menu = menu_url("player", "players");
    let item = disabled_item("accounts/delete-account", "not-implemented");
    let body = without(&healthy, &menu, &item).body;
    let at = body.find('>').expect("the .menu root opens") + 1;
    let moved = format!("{}{item}{}", &body[..at], &body[at..]);
    crawl(healthy.with(&menu, ok(moved)), false).await.assert_clean("delete-account drawn not implemented at the menu head");
}

/// A `Disabled` claim holds only for a control refused as not implemented: the players menu
/// drawing delete-account, a `.menu-item.disabled`, and the player page head drawing
/// suspend-header, a `button[disabled]`, disabled for any other reason, or with no refusal
/// kind, fail the claim naming the host, the action and the kind, alone or beside a not
/// implemented control of it; a not implemented control of another action holds no claim.
#[tokio::test]
async fn a_disabled_claim_fails_on_a_control_refused_for_another_reason() {
    let healthy = Site::healthy();
    let hosts: [(String, &str, &str, fn(&str, &str) -> String); 2] = [
        (menu_url("player", "players"), "players.menu.user", "deleteAccount", disabled_item),
        (follower_route("player"), "player", "suspend", disabled_button),
    ];
    for (route, host, opener, control) in hosts {
        let action = if host == "player" { "accounts/suspend-header" } else { "accounts/delete-account" };
        let refused = control(action, "not-implemented");
        let html = healthy.routes.get(&route).expect("the site serves the host").body.clone();
        assert_eq!(html.matches(&refused).count(), 1, "{host} draws {action} not implemented once: {html}");
        let drawn = |with: &str| healthy.clone().with(&route, ok(html.replacen(&refused, with, 1)));
        let claim = format!("EXCLUDED_MOCK_MODALS {host} PAGES.{opener} says the portal draws {action} disabled as not-implemented");
        let missing = format!("{claim}, but {host} draws no disabled control of it refused so");
        for kind in ["denied", "read-only", "module-down", ""] {
            let other = format!("{claim}, but {host} draws it disabled as {kind:?}");
            let c = crawl(drawn(&control(action, kind)), false).await;
            c.assert_failed_with(&format!("{host} {kind}"), &other);
            assert!(c.failures.iter().any(|f| f == &missing), "{host} {kind}: {:#?}", c.failures);
            let c = crawl(drawn(&format!("{refused}{}", control(action, kind))), false).await;
            c.assert_failed_with(&format!("{host} {kind} beside not-implemented"), &other);
            assert!(!c.failures.iter().any(|f| f == &missing), "{host} {kind}: {:#?}", c.failures);
        }
        let elsewhere = control("accounts/delete-other", "not-implemented");
        crawl(drawn(&elsewhere), false).await.assert_failed_with(&format!("{host}: another action's control"), &missing);
    }
}

/// `refusals_of` on the portal's own goldens: the row menu's `economy/grant` refused to the
/// role, the same menu with economy down refusing both its grant items as `module-down`, and
/// a panel tool `button[disabled]` refused as not implemented; an action drawn enabled, or not
/// drawn, has no refusal.
#[test]
fn refusals_of_reads_the_portals_disabled_controls() {
    let doc = |golden: &str| crate::html::parse(golden).expect("the golden parses");
    assert_eq!(super::refusals_of(&doc(MENU_BODY), "economy/grant"), ["denied"]);
    assert_eq!(super::refusals_of(&doc(MENU_DOWN), "economy/grant"), ["module-down", "module-down"]);
    assert_eq!(super::refusals_of(&doc(TOOL_BODY), "golden/pause"), ["not-implemented"]);
    assert!(super::refusals_of(&doc(MENU_BODY), "players/kick").is_empty(), "kick is drawn enabled");
    assert!(super::refusals_of(&doc(MENU_BODY), "golden/pause").is_empty(), "the menu draws no pause");
}

/// An `Action` claim declares the action it names on its host: drawn there, it crawls clean
/// and its modal is fetched once and kept under `raw/`; drawn nowhere, the claim fails.
#[tokio::test]
async fn an_action_claim_is_fetched_where_drawn_and_fails_where_not() {
    let claim = MockModalExclusion { host: "mail", opener: MockOpener::Pages("kick"), portal: PortalModal::Action("guild/kick"), why: "why" };
    let claims: Vec<_> = unmapped_claims(PAGE_MAP).into_iter().chain([claim]).collect();
    let lists = Lists { excluded_mock_modals: &claims, ..LISTS };
    let c = crawl_lists_into(ScratchDir::new("crawl"), drawing(Site::healthy(), "mail", "guild/kick"), &lists, false, false).await;
    c.assert_clean("mail draws the claimed guild/kick");
    assert_eq!(fetched(&c, &modal_url("guild/kick", "mail")), 1, "the claimed modal is fetched once");
    assert!(c.out.path().join("run").join("raw").join("mail.mock-modal.guild.kick.html").is_file(), "the claimed modal's body is kept");
    let c = crawl_lists_into(ScratchDir::new("crawl"), Site::healthy(), &lists, false, false).await;
    c.assert_failed_with("mail draws no guild/kick", "EXCLUDED_MOCK_MODALS mail PAGES.kick says the portal opens guild/kick, which mail does not draw");
}

/// With `--allow-empty` and players listing no row, the player page and both user menus are not
/// fetched: the crawl writes the CLEAN marker, and reports each `EXCLUDED_MOCK_MODALS` claim of
/// those hosts UNCHECKED, in list order; mail's claim, on a fetched host, is checked.
#[tokio::test]
async fn a_claim_on_an_unfetched_host_is_reported_unchecked() {
    let empty = Site::healthy().with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let c = crawl_with(empty, false, true).await;
    c.assert_clean("--allow-empty: players lists no row");
    let skipped = ["player", "players.menu.user", "player.menu.user"];
    let want: Vec<String> = EXCLUDED_MOCK_MODALS
        .iter()
        .filter(|x| skipped.contains(&x.host))
        .map(|x| format!("EXCLUDED_MOCK_MODALS {} {}", x.host, x.opener))
        .collect();
    assert_eq!(want.len() + 1, EXCLUDED_MOCK_MODALS.len(), "every claim but mail's is on a skipped host");
    assert_eq!(c.unchecked, want);
}

/// A claim whose host is no compared page, tab or menu target fails before anything is
/// fetched, naming the claim and the host, and as the only problem: the production claims
/// beside it pass. An `EXCLUDED_MENUS` menu of a compared page is no such host either.
#[tokio::test]
async fn a_claim_on_a_host_that_is_no_crawl_target_fails() {
    let (page, kind, _) = EXCLUDED_MENUS[0];
    for host in ["nowhere".to_string(), excluded_menu_id(page, kind)] {
        let host: &str = &host;
        let claim = MockModalExclusion { host, opener: MockOpener::Pages("kick"), portal: PortalModal::Absent, why: "why" };
        let claims: Vec<_> = unmapped_claims(PAGE_MAP).into_iter().chain([claim]).collect();
        let lists = Lists { excluded_mock_modals: &claims, ..LISTS };
        let c = crawl_lists_into(ScratchDir::new("crawl"), Site::healthy(), &lists, false, false).await;
        let want = format!(
            "modal drift:\n  EXCLUDED_MOCK_MODALS {host} PAGES.kick names host {host}, which is no PAGE_MAP page or tab that is not pending or menu target of one"
        );
        match &c.result {
            Err(e) => assert_eq!(format!("{e:#}"), want),
            other => panic!("{host}: expected {want:?}, got {other:?}"),
        }
        assert!(c.server.requests().is_empty(), "{host}: validated before anything is fetched: {:?}", c.server.requests());
        assert!(!c.clean_marker(), "{host}");
    }
}

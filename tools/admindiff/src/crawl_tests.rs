//! `crawl` against a loopback fake portal: no Postgres, no fleet. Each failing site differs
//! from [`Site::healthy`], which crawls clean, by the one defect it names. The healthy site
//! is built from [`PAGE_MAP`] and [`EXCLUDED_MENUS`], so a page or exclusion added there is
//! served too and every failing site still has only its own defect.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::{run_with, Config, CLEAN_MARKER, MAX_ROUNDS};
use crate::mock::mock_tests::FakeRepo;
use crate::pages::{Page, Route, EXCLUDED_MENUS, PAGE_MAP};
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
    PAGE_MAP
        .iter()
        .filter(|p| p.menu.is_none())
        .filter_map(|p| match p.route {
            Route::Mapped(route) => Some(Target { page: p.page, route, slot: panel_url(p.page) }),
            Route::FirstRow(_) | Route::Pending => None,
        })
        .collect()
}

/// Every `FirstRow` page of [`PAGE_MAP`] with the list page it follows.
fn followers() -> Vec<(&'static str, &'static str)> {
    PAGE_MAP
        .iter()
        .filter(|p| p.menu.is_none() && p.tab.is_none())
        .filter_map(|p| match p.route {
            Route::FirstRow(list) => Some((p.page, list)),
            Route::Mapped(_) | Route::Pending => None,
        })
        .collect()
}

/// Every menu entry of [`PAGE_MAP`] on an untabbed page: its target id, host page and kind.
fn menus() -> Vec<(String, &'static str, &'static str)> {
    PAGE_MAP.iter().filter(|p| p.tab.is_none()).filter_map(|p| p.menu.map(|m| (p.id(), p.page, m.kind))).collect()
}

/// The menu the healthy site's page `page` opens for its `kind` menu entry.
fn menu_url(kind: &str, page: &str) -> String {
    format!("/admin/_menu/{kind}/{page}")
}

/// The menu body the healthy site serves at every [`menu_url`].
const MENU_BODY: &str = r#"<div class="menu-group"><span>accounts</span></div><button class="menu-item"><span>Open</span></button>"#;

/// A menu button opening `url`, as the portal's template writes it.
fn menu_button(url: &str) -> String {
    format!(r#"<button class="btn sm icon ghost" data-menu-url="{}"></button>"#, escaped(url))
}

/// The buttons page `page` draws for its menu entries: in each table row on a list page
/// ([`rows`]), as the portal draws a row menu, else in the page.
fn menu_buttons(page: &str) -> String {
    menus().iter().filter(|(_, host, _)| *host == page).map(|(_, _, kind)| menu_button(&menu_url(kind, page))).collect()
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

fn shell(nav_links: &str, h1: &str, content: &str) -> String {
    format!(
        concat!(
            r#"<!DOCTYPE html><html><body><div class="app"><aside class="sidebar"><nav class="nav">{nav_links}</nav></aside>"#,
            r#"<main class="main"><div class="page"><div class="page-head"><h1>{h1}</h1></div>{content}</div></main>"#,
            r#"</div></body></html>"#,
        ),
        nav_links = nav_links,
        h1 = h1,
        content = content
    )
}

impl Site {
    /// Clean: the dashboard, every mapped page of [`PAGE_MAP`] and one page only the nav
    /// links, each with one slot whose fragment is in the ok state; a list page's fragment
    /// lists rows whose first opens its follower's entity page, which has one slot too. A page
    /// hosting menu entries draws their buttons ([`menu_buttons`]), in its rows if it lists
    /// any, and a page an [`EXCLUDED_MENUS`] entry names draws its buttons
    /// ([`excluded_buttons`]); each menu answers [`MENU_BODY`].
    fn healthy() -> Site {
        Site::with_content(&slot)
    }

    /// The healthy site's pages and nav with each page's content `content(<its slot url>)`;
    /// every slot url answers its ok fragment.
    fn with_content(content: &dyn Fn(&str) -> String) -> Site {
        let mut pages: Vec<(&str, &str, String)> = mapped().into_iter().map(|t| (t.route, t.page, t.slot)).collect();
        if !pages.iter().any(|(route, _, _)| *route == DASHBOARD) {
            pages.push((DASHBOARD, "dashboard", dashboard_slot()));
        }
        pages.push((EXTRA_ROUTE, "things", EXTRA_SLOT.to_string()));
        let links = nav(&pages.iter().map(|(route, _, _)| *route).collect::<Vec<_>>());
        let mut routes = BTreeMap::new();
        for (route, page, slot_url) in &pages {
            let title = if *route == DASHBOARD { "Dashboard" } else { page };
            let listed: String = followers()
                .iter()
                .filter(|(_, list)| list == page)
                .map(|(f, _)| rows(&follower_route(f), &menu_buttons(page)))
                .collect();
            let in_page = if listed.is_empty() { menu_buttons(page) } else { String::new() };
            let html = format!("{}{in_page}{}", content(slot_url), excluded_buttons(page));
            routes.insert(route.to_string(), ok(shell(&links, title, &html)));
            routes.insert(slot_url.clone(), ok(fragment(Some("ok"), &format!("{page} panel"), &listed)));
        }
        for (page, _) in followers() {
            let slot_url = panel_url(page);
            let html = format!("{}{}{}", content(&slot_url), menu_buttons(page), excluded_buttons(page));
            routes.insert(follower_route(page), ok(shell(&links, page, &html)));
            routes.insert(slot_url, ok(fragment(Some("ok"), &format!("{page} panel"), "")));
        }
        for (_, page, kind) in menus() {
            routes.insert(menu_url(kind, page), ok(MENU_BODY));
        }
        for (page, kind, _) in EXCLUDED_MENUS {
            routes.insert(menu_url(kind, page), ok(MENU_BODY));
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
    /// The targets the crawl reported EMPTY.
    empty: Vec<String>,
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
    let server = Server::start(site);
    let out = ScratchDir::new("crawl");
    let cfg = Config {
        base_url: server.base_url.clone(),
        username: "admin".to_string(),
        password: "admin".to_string(),
        allow_errors,
        allow_empty,
        out: out.path().join("run"),
    };
    let (result, empty) = match run_with(&cfg, page_map, excluded_menus).await {
        Ok(o) => (Ok(o.clean), o.empty),
        Err(e) => (Err(e), Vec::new()),
    };
    Crawl { result, empty, out, server }
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
    let login_title = Site::healthy().with(DASHBOARD, ok(shell("", "Sign in", "")));
    crawl(login_title, false).await.assert_error("page title: Some(\"Sign in\")");
    let not_ok = Site::healthy().with(DASHBOARD, status(500));
    crawl(not_ok, false).await.assert_error("GET /admin answered 500");
}

#[tokio::test]
async fn zero_nav_pages_fails() {
    let site = Site::healthy().with(DASHBOARD, ok(shell(&nav(&[DASHBOARD]), "Dashboard", &slot(&dashboard_slot()))));
    crawl(site, false).await.assert_failed("only the dashboard link");
}

#[tokio::test]
async fn zero_fragments_fails() {
    let site = Site::with_content(&|_| "<p>no slot</p>".to_string());
    crawl(site, false).await.assert_failed("no slot anywhere");
}

#[tokio::test]
async fn a_non_2xx_fragment_fails() {
    let failed = Response { status: 500, ..ok(fragment(Some("ok"), "Subscriptions", "")) };
    let site = Site::healthy().with(&other_mapped().slot, failed);
    crawl(site, false).await.assert_failed("a 500 fragment");
}

#[tokio::test]
async fn a_fragment_without_data_state_fails() {
    let site = Site::healthy().with(&other_mapped().slot, ok(fragment(None, "Subscriptions", "")));
    crawl(site, false).await.assert_failed("no data-state");
}

#[tokio::test]
async fn an_error_state_fragment_fails_unless_errors_are_allowed() {
    let site = Site::healthy().with(&other_mapped().slot, ok(fragment(Some("error"), "Subscriptions", "")));
    crawl(site.clone(), false).await.assert_failed("an error-state fragment");
    let allowed = crawl(site, true).await;
    assert!(matches!(allowed.result, Ok(true)), "{:?}", allowed.result);
    assert!(allowed.clean_marker(), "--allow-errors crawls clean");
}

#[tokio::test]
async fn slots_load_for_at_most_max_rounds() {
    let first = slot("/admin/_panel/chain/1");
    let chained = |depth: usize| {
        let site = Site { chain_depth: depth, ..Site::healthy() };
        site.with(&other_mapped().slot, ok(fragment(Some("ok"), "Subscriptions", &first)))
    };
    // Round 1 loads the page's slot, whose fragment holds the chain's first link; each link
    // holds the next until `depth`, so a chain of `depth` links takes `depth + 1` rounds.
    let within = crawl(chained(MAX_ROUNDS - 1), false).await;
    assert!(matches!(within.result, Ok(true)), "{MAX_ROUNDS} rounds: {:?}", within.result);
    crawl(chained(MAX_ROUNDS), false).await.assert_failed("one round more than MAX_ROUNDS");
}

/// The `players.menu.user` target: the crawl fetches the first `player` menu the players
/// rows draw, with `HX-Request`, and writes it inside a `.menu` as `players.menu.user.html`;
/// a menu answering 500 and rows that draw no `player` menu each fail the crawl, the latter
/// with `--allow-empty` too, since players lists rows. An excluded kind's menu before it in
/// the row is fetched for its status but is not the target's menu.
#[tokio::test]
async fn a_menu_target_writes_its_menu_and_fails_unless_reached() {
    let (id, page, kind) = menus().into_iter().find(|(id, _, _)| id == "players.menu.user").expect("PAGE_MAP: players.menu.user");
    let healthy = Site::healthy();
    let list_url = panel_url(page);
    let list = healthy.routes.get(&list_url).expect("the healthy site serves the players rows").body.clone();
    let button = menu_buttons(page);
    assert!(list.contains(&format!(r#"<td class="act">{button}</td>"#)), "the players rows draw the menu button: {list}");
    let guild = "/admin/_menu/guild/g-1";
    let site = healthy
        .clone()
        .with(&list_url, ok(list.replace(&button, &format!("{}{button}", menu_button(guild)))))
        .with(guild, ok(r#"<div class="menu-item"><span>guild</span></div>"#));
    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push((page, "guild", "another kind"));

    let fine = crawl_lists(site.clone(), PAGE_MAP, &excluded, false).await;
    assert!(matches!(fine.result, Ok(true)), "{:?}", fine.result);
    let fetched: Vec<Request> = fine.server.requests().into_iter().filter(|r| r.path.starts_with("/admin/_menu/")).collect();
    let url = menu_url(kind, page);
    assert_eq!(fetched.iter().filter(|r| r.path == url && r.hx).count(), 1, "{fetched:?}");
    assert_eq!(fetched.iter().filter(|r| r.path == guild && r.hx).count(), 1, "the excluded kind once: {fetched:?}");
    let written = std::fs::read_to_string(fine.out.path().join("run").join(format!("{id}.html"))).expect("the menu is written");
    assert_eq!(written, format!(r#"<div class="menu" data-menu-popup="">{MENU_BODY}</div>"#));

    crawl_lists(site.with(&url, status(500)), PAGE_MAP, &excluded, false).await.assert_failed("a 500 menu");
    let bare = healthy.with(&list_url, ok(list.replace(&button, "")));
    crawl(bare.clone(), false).await.assert_failed("players draws no player menu");
    crawl_with(bare, false, true).await.assert_failed("--allow-empty: players lists rows but draws no player menu");
}

/// A menu kind a compared, untabbed page draws must be declared: an undeclared `guild` menu on
/// a mapped page fails the crawl, and its `EXCLUDED_MENUS` twin crawls clean, fetching the
/// excluded menu once; an excluded menu answering 500 fails. The same menu on the page only
/// the nav links is no compared page's and crawls clean.
#[tokio::test]
async fn every_portal_menu_kind_on_a_compared_page_is_declared() {
    let target = other_mapped();
    let guild = "/admin/_menu/guild/g-1";
    let healthy = Site::healthy();
    let site = healthy.clone().with(target.route, add_to_page(&healthy, target.route, &menu_button(guild))).with(guild, ok(MENU_BODY));
    crawl(site.clone(), false).await.assert_failed("an undeclared guild menu");

    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push((target.page, "guild", "why"));
    let declared = crawl_lists(site.clone(), PAGE_MAP, &excluded, false).await;
    assert!(matches!(declared.result, Ok(true)), "{:?}", declared.result);
    assert_eq!(declared.server.requests().iter().filter(|r| r.path == guild && r.hx).count(), 1, "{:?}", declared.server.requests());
    let raw = declared.out.path().join("run").join("raw").join(format!("{}.excluded-menu.guild.html", target.page));
    assert!(raw.is_file(), "the excluded menu's body is kept: {}", raw.display());
    crawl_lists(site.with(guild, status(500)), PAGE_MAP, &excluded, false).await.assert_failed("an excluded menu answering 500");

    let nav_only = healthy.clone().with(EXTRA_ROUTE, add_to_page(&healthy, EXTRA_ROUTE, &menu_button(guild)));
    let c = crawl(nav_only, false).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
}

/// The production [`EXCLUDED_MENUS`] kinds are drawn by the healthy site and fetched; dropping
/// the page's button makes the exclusion stale, as does one naming a page the crawl does not
/// treat as an untabbed compared page, or one listed twice or without a reason.
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
        ("a page no untabbed entry names", EXCLUDED_MENUS.iter().copied().chain([("nosuchpage", "guild", "why")]).collect::<Vec<_>>()),
        ("listed twice", EXCLUDED_MENUS.iter().copied().chain([(host, kind, "again")]).collect()),
        ("no reason", others().chain([(host, kind, " ")]).collect()),
    ];
    for (why, excluded) in cases {
        crawl_lists(healthy.clone(), PAGE_MAP, &excluded, false).await.assert_failed(why);
    }
}

/// Menus are inventoried on untabbed pages only: a tab target whose page draws a `guild` menu
/// no list declares crawls clean, and the menu is not fetched.
#[tokio::test]
async fn a_tab_target_needs_no_menu_entry() {
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.menu.is_none()).expect("PAGE_MAP lists player");
    let mut page_map = PAGE_MAP.to_vec();
    page_map.push(Page { tab: Some("items"), ..*player });
    let route = follower_route("player");
    let tabbed = format!("{route}?tab=items");
    let healthy = Site::healthy();
    let guild = "/admin/_menu/guild/g-1";
    let tab = format!(r#"<button class="tab on" data-href="{}">items</button>{}"#, escaped(&tabbed), menu_button(guild));
    let site = healthy.clone().with(&tabbed, add_to_page(&healthy, &route, &tab));
    let c = crawl_map(site, &page_map).await;
    assert!(matches!(c.result, Ok(true)), "{:?}", c.result);
    assert!(!c.server.requests().iter().any(|r| r.path == guild), "{:?}", c.server.requests());
    let untabbed = healthy.clone().with(&route, add_to_page(&healthy, &route, &menu_button(guild)));
    crawl_map(untabbed, &page_map).await.assert_failed("the same menu on the untabbed player page");
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

/// The players page's first `tr.clickable` is followed: its escaped `data-href` is fetched as
/// a page and written as `player.html`; the plain row before it and the row after it are not.
#[tokio::test]
async fn an_entity_page_follows_the_first_clickable_row_of_its_list_page() {
    assert!(followers().contains(&("player", "players")), "PAGE_MAP: player follows players");
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
    let e = format!("{:#}", crate::run_diff(repo.path(), "player", &run, false).expect_err("no player page"));
    assert_eq!(e, format!("{} has no player page: the crawl found no row of players to follow", run.display()));
}

/// With `--allow-empty`, an exclusion on a page that lists no row, or on a page that was
/// skipped, is not stale; on a players page that lists rows, the players one is.
#[tokio::test]
async fn an_empty_or_skipped_page_cannot_make_a_menu_exclusion_stale() {
    let empty = Site::healthy().with(&panel_url("players"), ok(fragment(Some("ok"), "players panel", "")));
    let mut excluded = EXCLUDED_MENUS.to_vec();
    excluded.push(("players", "guild", "drawn in the players rows"));
    excluded.push(("player", "guild", "drawn on the player page"));
    let allowed = crawl_lists(empty.clone(), PAGE_MAP, &excluded, true).await;
    assert!(matches!(allowed.result, Ok(true)), "{:?}", allowed.result);
    assert_eq!(allowed.empty, ["player", "players.menu.user", "player.menu.user"]);
    let players_only: Vec<_> = excluded.iter().copied().filter(|e| e.0 != "player" || e.1 != "guild").collect();
    let healthy = crawl_lists(Site::healthy(), PAGE_MAP, &players_only, true).await;
    healthy.assert_failed("players lists rows and draws no guild menu");
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
    let player = PAGE_MAP.iter().find(|p| p.page == "player" && p.tab.is_none() && p.menu.is_none()).expect("PAGE_MAP lists player");
    let mut page_map = PAGE_MAP.to_vec();
    page_map.push(Page { tab: Some("items"), ..*player });
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

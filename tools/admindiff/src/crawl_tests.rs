//! `crawl` against a loopback fake portal: no Postgres, no fleet. Each failing site differs
//! from [`Site::healthy`], which crawls clean, by the one defect it names. The healthy site
//! is built from [`PAGE_MAP`], so a page added there is served too and every failing site
//! still has only its own defect.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::{run, Config, CLEAN_MARKER, MAX_ROUNDS};
use crate::pages::{Route, PAGE_MAP};
use crate::tests::ScratchDir;

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
        .filter_map(|p| match p.route {
            Route::Mapped(route) => Some(Target { page: p.page, route, slot: panel_url(p.page) }),
            Route::Pending => None,
        })
        .collect()
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
    /// links, each with one slot whose fragment is in the ok state.
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
            routes.insert(route.to_string(), ok(shell(&links, title, &content(slot_url))));
            routes.insert(slot_url.clone(), ok(fragment(Some("ok"), &format!("{page} panel"), "")));
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
    let server = Server::start(site);
    let out = ScratchDir::new("crawl");
    let cfg = Config {
        base_url: server.base_url.clone(),
        username: "admin".to_string(),
        password: "admin".to_string(),
        allow_errors,
        out: out.path().join("run"),
    };
    let result = run(&cfg).await;
    Crawl { result, out, server }
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
    let panels: Vec<Request> = c.server.requests().into_iter().filter(|r| r.path.starts_with("/admin/_panel/")).collect();
    assert_eq!(panels.len(), pages.len() + 1, "one fragment per mapped page and the nav-only page: {panels:?}");
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

#[tokio::test]
async fn a_non_2xx_row_menu_fails() {
    let menu = r#"<button class="btn sm icon ghost" data-menu-url="/admin/_menu/player/p-1"></button>"#;
    let site = Site::healthy().with(&other_mapped().slot, ok(fragment(Some("ok"), "Subscriptions", menu)));
    let fine = crawl(site.clone().with("/admin/_menu/player/p-1", ok("<div class=\"menu-group\"></div>")), false).await;
    assert!(matches!(fine.result, Ok(true)), "{:?}", fine.result);
    let menus: Vec<Request> =
        fine.server.requests().into_iter().filter(|r| r.path.starts_with("/admin/_menu/")).collect();
    assert_eq!(menus.len(), 1, "a menu is fetched once: {menus:?}");
    crawl(site.with("/admin/_menu/player/p-1", status(500)), false).await.assert_failed("a 500 row menu");
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

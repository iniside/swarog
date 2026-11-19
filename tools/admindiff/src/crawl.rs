//! `admindiff crawl`: log in, fetch every page the nav links and the page map name, load
//! each page's slots the way htmx does, and write the responses under the run directory.
//! Every declared target must be reached: a mapped page that fails, a `FirstRow` page whose
//! list has no row (unless `--allow-empty`), a tab the portal does not select and a menu
//! target whose page draws no menu of its kind all fail the crawl. A menu target's output is
//! the first such menu, the `.menu` popover the portal answers. Every menu kind an untabbed compared
//! page draws is a menu target of that page or in [`EXCLUDED_MENUS`], whose menus are fetched
//! once each and must answer 2xx; an exclusion its page does not draw fails as stale. Every
//! other crawled page, one only the nav links or a tab page, has the first menu of each kind
//! it draws fetched once, and it must answer 2xx. Every fetched menu, compared or not, fails
//! when it holds no module group or a group whose `data-state` is missing or neither `ok`
//! nor `error`, and, unless `--allow-errors`, on a group in the error state. A page and a
//! menu answer 200 whatever their state, so the root of each ([`ROOT_CLASS`], [`MENU_CLASS`])
//! carries one: `error`, while a manifest source has never answered and the portal draws none
//! of its modules, fails unless `--allow-errors`, and a root state missing or neither `ok` nor
//! `error` fails always. A slot fragment's `data-state` must be a panel state: `ok`, `empty`,
//! `readonly` and `denied` pass, `error` and `stale` (a failed provider call) fail unless
//! `--allow-errors`, and anything else fails always.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};

use crate::assemble::{assemble, pending_slots};
use crate::html::{self, find_all, find_in, Element, Node};
use crate::pages::{with_tab, Menu, Page, Route, EXCLUDED_MENUS, PAGE_MAP};
use crate::skeleton::{MENU_CLASS, ROOT_CLASS};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The panel states (`PanelState::as_str` in `core/admin`) a fragment passes in.
const PANEL_PASS: &[&str] = &["ok", "empty", "readonly", "denied"];
/// The panel states of a failed provider call, which fail unless `--allow-errors`.
const PANEL_FAIL: &[&str] = &["error", "stale"];
/// Written into the run directory only by a clean crawl; `diff` refuses a run without it.
pub const CLEAN_MARKER: &str = "CLEAN";
/// A fragment may hold slots of its own; past this many load rounds the page fails.
const MAX_ROUNDS: usize = 4;

pub struct Config {
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub allow_errors: bool,
    pub allow_empty: bool,
    pub out: PathBuf,
}

/// Runs the crawl of [`PAGE_MAP`] and [`EXCLUDED_MENUS`]; `Ok(false)` when it finished with
/// failures, which it has printed.
pub async fn run(cfg: &Config) -> Result<bool> {
    let outcome = run_with(cfg, PAGE_MAP, EXCLUDED_MENUS).await?;
    if !outcome.empty.is_empty() {
        println!("empty, nothing crawled: {}", outcome.empty.join(", "));
    }
    Ok(outcome.clean)
}

/// How a crawl finished: clean or not, and the targets it reported EMPTY.
struct Outcome {
    clean: bool,
    empty: Vec<String>,
}

/// [`run`] over the given page map and menu exclusions.
async fn run_with(cfg: &Config, page_map: &[Page], excluded_menus: &[(&str, &str, &str)]) -> Result<Outcome> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let dashboard = login(&client, cfg).await?;
    let mut failures = Vec::new();

    let mut nav = Vec::new();
    find_all(&dashboard, &|e| e.has_class("nav-item") && e.tag == "a", &mut nav);
    let mut nav_routes: Vec<String> = Vec::new();
    for a in nav {
        if let Some(href) = a.attr("href").filter(|h| *h != "/admin") {
            if !nav_routes.iter().any(|r| r == href) {
                nav_routes.push(href.to_string());
            }
        }
    }
    println!("nav: {} page(s)", nav_routes.len());
    if nav_routes.is_empty() {
        failures.push("the nav links no page".to_string());
    }

    let mut targets: Vec<(String, String)> = Vec::new();
    let mut followers: Vec<(&Page, &str)> = Vec::new();
    let mut menus: Vec<(&Page, Menu)> = Vec::new();
    for p in page_map {
        if let Some(menu) = p.menu {
            menus.push((p, menu));
            continue;
        }
        match p.route {
            Route::Mapped(route) => targets.push((p.page.to_string(), route.to_string())),
            Route::FirstRow(list) => followers.push((p, list)),
            Route::Pending => println!("PENDING {}: not ported, nothing to crawl", p.page),
        }
    }
    for route in &nav_routes {
        if !targets.iter().any(|(_, r)| r == route) {
            targets.push((slug(route), route.clone()));
        }
    }

    std::fs::create_dir_all(cfg.out.join("raw")).with_context(|| format!("create {}", cfg.out.display()))?;
    let mut fragments = 0;
    let mut pages = targets.len();
    // Each crawled target's id and assembled page, and the ids reported EMPTY.
    let mut crawled: Vec<(String, Vec<Node>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (page, route) in &targets {
        println!("page {page}  {route}");
        match crawl_page(&client, cfg, page, route, &mut failures).await {
            Ok((n, doc)) => {
                fragments += n;
                crawled.push((page.clone(), doc));
            }
            Err(e) => failures.push(format!("{page} ({route}): {e:#}")),
        }
    }
    for (entry, list) in followers {
        let page = entry.id();
        let Some((_, doc)) = crawled.iter().find(|(p, _)| p == list) else {
            failures.push(format!("{page}: its list page {list} was not crawled"));
            continue;
        };
        let Some(row) = first_row_href(doc) else {
            println!("EMPTY {page}: {list} lists no row to follow, nothing to crawl");
            if !cfg.allow_empty {
                failures.push(format!("{page}: {list} lists no row to follow (--allow-empty skips it)"));
            }
            skipped.push(page);
            continue;
        };
        let route = match entry.tab {
            Some(tab) => with_tab(&row, tab),
            None => row,
        };
        println!("page {page}  {route}  (first row of {list})");
        pages += 1;
        match crawl_page(&client, cfg, &page, &route, &mut failures).await {
            Ok((n, doc)) => {
                fragments += n;
                if let Some(tab) = entry.tab {
                    if !selects_tab(&doc, tab) {
                        failures.push(format!("{page} ({route}): the page does not select tab {tab}; the portal has no such tab"));
                    }
                }
                crawled.push((page, doc));
            }
            Err(e) => failures.push(format!("{page} ({route}): {e:#}")),
        }
    }
    for (entry, menu) in menus {
        let id = entry.id();
        let host = entry.host().id();
        if skipped.contains(&host) {
            println!("EMPTY {id}: its page {host} was skipped, nothing to crawl");
            skipped.push(id);
            continue;
        }
        let Some((_, doc)) = crawled.iter().find(|(p, _)| *p == host) else {
            failures.push(format!("{id}: its page {host} was not crawled"));
            continue;
        };
        let Some(url) = first_menu_url(doc, menu.kind) else {
            // A FirstRow host exists only because its list had a row, so it has no empty state.
            if !matches!(entry.host().route, Route::FirstRow(_)) && !lists_rows(doc) {
                println!("EMPTY {id}: {host} lists no row, nothing to crawl");
                if !cfg.allow_empty {
                    failures.push(format!("{id}: {host} lists no row and draws no {} menu (--allow-empty skips it)", menu.kind));
                }
                skipped.push(id);
                continue;
            }
            failures.push(format!("{id}: {host} draws no {} menu; the portal has no such menu", menu.kind));
            continue;
        };
        println!("menu {id}  {url}");
        if let Err(e) = crawl_menu(&client, cfg, &id, &url).await {
            failures.push(format!("{id} ({url}): {e:#}"));
        }
    }
    check_menu_kinds(&client, cfg, page_map, excluded_menus, &crawled, &mut failures).await;
    if fragments == 0 {
        failures.push("no page loaded a fragment".to_string());
    }
    println!("wrote {}", cfg.out.display());
    if failures.is_empty() {
        write(&cfg.out.join(CLEAN_MARKER), "")?;
        println!("crawl: {pages} page(s), {fragments} fragment(s), clean");
        return Ok(Outcome { clean: true, empty: skipped });
    }
    eprintln!("crawl FAILED, {} problem(s):", failures.len());
    for f in &failures {
        eprintln!("  {f}");
    }
    Ok(Outcome { clean: false, empty: skipped })
}

/// Logs in and returns the dashboard it lands on; anything but the dashboard shell fails.
async fn login(client: &Client, cfg: &Config) -> Result<Vec<Node>> {
    let resp = client
        .post(format!("{}/admin/login", cfg.base_url))
        .form(&[("username", cfg.username.as_str()), ("password", cfg.password.as_str())])
        .send()
        .await
        .context("POST /admin/login")?;
    let location = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    if resp.status() != StatusCode::SEE_OTHER || location != "/admin" {
        bail!("login failed: POST /admin/login answered {} (location {location:?}); expected 303 to /admin", resp.status());
    }
    let resp = client.get(format!("{}/admin", cfg.base_url)).send().await.context("GET /admin")?;
    let status = resp.status();
    let body = resp.text().await.context("read /admin")?;
    if status != StatusCode::OK {
        bail!("login did not land on the dashboard: GET /admin answered {status}");
    }
    let doc = html::parse(&body).context("parse /admin")?;
    let shell = find_in(&doc, &|e| e.tag == "aside" && e.has_class("sidebar")).is_some();
    let title = find_in(&doc, &|e| e.has_class("page-head")).and_then(|h| h.find(&|e| e.tag == "h1")).map(Element::text);
    if !shell || title.as_deref() != Some("Dashboard") {
        bail!("login did not land on the dashboard shell (sidebar: {shell}, page title: {title:?})");
    }
    println!("login: dashboard shell");
    Ok(doc)
}

/// The `data-href` of the first `tr.clickable` in `doc`.
fn first_row_href(doc: &[Node]) -> Option<String> {
    find_in(doc, &|e| e.tag == "tr" && e.has_class("clickable") && e.attr("data-href").is_some())
        .and_then(|tr| tr.attr("data-href"))
        .map(str::to_string)
}

/// Whether `doc` draws tab `tab` selected: a `.tab.on` whose `data-href` ends in
/// `tab=<tab>`. The portal falls back to its first tab for an id it does not know, so the
/// response status alone does not show the tab was reached.
fn selects_tab(doc: &[Node], tab: &str) -> bool {
    let want = format!("tab={tab}");
    find_in(doc, &|e| {
        e.has_class("tab")
            && e.has_class("on")
            && e.attr("data-href").is_some_and(|h| h.ends_with(&want) && h[..h.len() - want.len()].ends_with(['?', '&']))
    })
    .is_some()
}

/// The `data-menu-url` of the first element in `doc` that opens a `kind` menu.
fn first_menu_url(doc: &[Node], kind: &str) -> Option<String> {
    let prefix = format!("/admin/_menu/{kind}/");
    find_in(doc, &|e| e.attr("data-menu-url").is_some_and(|u| u.starts_with(&prefix)))
        .and_then(|e| e.attr("data-menu-url"))
        .map(str::to_string)
}

/// Each menu kind `doc` draws, once, with the first `data-menu-url` of that kind: the
/// `/admin/_menu/<kind>/` segment, as the portal writes it.
fn menu_kinds(doc: &[Node]) -> Vec<(String, String)> {
    const PREFIX: &str = "/admin/_menu/";
    let mut openers = Vec::new();
    find_all(doc, &|e| e.attr("data-menu-url").is_some_and(|u| u.starts_with(PREFIX)), &mut openers);
    let mut kinds: Vec<(String, String)> = Vec::new();
    for o in openers {
        let url = o.attr("data-menu-url").unwrap_or_default();
        let kind = url[PREFIX.len()..].split('/').next().unwrap_or_default();
        if !kinds.iter().any(|(k, _)| k == kind) {
            kinds.push((kind.to_string(), url.to_string()));
        }
    }
    kinds
}

/// Whether `doc` lists a row: a `tr` in a `tbody`. An empty portal table draws an empty
/// state instead of its body rows.
fn lists_rows(doc: &[Node]) -> bool {
    find_in(doc, &|e| e.tag == "tbody" && e.find(&|r| r.tag == "tr").is_some()).is_some()
}

/// The menu inventory of every untabbed, not pending `page_map` page the crawl reached: each
/// menu kind it draws is a menu entry of that page or an `excluded_menus` entry, not both; an
/// excluded kind's first menu is fetched and a non-2xx answer fails. Every other crawled page,
/// a page only the nav links or a tab page, needs no declaration, but the first menu of each
/// kind it draws is fetched and a non-2xx answer fails. An exclusion is listed once, with a
/// reason, on such a page, and that page draws it; a page that was not crawled
/// (`--allow-empty` skipped it, or it failed), or a page other than a `FirstRow` page that
/// lists no row under `--allow-empty`, cannot make it stale.
async fn check_menu_kinds(
    client: &Client,
    cfg: &Config,
    page_map: &[Page],
    excluded_menus: &[(&str, &str, &str)],
    crawled: &[(String, Vec<Node>)],
    failures: &mut Vec<String>,
) {
    let mut hosts: Vec<String> = Vec::new();
    let mut first_row_hosts: Vec<String> = Vec::new();
    for p in page_map.iter().filter(|p| p.menu.is_none() && p.tab.is_none() && p.route != Route::Pending) {
        if !hosts.contains(&p.id()) {
            hosts.push(p.id());
        }
        if matches!(p.route, Route::FirstRow(_)) && !first_row_hosts.contains(&p.id()) {
            first_row_hosts.push(p.id());
        }
    }
    for (page, doc) in crawled.iter().filter(|(p, _)| !hosts.contains(p)) {
        for (kind, url) in menu_kinds(doc) {
            println!("menu {page} {kind}  {url}  (status only)");
            if let Err(e) = fetch_menu(client, cfg, &format!("{page}.status-menu.{kind}"), &url).await {
                failures.push(format!("{page} {kind} menu ({url}): {e:#}"));
            }
        }
    }
    let mut drawn: Vec<(String, String)> = Vec::new();
    for host in &hosts {
        // A page that was not crawled has failed or been skipped already.
        let Some((_, doc)) = crawled.iter().find(|(p, _)| p == host) else { continue };
        for (kind, url) in menu_kinds(doc) {
            let mapped = page_map.iter().any(|p| p.menu.is_some_and(|m| m.kind == kind) && p.host().id() == *host);
            let excluded = excluded_menus.iter().any(|(h, k, _)| h == host && *k == kind);
            match (mapped, excluded) {
                (false, false) => {
                    failures.push(format!("{host} draws a {kind} menu, which is in neither PAGE_MAP nor EXCLUDED_MENUS"))
                }
                (true, true) => failures.push(format!("{host} menu {kind} is listed in PAGE_MAP and again in EXCLUDED_MENUS")),
                (false, true) => {
                    println!("menu {host} {kind}  {url}  (excluded)");
                    if let Err(e) = fetch_menu(client, cfg, &format!("{host}.excluded-menu.{kind}"), &url).await {
                        failures.push(format!("{host} excluded {kind} menu ({url}): {e:#}"));
                    }
                }
                (true, false) => {}
            }
            drawn.push((host.clone(), kind));
        }
    }
    let mut seen = BTreeSet::new();
    for (host, kind, why) in excluded_menus {
        if !seen.insert((*host, *kind)) {
            failures.push(format!("{host} menu {kind} is listed in EXCLUDED_MENUS and again in EXCLUDED_MENUS"));
        }
        if why.trim().is_empty() {
            failures.push(format!("EXCLUDED_MENUS {host} menu {kind} has no reason"));
        }
        if !hosts.iter().any(|h| h == host) {
            failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, and {host} is no untabbed PAGE_MAP page"));
            continue;
        }
        if drawn.iter().any(|(h, k)| h == host && k == kind) {
            continue;
        }
        // A page that was not crawled, skipped or failed, has nothing to compare it with.
        let Some((_, doc)) = crawled.iter().find(|(p, _)| p == host) else { continue };
        // A FirstRow host exists only because its list had a row, so it has no empty state.
        if !first_row_hosts.iter().any(|h| h == host) && !lists_rows(doc) {
            if cfg.allow_empty {
                println!("EMPTY EXCLUDED_MENUS {host} menu {kind}: {host} lists no row");
            } else {
                failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, and {host} lists no row (--allow-empty skips it)"));
            }
            continue;
        }
        failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, which {host} does not draw"));
    }
}

/// Fetches the menu at `url` as `admin.js` does and writes its body as `raw/<raw>.html`; a
/// non-2xx answer fails, and so does a body whose first element is no `.menu` root, a root
/// state that fails [`root_state`], a body with no module group (a drawn menu button always
/// has a row action, so an empty menu is template drift), a group whose `data-state` is
/// missing or neither `ok` nor `error`, and, unless `--allow-errors`, a group whose module
/// is down (`data-state="error"`): the menu answers 200 either way.
async fn fetch_menu(client: &Client, cfg: &Config, raw: &str, url: &str) -> Result<String> {
    let resp = client.get(format!("{}{url}", cfg.base_url)).header("HX-Request", "true").send().await.context("GET")?;
    let status = resp.status();
    let body = resp.text().await.context("read body")?;
    write(&cfg.out.join("raw").join(format!("{raw}.html")), &body)?;
    if !status.is_success() {
        bail!("answered {status}");
    }
    let nodes = html::parse(&body).context("parse menu")?;
    let root = nodes.iter().find_map(|n| match n {
        Node::Element(e) => Some(e),
        _ => None,
    });
    let Some(root) = root.filter(|r| r.has_class(MENU_CLASS)) else { bail!("the menu answers no .{MENU_CLASS} root") };
    root_state(cfg, root, "menu")?;
    let mut groups = Vec::new();
    find_all(&nodes, &|e| e.has_class("menu-group"), &mut groups);
    if groups.is_empty() {
        bail!("the menu holds no menu-group");
    }
    for group in groups {
        let name = group.text();
        match group.attr("data-state") {
            None => bail!("menu group {name:?} carries no data-state"),
            Some("ok") => {}
            Some("error") => {
                if !cfg.allow_errors {
                    bail!("menu group {name:?} is in the error state");
                }
            }
            Some(other) => bail!("menu group {name:?} carries data-state {other:?}, neither ok nor error"),
        }
    }
    Ok(body)
}

/// Fetches the menu at `url` ([`fetch_menu`]) and writes it as `<id>.html`: the portal
/// answers the `.menu` popover `admin.js` shows.
async fn crawl_menu(client: &Client, cfg: &Config, id: &str, url: &str) -> Result<()> {
    let body = fetch_menu(client, cfg, id, url).await?;
    write(&cfg.out.join(format!("{id}.html")), &body)
}

/// Checks the `data-state` of a page or menu root (`what`): `ok` passes, `error` only under
/// `--allow-errors`, and any other value or none fails.
fn root_state(cfg: &Config, root: &Element, what: &str) -> Result<()> {
    match root.attr("data-state") {
        Some("ok") => Ok(()),
        Some("error") if cfg.allow_errors => Ok(()),
        Some("error") => bail!("the {what} root is in the error state: a manifest source has never answered"),
        Some(other) => bail!("the {what} root carries data-state {other:?}, neither ok nor error"),
        None => bail!("the {what} root carries no data-state"),
    }
}

/// Fetches one page and its slots round by round; returns the fragment count and the
/// assembled page. A page root or fragment state that fails is pushed to `failures`.
async fn crawl_page(
    client: &Client,
    cfg: &Config,
    page: &str,
    route: &str,
    failures: &mut Vec<String>,
) -> Result<(usize, Vec<Node>)> {
    let resp = client.get(format!("{}{route}", cfg.base_url)).send().await.context("GET")?;
    let status = resp.status();
    let body = resp.text().await.context("read body")?;
    write(&cfg.out.join("raw").join(format!("{page}.html")), &body)?;
    if !status.is_success() {
        bail!("answered {status}");
    }
    let mut doc = html::parse(&body).context("parse page")?;
    match find_in(&doc, &|e| e.has_class(ROOT_CLASS)) {
        Some(root) => {
            if let Err(e) = root_state(cfg, root, "page") {
                failures.push(format!("{page}: {e}"));
            }
        }
        None => failures.push(format!("{page}: the page has no .{ROOT_CLASS} root")),
    }
    let frag_dir = cfg.out.join("fragments").join(page);
    let mut count = 0;
    for round in 1..=MAX_ROUNDS + 1 {
        let urls = pending_slots(&doc);
        if urls.is_empty() {
            break;
        }
        if round > MAX_ROUNDS {
            bail!("{} slot(s) still pending after {MAX_ROUNDS} load rounds", urls.len());
        }
        let mut parsed = Vec::with_capacity(urls.len());
        for url in &urls {
            count += 1;
            let resp = client
                .get(format!("{}{url}", cfg.base_url))
                .header("HX-Request", "true")
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            let body = resp.text().await.with_context(|| format!("read {url}"))?;
            write(&frag_dir.join(format!("{count:03}.html")), &body)?;
            let nodes = html::parse(&body).with_context(|| format!("parse {url}"))?;
            let (state, title) = describe(&nodes);
            println!("  {} {:<9} {}  {url}", status.as_u16(), state.as_deref().unwrap_or("-"), title);
            if !status.is_success() {
                failures.push(format!("{page}: {url} answered {status}"));
            } else {
                match state.as_deref() {
                    None => failures.push(format!("{page}: {url} carries no data-state")),
                    Some(s) if PANEL_PASS.contains(&s) => {}
                    Some(s) if PANEL_FAIL.contains(&s) => {
                        if !cfg.allow_errors {
                            failures.push(format!("{page}: {url} is in the {s} state"));
                        }
                    }
                    Some(other) => failures.push(format!("{page}: {url} carries data-state {other:?}, no panel state")),
                }
            }
            parsed.push(nodes);
        }
        doc = assemble(doc, parsed).context("assemble")?;
    }
    write(&cfg.out.join(format!("{page}.html")), &html::serialize(&doc))?;
    Ok((count, doc))
}

/// A fragment's `data-state` and its title: the panel title or the tile label, else `-`.
fn describe(nodes: &[Node]) -> (Option<String>, String) {
    let root = nodes.iter().find_map(|n| match n {
        Node::Element(e) if e.has_class("slot") => Some(e),
        _ => None,
    });
    let state = root.and_then(|r| r.attr("data-state")).map(str::to_string);
    let title = root
        .and_then(|r| r.find(&|e| e.has_class("panel-title") || e.has_class("kpi-label")))
        .map(Element::text)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "-".to_string());
    (state, title)
}

fn slug(route: &str) -> String {
    route.trim_start_matches("/admin").trim_matches('/').replace('/', "-")
}

fn write(path: &Path, body: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
#[path = "crawl_tests.rs"]
mod crawl_tests;

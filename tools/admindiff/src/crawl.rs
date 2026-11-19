//! `admindiff crawl`: log in, fetch every page the nav links and the page map name, load
//! each page's slots the way htmx does, and write the responses under the run directory.
//! Every declared target must be reached: a mapped page that fails, a `FirstRow` page whose
//! list has no row (unless `--allow-empty`) and a tab the portal does not select all fail
//! the crawl.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};

use crate::assemble::{assemble, pending_slots};
use crate::html::{self, find_all, find_in, Element, Node};
use crate::pages::{with_tab, Page, Route, PAGE_MAP};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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

/// Runs the crawl of [`PAGE_MAP`]; `Ok(false)` when it finished with failures, which it has
/// printed.
pub async fn run(cfg: &Config) -> Result<bool> {
    run_with(cfg, PAGE_MAP).await
}

/// [`run`] over the given page map.
async fn run_with(cfg: &Config, page_map: &[Page]) -> Result<bool> {
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
    for p in page_map {
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
    let mut crawled: Vec<(String, Vec<Node>)> = Vec::new();
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
            }
            Err(e) => failures.push(format!("{page} ({route}): {e:#}")),
        }
    }
    if fragments == 0 {
        failures.push("no page loaded a fragment".to_string());
    }
    println!("wrote {}", cfg.out.display());
    if failures.is_empty() {
        write(&cfg.out.join(CLEAN_MARKER), "")?;
        println!("crawl: {pages} page(s), {fragments} fragment(s), clean");
        return Ok(true);
    }
    eprintln!("crawl FAILED, {} problem(s):", failures.len());
    for f in &failures {
        eprintln!("  {f}");
    }
    Ok(false)
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

/// Fetches one page, its slots round by round and its row menus; returns the fragment count
/// and the assembled page.
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
                    Some("error") if !cfg.allow_errors => failures.push(format!("{page}: {url} is in the error state")),
                    _ => {}
                }
            }
            parsed.push(nodes);
        }
        doc = assemble(doc, parsed).context("assemble")?;
    }
    write(&cfg.out.join(format!("{page}.html")), &html::serialize(&doc))?;

    let mut menus = Vec::new();
    find_all(&doc, &|e| e.attr("data-menu-url").is_some(), &mut menus);
    let mut seen: Vec<&str> = Vec::new();
    for m in menus {
        let url = m.attr("data-menu-url").unwrap_or_default();
        if seen.contains(&url) {
            continue;
        }
        seen.push(url);
        let status = client.get(format!("{}{url}", cfg.base_url)).header("HX-Request", "true").send().await.map(|r| r.status());
        match status {
            Ok(s) if s.is_success() => println!("  menu {} {url}", s.as_u16()),
            Ok(s) => failures.push(format!("{page}: menu {url} answered {s}")),
            Err(e) => failures.push(format!("{page}: menu {url}: {e}")),
        }
    }
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

//! `admindiff crawl`: log in, fetch every page the nav links and the page map name, load
//! each page's slots the way htmx does, and write the responses under the run directory.
//! Every declared target must be reached: a mapped page that fails, a `FirstRow` page whose
//! list has no row (unless `--allow-empty`), a tab the portal does not select and a menu
//! target whose page draws no menu of its kind all fail the crawl. A menu target's output is
//! the first such menu, the `.menu` popover the portal answers. Every menu kind a compared page
//! or tab target draws is a menu target of that page or tab or in [`EXCLUDED_MENUS`], whose
//! menus are fetched once per target and must answer 2xx; an exclusion its page does not draw fails
//! as stale. A modal
//! target's output is the modal the portal answers for the first opener of its action its
//! page draws (an `hx-get` to `/admin/_action/<module>/<action>`, whose `?entity=`/`?item=`
//! a fleet mints); a page drawing none skips it as EMPTY, a failure unless `--allow-empty`.
//! A modal target hosted by a menu target opens the first opener of its action in that fetched
//! menu. Every modal action a compared page or tab target draws, and every one a menu target's
//! fetched menu or a compared page or tab's excluded kind draws, is a modal target of that
//! page, tab or menu or in [`EXCLUDED_MODALS`], whose modals are fetched once per target; an exclusion its page or menu
//! does not draw fails as stale, `--allow-empty` or not: an opener sits in a page head, a panel
//! tool, a row or a menu, so a page listing no row does not excuse a missing one. Every fetched
//! modal fails on a non-2xx answer, a toast, or a body that is no `.scrim` around a `.modal`.
//! Every other crawled page, one only the nav links, has the first menu of each kind and the
//! first modal of each action it and those menus draw fetched once; the menu must answer 2xx,
//! the modal as every fetched modal must. Every fetched menu, compared or not, fails
//! when it holds no module group or a group whose `data-state` is missing or neither `ok`
//! nor `error`, and, unless `--allow-errors`, on a group in the error state. A page and a
//! menu answer 200 whatever their state, so the root of each ([`ROOT_CLASS`], [`MENU_CLASS`])
//! carries one: `error`, while a manifest source has never answered and the portal draws none
//! of its modules, fails unless `--allow-errors`, and a root state missing or neither `ok` nor
//! `error` fails always. A slot fragment's `data-state` must be a panel state: `ok`, `empty`,
//! `readonly` and `denied` pass, `error` and `stale` (a failed provider call) fail unless
//! `--allow-errors`, and anything else fails always. Only a crawl with no failure and without
//! `--allow-errors` writes [`CLEAN_MARKER`]: a run with `--allow-errors` is a preview whose
//! output can be read but never diffed, so a page drawn while a provider was down can never
//! make an accept file. `--allow-empty` only skips a target, which writes no page to diff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};

use crate::assemble::{assemble, pending_slots};
use crate::html::{self, find_all, find_in, Element, Node};
use crate::pages::{compared_hosts, excluded_menu_id, validate_excluded_modals, with_tab, Lists, Menu, Modal, Page, Route, LISTS};
use crate::skeleton::{MENU_CLASS, MODAL_CLASS, ROOT_CLASS};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The panel states (`PanelState::as_str` in `core/admin`) a fragment passes in.
const PANEL_PASS: &[&str] = &["ok", "empty", "readonly", "denied"];
/// The panel states of a failed provider call, which fail unless `--allow-errors`.
const PANEL_FAIL: &[&str] = &["error", "stale"];
/// Written into the run directory only by a crawl with no failure and without `--allow-errors`;
/// `diff` refuses a run without it.
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

/// Runs the crawl of the production [`LISTS`]; `Ok(false)` when it finished with failures, which
/// it has printed.
pub async fn run(cfg: &Config) -> Result<bool> {
    let outcome = run_with(cfg, &LISTS).await?;
    if !outcome.empty.is_empty() {
        println!("empty, nothing crawled: {}", outcome.empty.join(", "));
    }
    if outcome.failures.is_empty() {
        return Ok(true);
    }
    eprintln!("crawl FAILED, {} problem(s):", outcome.failures.len());
    for f in &outcome.failures {
        eprintln!("  {f}");
    }
    Ok(false)
}

/// How a crawl finished: its failures, none for a crawl that passed, and the targets and
/// exclusions it reported EMPTY.
struct Outcome {
    failures: Vec<String>,
    empty: Vec<String>,
}

/// The menu bodies a crawl fetched, as `(<host id>, <body>)`: `compared` holds each menu
/// target's (its id, on a page or a tab) and each excluded menu's on a compared page or tab
/// ([`excluded_menu_id`]), whose modal actions are inventoried; `status` the first menu of each
/// kind any other crawled page, one only the nav links, draws (`<page>.status-menu.<kind>`), whose modals are fetched
/// for their status only.
#[derive(Default)]
struct MenuBodies {
    compared: Vec<(String, Vec<Node>)>,
    status: Vec<(String, Vec<Node>)>,
}

/// [`run`] over `lists`: its page map, menu and modal exclusions; modal exclusions that fail
/// [`validate_excluded_modals`] stop it before anything is fetched.
async fn run_with(cfg: &Config, lists: &Lists<'_>) -> Result<Outcome> {
    validate_excluded_modals(lists)?;
    let page_map = lists.page_map;
    // A reused run dir keeps a marker from an earlier clean run; drop it before this run decides.
    let marker = cfg.out.join(CLEAN_MARKER);
    match std::fs::remove_file(&marker) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("remove {}", marker.display()));
        }
        _ => {}
    }
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
    let mut modals: Vec<(&Page, Modal)> = Vec::new();
    for p in page_map {
        if let Some(modal) = p.modal {
            modals.push((p, modal));
            continue;
        }
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
    let mut menu_targets: Vec<(String, Vec<Node>)> = Vec::new();
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
        match crawl_menu(&client, cfg, &id, &url).await {
            Ok(doc) => menu_targets.push((id, doc)),
            Err(e) => failures.push(format!("{id} ({url}): {e:#}")),
        }
    }
    let mut menu_bodies = check_menu_kinds(&client, cfg, lists, &crawled, &mut failures, &mut skipped).await;
    menu_bodies.compared.extend(menu_targets);
    for (entry, modal) in modals {
        let id = entry.id();
        let host = entry.host().id();
        let action = modal.action_path();
        if skipped.contains(&host) {
            println!("EMPTY {id}: its host {host} was skipped, nothing to crawl");
            skipped.push(id);
            continue;
        }
        // A menu target's modal opens from its fetched menu, a page's from the page.
        let Some((_, doc)) = crawled.iter().chain(&menu_bodies.compared).find(|(p, _)| *p == host) else {
            failures.push(format!("{id}: its host {host} was not crawled"));
            continue;
        };
        let Some(url) = first_modal_url(doc, &action) else {
            println!("EMPTY {id}: {host} draws no {action} opener, nothing to crawl");
            if !cfg.allow_empty {
                failures.push(format!("{id}: {host} draws no {action} opener (--allow-empty skips it)"));
            }
            skipped.push(id);
            continue;
        };
        println!("modal {id}  {url}");
        if let Err(e) = crawl_modal(&client, cfg, &id, &url).await {
            failures.push(format!("{id} ({url}): {e:#}"));
        }
    }
    check_modal_actions(&client, cfg, lists, &crawled, &menu_bodies, &mut failures).await;
    if fragments == 0 {
        failures.push("no page loaded a fragment".to_string());
    }
    println!("wrote {}", cfg.out.display());
    if failures.is_empty() {
        if cfg.allow_errors {
            println!("crawl: {pages} page(s), {fragments} fragment(s), no failures; --allow-errors makes it a preview, not marked {CLEAN_MARKER}");
        } else {
            write(&cfg.out.join(CLEAN_MARKER), "")?;
            println!("crawl: {pages} page(s), {fragments} fragment(s), clean");
        }
    }
    Ok(Outcome { failures, empty: skipped })
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

/// The prefix of a portal action URL; an `hx-get` to one opens that action's modal.
const ACTION_PREFIX: &str = "/admin/_action/";

/// Each modal action `doc` draws, once, as `<module>/<action>` with the URL of its first
/// opener: every `hx-get` to [`ACTION_PREFIX`]. A `Form` or `Confirm` action's button GETs its
/// modal; an `Invoke` action's posts (`hx-post`) and a `Link`'s is an `<a href>`
/// (`primitives.html.tmpl` `hx_action`).
fn modal_actions(doc: &[Node]) -> Vec<(String, String)> {
    let mut openers = Vec::new();
    find_all(doc, &|e| e.attr("hx-get").is_some_and(|u| u.starts_with(ACTION_PREFIX)), &mut openers);
    let mut actions: Vec<(String, String)> = Vec::new();
    for o in openers {
        let url = o.attr("hx-get").unwrap_or_default();
        let action = url[ACTION_PREFIX.len()..].split('?').next().unwrap_or_default();
        if !actions.iter().any(|(a, _)| a == action) {
            actions.push((action.to_string(), url.to_string()));
        }
    }
    actions
}

/// The URL of the first opener of `action` (`<module>/<action>`) in `doc`, with the
/// `?entity=`/`?item=` the portal bound it to.
fn first_modal_url(doc: &[Node], action: &str) -> Option<String> {
    modal_actions(doc).into_iter().find(|(a, _)| a == action).map(|(_, url)| url)
}

/// Whether `doc` lists a row: a `tr` in a `tbody`. An empty portal table draws an empty
/// state instead of its body rows.
fn lists_rows(doc: &[Node]) -> bool {
    find_in(doc, &|e| e.tag == "tbody" && e.find(&|r| r.tag == "tr").is_some()).is_some()
}

/// The menu inventory of every compared host (a not pending `page_map` page or tab entry,
/// [`compared_hosts`]) the crawl reached: each menu kind it draws is a menu entry of that host
/// or an `excluded_menus` entry, not both; an excluded kind's first menu is fetched and a
/// non-2xx answer fails, and a mapped kind's is not fetched again (`run_with` fetched it as
/// its menu target). Every other crawled page, a page only the nav links, needs no declaration, but the first menu of each
/// kind it draws is fetched and a non-2xx answer fails. An exclusion is listed once, with a
/// reason, on such a page, and that page draws it; a page that was not crawled
/// (`--allow-empty` skipped it, or it failed), or a page other than a `FirstRow` page that
/// lists no row under `--allow-empty`, which reports it EMPTY in `empty`, cannot make it stale.
/// Returns the bodies it fetched: an excluded menu's as compared, any other as status only.
async fn check_menu_kinds(
    client: &Client,
    cfg: &Config,
    lists: &Lists<'_>,
    crawled: &[(String, Vec<Node>)],
    failures: &mut Vec<String>,
    empty: &mut Vec<String>,
) -> MenuBodies {
    let Lists { page_map, excluded_menus, .. } = *lists;
    let mut bodies = MenuBodies::default();
    let hosts = compared_hosts(page_map);
    // A FirstRow host, a tab entry included, exists only because its list had a row.
    let first_row_hosts: Vec<String> =
        page_map.iter().filter(|p| p.is_compared_host() && matches!(p.route, Route::FirstRow(_))).map(Page::id).collect();
    for (page, doc) in crawled.iter().filter(|(p, _)| !hosts.contains(p)) {
        for (kind, url) in menu_kinds(doc) {
            println!("menu {page} {kind}  {url}  (status only)");
            let id = format!("{page}.status-menu.{kind}");
            match fetch_menu(client, cfg, &id, &url).await {
                Ok((_, doc)) => bodies.status.push((id, doc)),
                Err(e) => failures.push(format!("{page} {kind} menu ({url}): {e:#}")),
            }
        }
    }
    let mut drawn: Vec<(String, String)> = Vec::new();
    for host in &hosts {
        // A page that was not crawled has failed or been skipped already.
        let Some((_, doc)) = crawled.iter().find(|(p, _)| p == host) else { continue };
        for (kind, url) in menu_kinds(doc) {
            let mapped = page_map.iter().any(|p| p.is_menu() && p.menu.is_some_and(|m| m.kind == kind) && p.host().id() == *host);
            let excluded = excluded_menus.iter().any(|(h, k, _)| h == host && *k == kind);
            match (mapped, excluded) {
                (false, false) => {
                    failures.push(format!("{host} draws a {kind} menu, which is in neither PAGE_MAP nor EXCLUDED_MENUS"))
                }
                (true, true) => failures.push(format!("{host} menu {kind} is listed in PAGE_MAP and again in EXCLUDED_MENUS")),
                (false, true) => {
                    println!("menu {host} {kind}  {url}  (excluded)");
                    let id = excluded_menu_id(host, &kind);
                    match fetch_menu(client, cfg, &id, &url).await {
                        Ok((_, doc)) => bodies.compared.push((id, doc)),
                        Err(e) => failures.push(format!("{host} excluded {kind} menu ({url}): {e:#}")),
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
            failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, and {host} is no PAGE_MAP page or tab that is not pending"));
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
                empty.push(excluded_menu_id(host, kind));
            } else {
                failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, and {host} lists no row (--allow-empty skips it)"));
            }
            continue;
        }
        failures.push(format!("EXCLUDED_MENUS lists {host} menu {kind}, which {host} does not draw"));
    }
    bodies
}

/// The modal inventory of every not pending page and tab entry the crawl reached and of every
/// compared menu body it fetched (`menus.compared`): each modal action one draws is a modal
/// entry hosted by it or an `excluded_modals` entry ([`validate_excluded_modals`] keeps it from
/// being both); an excluded action's first modal is fetched ([`fetch_modal`]). Every other
/// crawled page, a page only the nav links, and every status-only menu
/// (`menus.status`) needs no declaration, but the first modal of each action it draws is
/// fetched ([`fetch_modal`]) for its status. An exclusion whose host was fetched and does not
/// draw it fails, `--allow-empty` or not: an opener sits in a page head, a panel tool, a row or
/// a menu, so a page listing no row does not excuse it, unlike a row menu. A host that was not
/// fetched (`--allow-empty` skipped it, or it failed) cannot make it stale.
async fn check_modal_actions(
    client: &Client,
    cfg: &Config,
    lists: &Lists<'_>,
    crawled: &[(String, Vec<Node>)],
    menus: &MenuBodies,
    failures: &mut Vec<String>,
) {
    let Lists { page_map, excluded_modals, .. } = *lists;
    let pages = compared_hosts(page_map);
    let (compared, status): (Vec<_>, Vec<_>) = crawled.iter().partition(|(p, _)| pages.contains(p));
    let compared: Vec<&(String, Vec<Node>)> = compared.into_iter().chain(&menus.compared).collect();
    for (page, doc) in status.into_iter().chain(&menus.status) {
        for (action, url) in modal_actions(doc) {
            println!("modal {page} {action}  {url}  (status only)");
            let raw = format!("{page}.status-modal.{}", action.replace('/', "."));
            if let Err(e) = fetch_modal(client, cfg, &raw, &url).await {
                failures.push(format!("{page} {action} modal ({url}): {e:#}"));
            }
        }
    }
    let mut drawn: Vec<(String, String)> = Vec::new();
    for (host, doc) in compared.iter().copied() {
        for (action, url) in modal_actions(doc) {
            let mapped = page_map.iter().any(|p| p.modal.is_some_and(|m| m.action_path() == action) && p.host().id() == *host);
            let excluded = excluded_modals.iter().any(|(h, a, _)| h == host && *a == action);
            if excluded {
                println!("modal {host} {action}  {url}  (excluded)");
                let raw = format!("{host}.excluded-modal.{}", action.replace('/', "."));
                if let Err(e) = fetch_modal(client, cfg, &raw, &url).await {
                    failures.push(format!("{host} excluded {action} modal ({url}): {e:#}"));
                }
            } else if !mapped {
                failures.push(format!("{host} draws a {action} modal, which is in neither PAGE_MAP nor EXCLUDED_MODALS"));
            }
            drawn.push((host.clone(), action));
        }
    }
    for (host, action, _) in excluded_modals {
        if drawn.iter().any(|(h, a)| h == host && a == action) {
            continue;
        }
        // A host that was not fetched, skipped or failed, has nothing to compare it with.
        if !compared.iter().any(|(p, _)| p == host) {
            continue;
        }
        failures.push(format!("EXCLUDED_MODALS lists {host} modal {action}, which {host} does not draw"));
    }
}

/// Fetches the modal at `url` as htmx does for an opener and writes its body as
/// `raw/<raw>.html`; a non-2xx answer fails, and so does a toast in its place (the portal's
/// refusal: a first element carrying `data-toast`) and a body whose first element is no
/// `.scrim` or holds no `.modal` (`modal.html.tmpl`).
async fn fetch_modal(client: &Client, cfg: &Config, raw: &str, url: &str) -> Result<String> {
    let resp = client.get(format!("{}{url}", cfg.base_url)).header("HX-Request", "true").send().await.context("GET")?;
    let status = resp.status();
    let body = resp.text().await.context("read body")?;
    write(&cfg.out.join("raw").join(format!("{raw}.html")), &body)?;
    let nodes = html::parse(&body).context("parse modal")?;
    let root = nodes.iter().find_map(|n| match n {
        Node::Element(e) => Some(e),
        _ => None,
    });
    let toast = root.filter(|r| r.attr("data-toast").is_some()).map(|r| html::collapse(&r.text()));
    if !status.is_success() {
        match toast {
            Some(text) => bail!("answered {status}, a toast: {text:?}"),
            None => bail!("answered {status}"),
        }
    }
    if let Some(text) = toast {
        bail!("the portal answers a toast, not a modal: {text:?}");
    }
    let Some(root) = root.filter(|r| r.has_class("scrim")) else { bail!("the modal answers no .scrim root") };
    if root.find(&|e| e.has_class(MODAL_CLASS)).is_none() {
        bail!("the .scrim holds no .{MODAL_CLASS}");
    }
    Ok(body)
}

/// Fetches the modal at `url` ([`fetch_modal`]) and writes it as `<id>.html`: the `.scrim` the
/// portal swaps into its modal root.
async fn crawl_modal(client: &Client, cfg: &Config, id: &str, url: &str) -> Result<()> {
    let body = fetch_modal(client, cfg, id, url).await?;
    write(&cfg.out.join(format!("{id}.html")), &body)
}

/// Fetches the menu at `url` as `admin.js` does and writes its body as `raw/<raw>.html`; a
/// non-2xx answer fails, and so does a body whose first element is no `.menu` root, a root
/// state that fails [`root_state`], a body with no module group (a drawn menu button always
/// has a row action, so an empty menu is template drift), a group whose `data-state` is
/// missing or neither `ok` nor `error`, and, unless `--allow-errors`, a group whose module
/// is down (`data-state="error"`): the menu answers 200 either way.
async fn fetch_menu(client: &Client, cfg: &Config, raw: &str, url: &str) -> Result<(String, Vec<Node>)> {
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
    Ok((body, nodes))
}

/// Fetches the menu at `url` ([`fetch_menu`]), writes it as `<id>.html` and returns it parsed:
/// the portal answers the `.menu` popover `admin.js` shows.
async fn crawl_menu(client: &Client, cfg: &Config, id: &str, url: &str) -> Result<Vec<Node>> {
    let (body, nodes) = fetch_menu(client, cfg, id, url).await?;
    write(&cfg.out.join(format!("{id}.html")), &body)?;
    Ok(nodes)
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

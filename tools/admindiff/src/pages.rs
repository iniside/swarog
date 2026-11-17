//! Which mockup page compares with which portal route. A page is named by its mockup file
//! stem (`UILayout/redesign/<page>.html`); the file's `PAGES.boot("<key>")` names the
//! `P.<key>` builder the harness renders.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Ported: the portal route the crawl fetches and the diff compares.
    Mapped(&'static str),
    /// Ported entity page: the crawl follows the first `tr.clickable` `data-href` of the
    /// named `Mapped` list page, whose entity ids a fleet mints at random.
    FirstRow(&'static str),
    /// Not ported yet. `crawl` prints it on every run and `diff` refuses it.
    Pending,
}

#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub page: &'static str,
    pub route: Route,
    /// The harness's `location.search`; an entity page's fixed entity lives here.
    pub search: &'static str,
}

pub const PAGE_MAP: &[Page] = &[
    Page { page: "index", route: Route::Mapped("/admin"), search: "" },
    Page { page: "events", route: Route::Mapped("/admin/p/core.events/events"), search: "" },
    Page { page: "players", route: Route::Mapped("/admin/p/accounts/players"), search: "" },
    Page { page: "player", route: Route::FirstRow("players"), search: "?u=aiko.tanaka" },
    Page { page: "apikeys", route: Route::Pending, search: "" },
    Page { page: "mail", route: Route::Mapped("/admin/p/mail/outbox"), search: "" },
    Page { page: "scheduler", route: Route::Pending, search: "" },
    Page { page: "config", route: Route::Pending, search: "" },
    Page { page: "audit", route: Route::Pending, search: "" },
    Page { page: "economy", route: Route::Pending, search: "" },
];

const NO_LORE: &str = "no Lore module exists yet";

pub const EXCLUDED: &[(&str, &str)] = &[
    ("states", "Declared deviation D3: no states page; the golden render test pins every panel state"),
    ("catalog", "Declared deviation D3: no catalog page; the golden render test pins every primitive"),
    ("lore-repos", NO_LORE),
    ("lore-repo", NO_LORE),
    ("lore-permissions", NO_LORE),
    ("lore-locks", NO_LORE),
    ("lore-server", NO_LORE),
    ("lore-events", NO_LORE),
    ("lore-billing", NO_LORE),
];

/// Every mockup page under `redesign` with its `P.<key>`, read from the `PAGES.boot` call.
pub fn mockup_pages(redesign: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let dir = std::fs::read_dir(redesign).with_context(|| format!("read {}", redesign.display()))?;
    for entry in dir {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let src = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let key = boot_key(&src).with_context(|| format!("{}: no PAGES.boot(\"<key>\") call", path.display()))?;
        out.insert(stem, key);
    }
    Ok(out)
}

fn boot_key(src: &str) -> Option<String> {
    let at = src.find("PAGES.boot(\"")? + "PAGES.boot(\"".len();
    let len = src[at..].find('"')?;
    Some(src[at..at + len].to_string())
}

/// Checks [`PAGE_MAP`] and [`EXCLUDED`] against the mockup and each other: every listed
/// page exists, no page is listed twice or in both lists, every mockup page is listed, and
/// every `FirstRow` names a `Mapped` page.
pub fn validate(mockup: &BTreeMap<String, String>) -> Result<()> {
    validate_lists(mockup, PAGE_MAP, EXCLUDED)
}

/// [`validate`] over the given lists, which its problems name `PAGE_MAP` and `EXCLUDED`.
fn validate_lists(mockup: &BTreeMap<String, String>, page_map: &[Page], excluded: &[(&str, &str)]) -> Result<()> {
    let mut problems = Vec::new();
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let listed = page_map.iter().map(|p| (p.page, "PAGE_MAP")).chain(excluded.iter().map(|(p, _)| (*p, "EXCLUDED")));
    for (page, list) in listed {
        if let Some(first) = seen.insert(page, list) {
            problems.push(format!("{page} is listed in {first} and again in {list}"));
        }
        if !mockup.contains_key(page) {
            problems.push(format!("{list} lists {page}, which is not a mockup page"));
        }
    }
    for page in mockup.keys() {
        if !seen.contains_key(page.as_str()) {
            problems.push(format!("mockup page {page} is in neither PAGE_MAP nor EXCLUDED"));
        }
    }
    for p in page_map {
        if let Route::FirstRow(list) = p.route {
            if !page_map.iter().any(|q| q.page == list && matches!(q.route, Route::Mapped(_))) {
                problems.push(format!("PAGE_MAP {} follows {list}, which is not a Mapped page", p.page));
            }
        }
    }
    for (page, why) in excluded {
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED {page} has no reason"));
        }
    }
    if !problems.is_empty() {
        bail!("page map drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

pub fn lookup(page: &str) -> Result<&'static Page> {
    if let Some(p) = PAGE_MAP.iter().find(|p| p.page == page) {
        return Ok(p);
    }
    if let Some((_, why)) = EXCLUDED.iter().find(|(p, _)| *p == page) {
        bail!("page {page} is excluded: {why}");
    }
    bail!("unknown page {page}; pages: {}", PAGE_MAP.iter().map(|p| p.page).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod pages_tests;

//! Which mockup page compares with which portal route. A page is named by its mockup file
//! stem (`UILayout/redesign/<page>.html`); the file's `PAGES.boot("<key>")` names the
//! `P.<key>` builder the harness renders. An entry that names a tab of a `FirstRow` page is
//! its own target, `<page>.<tab>`, with its own crawl output and accept file. Every other tab
//! the mockup draws on a `FirstRow` page, besides the one its untabbed entry renders
//! selected, is in [`EXCLUDED_TABS`].

use std::collections::{BTreeMap, BTreeSet};
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
    /// A tab of a `FirstRow` page: both sides add `tab=<id>` to their query, and a crawl
    /// or render that does not select it fails.
    pub tab: Option<&'static str>,
}

impl Page {
    /// The target's name: `<page>`, or `<page>.<tab>` for a tab entry. It names the
    /// crawl's output, the accept file and the `--page` argument.
    pub fn id(&self) -> String {
        match self.tab {
            Some(tab) => format!("{}.{tab}", self.page),
            None => self.page.to_string(),
        }
    }

    /// The harness's `location.search`: [`Page::search`] plus the tab's `tab=<id>`.
    pub fn mock_search(&self) -> String {
        match self.tab {
            Some(tab) => with_tab(self.search, tab),
            None => self.search.to_string(),
        }
    }
}

/// `url` with `tab=<tab>` added to its query. A tab id is `[a-z0-9_-]+` ([`validate`]),
/// so it needs no encoding.
pub fn with_tab(url: &str, tab: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}tab={tab}")
}

fn is_tab_id(tab: &str) -> bool {
    !tab.is_empty() && tab.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub const PAGE_MAP: &[Page] = &[
    Page { page: "index", route: Route::Mapped("/admin"), search: "", tab: None },
    Page { page: "events", route: Route::Mapped("/admin/p/core.events/events"), search: "", tab: None },
    Page { page: "players", route: Route::Mapped("/admin/p/accounts/players"), search: "", tab: None },
    Page { page: "player", route: Route::FirstRow("players"), search: "?u=aiko.tanaka", tab: None },
    Page { page: "apikeys", route: Route::Pending, search: "", tab: None },
    Page { page: "mail", route: Route::Mapped("/admin/p/mail/outbox"), search: "", tab: None },
    Page { page: "scheduler", route: Route::Pending, search: "", tab: None },
    Page { page: "config", route: Route::Pending, search: "", tab: None },
    Page { page: "audit", route: Route::Pending, search: "", tab: None },
    Page { page: "economy", route: Route::Pending, search: "", tab: None },
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

/// Tabs the mockup draws on a `FirstRow` page that no `PAGE_MAP` tab entry diffs, as
/// `<page>.<tab>` with the reason. [`validate_tabs`] holds it to the mockup render.
pub const EXCLUDED_TABS: &[(&str, &str)] = &[
    ("player.characters", "not ported yet: the Characters tab arrives with the characters port (Step 14)"),
    ("player.wallet", "not ported yet: the Wallet tab arrives with the wallet port (Step 15)"),
    ("player.inventory", "not ported yet: the Inventory tab arrives with the inventory port (Step 16)"),
    ("player.friends", "not ported yet: the Friends tab arrives with the friends port (Step 17)"),
    ("player.groups", "not ported yet: the Groups tab arrives with the groups port (Step 18)"),
    ("player.saves", "not ported yet: the Cloud saves tab arrives with the storage port (Step 19)"),
    ("player.inbox", "not ported yet: the Inbox tab arrives with the notifications port (Step 20)"),
    ("player.lore", NO_LORE),
];

/// The tabs a mockup render draws, in order, and the one it draws selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawnTabs {
    pub tabs: Vec<String>,
    pub selected: Option<String>,
}

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
/// page exists, no target is listed twice or in both lists, every mockup page is listed,
/// every `FirstRow` names a `Mapped` page, and every tab entry has a `[a-z0-9_-]+` id and
/// an untabbed `FirstRow` entry of its page with the same route.
pub fn validate(mockup: &BTreeMap<String, String>) -> Result<()> {
    validate_lists(mockup, PAGE_MAP, EXCLUDED)
}

/// [`validate`] over the given lists, which its problems name `PAGE_MAP` and `EXCLUDED`.
pub(crate) fn validate_lists(mockup: &BTreeMap<String, String>, page_map: &[Page], excluded: &[(&str, &str)]) -> Result<()> {
    let mut problems = Vec::new();
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    let listed =
        page_map.iter().map(|p| (p.id(), p.page, "PAGE_MAP")).chain(excluded.iter().map(|(p, _)| (p.to_string(), *p, "EXCLUDED")));
    for (id, page, list) in listed {
        if let Some(first) = seen.insert(id.clone(), list) {
            problems.push(format!("{id} is listed in {first} and again in {list}"));
        }
        if !mockup.contains_key(page) {
            problems.push(format!("{list} lists {id}, which is not a mockup page"));
        }
    }
    for page in mockup.keys() {
        if !seen.contains_key(page.as_str()) && !page_map.iter().any(|p| p.page == page) {
            problems.push(format!("mockup page {page} is in neither PAGE_MAP nor EXCLUDED"));
        }
    }
    for p in page_map {
        if let Route::FirstRow(list) = p.route {
            if !page_map.iter().any(|q| q.page == list && matches!(q.route, Route::Mapped(_))) {
                problems.push(format!("PAGE_MAP {} follows {list}, which is not a Mapped page", p.page));
            }
        }
        let Some(tab) = p.tab else { continue };
        let id = p.id();
        if !is_tab_id(tab) {
            problems.push(format!("PAGE_MAP {id} names tab {tab:?}, which is not a [a-z0-9_-]+ id"));
        }
        if !matches!(p.route, Route::FirstRow(_)) {
            problems.push(format!("PAGE_MAP {id} names a tab, which only a FirstRow page has"));
        } else if !page_map.iter().any(|q| q.page == p.page && q.tab.is_none() && q.route == p.route) {
            problems.push(format!("PAGE_MAP {id} is a tab of {}, which has no untabbed entry with the same route", p.page));
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

/// Checks the tabs the mockup draws on each untabbed `FirstRow` page (`drawn`, by page)
/// against `page_map` and `excluded_tabs`, which its problems name `PAGE_MAP` and
/// `EXCLUDED_TABS`: every drawn tab but the selected one, which the untabbed entry diffs, is a
/// tab entry or excluded, not both; an exclusion names a drawn, unselected tab, once, with a
/// reason.
pub fn validate_tabs(drawn: &[(&str, DrawnTabs)], page_map: &[Page], excluded_tabs: &[(&str, &str)]) -> Result<()> {
    let mut problems = Vec::new();
    for (page, d) in drawn {
        for tab in d.tabs.iter().filter(|t| d.selected.as_ref() != Some(*t)) {
            let id = format!("{page}.{tab}");
            let mapped = page_map.iter().any(|p| p.page == *page && p.tab == Some(tab.as_str()));
            let excluded = excluded_tabs.iter().any(|(t, _)| *t == id);
            match (mapped, excluded) {
                (false, false) => problems.push(format!("mockup tab {id} is in neither PAGE_MAP nor EXCLUDED_TABS")),
                (true, true) => problems.push(format!("{id} is listed in PAGE_MAP and again in EXCLUDED_TABS")),
                _ => {}
            }
        }
    }
    // The page drawing `id`'s tab, and whether it draws that tab selected.
    let drawn_as = |id: &str| {
        drawn.iter().find_map(|(page, d)| {
            let tab = id.strip_prefix(page)?.strip_prefix('.')?;
            d.tabs.iter().any(|t| t == tab).then_some((*page, d.selected.as_deref() == Some(tab)))
        })
    };
    let mut seen = BTreeSet::new();
    for (id, why) in excluded_tabs {
        if !seen.insert(*id) {
            problems.push(format!("{id} is listed in EXCLUDED_TABS and again in EXCLUDED_TABS"));
        }
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED_TABS {id} has no reason"));
        }
        match drawn_as(id) {
            None => problems.push(format!("EXCLUDED_TABS lists {id}, which the mockup does not draw")),
            Some((page, true)) => problems.push(format!("EXCLUDED_TABS lists {id}, the tab the untabbed {page} entry diffs")),
            Some((_, false)) => {}
        }
    }
    if !problems.is_empty() {
        bail!("tab drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// The [`PAGE_MAP`] entry whose [`Page::id`] is `page`.
pub fn lookup(page: &str) -> Result<&'static Page> {
    if let Some(p) = PAGE_MAP.iter().find(|p| p.id() == page) {
        return Ok(p);
    }
    if let Some((_, why)) = EXCLUDED.iter().find(|(p, _)| *p == page) {
        bail!("page {page} is excluded: {why}");
    }
    if let Some((_, why)) = EXCLUDED_TABS.iter().find(|(t, _)| *t == page) {
        bail!("tab {page} is excluded: {why}");
    }
    bail!("unknown page {page}; pages: {}", PAGE_MAP.iter().map(Page::id).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod pages_tests;

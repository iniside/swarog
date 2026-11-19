//! `admindiff` — compares the running admin portal with the UILayout mockup.
//!
//!   admindiff crawl [--base-url <url>] [--user <name>] [--allow-errors] [--allow-empty]
//!   admindiff mock --page <page>[.<tab>]
//!   admindiff diff --page <page>[.<tab>] --portal <crawl dir> [--text]

pub mod accept;
pub mod assemble;
pub mod crawl;
pub mod html;
pub mod mock;
pub mod pages;
pub mod skeleton;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

pub const DEFAULT_BASE_URL: &str = "http://localhost:8080";
pub const DEFAULT_USER: &str = "admin";
pub const PASSWORD_ENV: &str = "ADMINDIFF_PASSWORD";
pub const DEFAULT_PASSWORD: &str = "admin";

pub const USAGE: &str = "\
admindiff — crawl the admin portal and diff its pages against the UILayout mockup

USAGE:
  admindiff crawl [--base-url <url>] [--user <name>] [--allow-errors] [--allow-empty]
  admindiff mock --page <page>[.<tab>]
  admindiff diff --page <page>[.<tab>] --portal <crawl dir> [--text]

  crawl     log in, fetch every nav page and mapped page, load their slots with HX-Request,
            fetch row menus for status; writes target/admindiff/<run>/ (and its CLEAN marker
            when clean) and fails on any non-2xx, an error-state fragment (unless
            --allow-errors), a fragment without data-state, no nav page or no fragment, and an
            entity page whose list page has no row to follow (unless --allow-empty), and a
            tab target whose page does not select the tab
  mock      render the mockup page through node into target/admindiff/mock/<page>.html
  diff      compare the page's class skeleton with the crawl's assembled page; refuses a
            crawl without its CLEAN marker, and fails on a difference not in
            tools/admindiff/accept/<page>.txt, a stale accept entry or zero matched panels;
            prints each difference as ok, NEW or STALE. A mock-only or portal-only repeat
            item (a table row, list item, ...) ends in (\"<hint>\"): its first badge's text and
            its first cell's, as \"badge · cell\". The hint is not part of the difference: an accept entry
            may keep or drop it and matches either way

  --base-url <url>   portal origin (default http://localhost:8080; split: http://localhost:8085)
  --user <name>      admin login (default admin); the password is $ADMINDIFF_PASSWORD (default admin)
  --page <page>      mockup page: the file stem of UILayout/redesign/<page>.html; <page>.<tab>
                     names a tab target of the page map, diffed against accept/<page>.<tab>.txt
  --portal <dir>     a crawl run directory
  --text             compare text too

Requires node on PATH for mock and diff.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Crawl { base_url: String, user: String, allow_errors: bool, allow_empty: bool },
    Mock { page: String },
    Diff { page: String, portal: PathBuf, text: bool },
}

pub fn parse_args(args: &[String]) -> Result<Command> {
    let Some((verb, rest)) = args.split_first() else { return Ok(Command::Help) };
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut user = DEFAULT_USER.to_string();
    let (mut allow_errors, mut allow_empty, mut text) = (false, false, false);
    let (mut page, mut portal) = (None, None);
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        let allowed: &[&str] = match verb.as_str() {
            "crawl" => &["--base-url", "--user", "--allow-errors", "--allow-empty"],
            "mock" => &["--page"],
            "diff" => &["--page", "--portal", "--text"],
            "-h" | "--help" | "help" => return Ok(Command::Help),
            other => bail!("admindiff: unknown command {other:?}\n\n{USAGE}"),
        };
        if !allowed.contains(&flag.as_str()) {
            bail!("admindiff {verb}: unknown argument {flag:?}\n\n{USAGE}");
        }
        match flag.as_str() {
            "--allow-errors" => allow_errors = true,
            "--allow-empty" => allow_empty = true,
            "--text" => text = true,
            _ => {
                let value = iter.next().ok_or_else(|| anyhow!("admindiff: {flag} requires a value"))?.clone();
                match flag.as_str() {
                    "--base-url" => base_url = value.trim_end_matches('/').to_string(),
                    "--user" => user = value,
                    "--page" => page = Some(value),
                    _ => portal = Some(PathBuf::from(value)),
                }
            }
        }
    }
    let need = |v: Option<String>, flag: &str| v.ok_or_else(|| anyhow!("admindiff {verb}: {flag} is required\n\n{USAGE}"));
    match verb.as_str() {
        "crawl" => Ok(Command::Crawl { base_url, user, allow_errors, allow_empty }),
        "mock" => Ok(Command::Mock { page: need(page, "--page")? }),
        "diff" => Ok(Command::Diff {
            page: need(page, "--page")?,
            portal: portal.ok_or_else(|| anyhow!("admindiff diff: --portal is required\n\n{USAGE}"))?,
            text,
        }),
        "-h" | "--help" | "help" => Ok(Command::Help),
        other => bail!("admindiff: unknown command {other:?}\n\n{USAGE}"),
    }
}

/// The workspace root, which holds `UILayout/` and `tools/admindiff/accept/`.
pub fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(2).unwrap_or(manifest).to_path_buf()
}

pub fn target_dir(repo: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| repo.join("target")).join("admindiff")
}

/// Every mockup page with its `P.<key>`, after checking the page map against them.
pub fn validated_mockup(repo: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let mockup = pages::mockup_pages(&repo.join("UILayout").join("redesign"))?;
    pages::validate(&mockup)?;
    Ok(mockup)
}

/// The mockup render of `entry`, parsed: `P.<key>` with the entry's search and tab; a tab
/// entry the render does not select fails.
fn render_mock(repo: &Path, key: &str, entry: &pages::Page) -> Result<(String, Vec<html::Node>)> {
    let rendered = mock::render(repo, key, &entry.mock_search())?;
    let doc = html::parse(&rendered).with_context(|| format!("parse the P.{key} render"))?;
    if let Some(tab) = entry.tab {
        mock::check_tab(&doc, key, tab)?;
    }
    Ok((rendered, doc))
}

/// The validated page map entry for `page` and its mockup `P.<key>`.
fn resolve(repo: &Path, page: &str) -> Result<(&'static pages::Page, String)> {
    let mockup = validated_mockup(repo)?;
    let entry = pages::lookup(page)?;
    let key = mockup.get(entry.page).cloned().ok_or_else(|| anyhow!("page {} has no mockup file", entry.page))?;
    Ok((entry, key))
}

pub fn run_mock(repo: &Path, page: &str) -> Result<()> {
    let (entry, key) = resolve(repo, page)?;
    let (rendered, doc) = render_mock(repo, &key, entry)?;
    let root = skeleton::prepare(&doc, skeleton::MOCK_STRIP, "mock")?;
    let shape = skeleton::page(&root, false);
    let out = target_dir(repo).join("mock").join(format!("{page}.html"));
    std::fs::create_dir_all(out.parent().expect("has a parent"))?;
    std::fs::write(&out, &rendered).with_context(|| format!("write {}", out.display()))?;
    println!("mock {page} (P.{key}): {} unit(s)", shape.units.len());
    for unit in &shape.units {
        println!("  {}", unit.label());
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// Checks [`skeleton::REPEAT`] against every mapped and pending mockup page. The mockup is
/// its side: most portal pages are not ported yet.
fn validate_repeats(repo: &Path) -> Result<()> {
    let mockup = validated_mockup(repo)?;
    let mut roots = Vec::new();
    for p in pages::PAGE_MAP {
        let key = mockup.get(p.page).ok_or_else(|| anyhow!("page {} has no mockup file", p.page))?;
        let (_, doc) = render_mock(repo, key, p)?;
        roots.push(skeleton::prepare(&doc, skeleton::MOCK_STRIP, "mock")?);
    }
    skeleton::validate_repeats(&roots)
}

/// Runs the diff; `Ok(false)` when the page fails, which it has printed.
pub fn run_diff(repo: &Path, page: &str, portal_dir: &Path, text: bool) -> Result<bool> {
    let (entry, key) = resolve(repo, page)?;
    if entry.route == pages::Route::Pending {
        bail!("page {page} is pending: it has no portal route to diff yet");
    }
    let accept_path = repo.join("tools").join("admindiff").join("accept").join(format!("{page}.txt"));
    let accept = match std::fs::read_to_string(&accept_path) {
        Ok(src) => accept::parse(&src).with_context(|| format!("{}", accept_path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(e).with_context(|| format!("read {}", accept_path.display())),
    };
    if !portal_dir.join(crawl::CLEAN_MARKER).is_file() {
        bail!("{} has no {} marker: its crawl failed or did not finish", portal_dir.display(), crawl::CLEAN_MARKER);
    }
    validate_repeats(repo)?;
    let portal_file = portal_dir.join(format!("{page}.html"));
    if let pages::Route::FirstRow(list) = entry.route {
        if !portal_file.is_file() {
            bail!("{} has no {page} page: the crawl found no row of {list} to follow", portal_dir.display());
        }
    }
    let portal_src =
        std::fs::read_to_string(&portal_file).with_context(|| format!("read {} (run crawl first)", portal_file.display()))?;
    let portal_doc = html::parse(&portal_src).with_context(|| format!("parse {}", portal_file.display()))?;
    let (_, mock_doc) = render_mock(repo, &key, entry)?;
    let mock_page = skeleton::page(&skeleton::prepare(&mock_doc, skeleton::MOCK_STRIP, "mock")?, text);
    let portal_page = skeleton::page(&skeleton::prepare(&portal_doc, skeleton::PORTAL_STRIP, "portal")?, text);
    let outcome = skeleton::diff_pages(&mock_page, &portal_page);
    let verdict = accept::judge(&outcome.lines, &accept);
    println!(
        "diff {page}: {} panel(s) and {} tile(s) matched; {} difference(s), {} accepted",
        outcome.matched_panels,
        outcome.matched_tiles,
        outcome.lines.len(),
        verdict.accepted.len()
    );
    for line in &verdict.accepted {
        println!("  ok     {}", outcome.shown(line));
    }
    for line in &verdict.unaccepted {
        println!("  NEW    {}", outcome.shown(line));
    }
    for line in &verdict.stale {
        println!("  STALE  {line}");
    }
    let mut failed = false;
    if outcome.matched_panels == 0 {
        eprintln!("diff {page} FAILED: no panel matched between the mockup and the portal");
        failed = true;
    }
    if !verdict.unaccepted.is_empty() {
        eprintln!("diff {page} FAILED: {} difference(s) not in {}", verdict.unaccepted.len(), accept_path.display());
        failed = true;
    }
    if !verdict.stale.is_empty() {
        eprintln!("diff {page} FAILED: {} stale accept entr(y/ies) match no difference", verdict.stale.len());
        failed = true;
    }
    if !failed {
        println!("diff {page}: clean");
    }
    Ok(!failed)
}

#[cfg(test)]
mod tests;

//! `admindiff` — compares the running admin portal with the UILayout mockup.
//!
//!   admindiff crawl [--base-url <url>] [--user <name>] [--allow-errors]
//!   admindiff mock --page <page>
//!   admindiff diff --page <page> --portal <crawl dir> [--text]

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
  admindiff crawl [--base-url <url>] [--user <name>] [--allow-errors]
  admindiff mock --page <page>
  admindiff diff --page <page> --portal <crawl dir> [--text]

  crawl     log in, fetch every nav page and mapped page, load their slots with HX-Request,
            fetch row menus for status; writes target/admindiff/<run>/ and fails on any
            non-2xx, an error-state fragment (unless --allow-errors), no nav page or no fragment
  mock      render the mockup page through node into target/admindiff/mock/<page>.html
  diff      compare the page's class skeleton with the crawl's assembled page; fails on a
            difference not in tools/admindiff/accept/<page>.txt or a stale accept entry

  --base-url <url>   portal origin (default http://localhost:8080; split: http://localhost:8085)
  --user <name>      admin login (default admin); the password is $ADMINDIFF_PASSWORD (default admin)
  --page <page>      mockup page: the file stem of UILayout/redesign/<page>.html
  --portal <dir>     a crawl run directory
  --text             compare text too

Requires node on PATH for mock and diff.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Crawl { base_url: String, user: String, allow_errors: bool },
    Mock { page: String },
    Diff { page: String, portal: PathBuf, text: bool },
}

pub fn parse_args(args: &[String]) -> Result<Command> {
    let Some((verb, rest)) = args.split_first() else { return Ok(Command::Help) };
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut user = DEFAULT_USER.to_string();
    let (mut allow_errors, mut text) = (false, false);
    let (mut page, mut portal) = (None, None);
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        let allowed: &[&str] = match verb.as_str() {
            "crawl" => &["--base-url", "--user", "--allow-errors"],
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
        "crawl" => Ok(Command::Crawl { base_url, user, allow_errors }),
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

/// The validated page map entry for `page` and its mockup `P.<key>`.
fn resolve(repo: &Path, page: &str) -> Result<(&'static pages::Page, String)> {
    let mockup = validated_mockup(repo)?;
    let entry = pages::lookup(page)?;
    let key = mockup.get(page).cloned().ok_or_else(|| anyhow!("page {page} has no mockup file"))?;
    Ok((entry, key))
}

pub fn run_mock(repo: &Path, page: &str) -> Result<()> {
    let (entry, key) = resolve(repo, page)?;
    let rendered = mock::render(repo, &key, entry.search)?;
    let doc = html::parse(&rendered).with_context(|| format!("parse the P.{key} render"))?;
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
    let portal_file = portal_dir.join(format!("{page}.html"));
    let portal_src =
        std::fs::read_to_string(&portal_file).with_context(|| format!("read {} (run crawl first)", portal_file.display()))?;
    let portal_doc = html::parse(&portal_src).with_context(|| format!("parse {}", portal_file.display()))?;
    let mock_doc = html::parse(&mock::render(repo, &key, entry.search)?).with_context(|| format!("parse the P.{key} render"))?;
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
    for line in &verdict.unaccepted {
        println!("  NEW    {line}");
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

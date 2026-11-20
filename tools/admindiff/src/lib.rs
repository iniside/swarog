//! `admindiff` — compares the running admin portal with the UILayout mockup.
//!
//!   admindiff crawl [--base-url <url>] [--user <name>] [--allow-errors] [--allow-empty]
//!   admindiff mock --page <target>
//!   admindiff diff --page <target> --portal <crawl dir> [--text]

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
  admindiff mock --page <target>
  admindiff diff --page <target> --portal <crawl dir> [--text]

  crawl     log in, fetch every nav page and mapped page, load their slots with HX-Request,
            fetch the first menu each menu target's page draws and the first modal of each
            modal target's action its page, or the fetched menu of a menu target hosting it,
            draws (the opener's hx-get); writes
            target/admindiff/<run>/ (and its CLEAN marker when it has no failure and runs without
            --allow-errors: a run with it is a preview diff refuses) and fails on any
            non-2xx, a page or menu whose root data-state is error (a manifest source has never
            answered) or a fragment in the error or stale state (each unless --allow-errors), a
            root data-state missing or neither ok nor error, a fragment data-state missing or no
            panel state, no nav page or no fragment, and an entity page whose list page has no
            row to follow (unless --allow-empty), a tab target whose page does not select the
            tab, a menu target whose page draws no menu of its kind (unless its list page lists
            no row and --allow-empty; an entity page never), a menu kind an untabbed compared page draws that is neither
            a page map menu entry nor in pages::EXCLUDED_MENUS, an excluded menu answering
            non-2xx, the first menu of a kind any other crawled page draws answering
            non-2xx, any fetched menu with a module group in the error state (unless
            --allow-errors), with no module group or with a group whose data-state is missing or
            neither ok nor error, and an EXCLUDED_MENUS entry its page does not draw (unless
            its page lists no row and --allow-empty; an entity page never); a modal target
            whose page draws no opener of its action (unless --allow-empty), a modal action a
            compared page or tab target draws that is neither a page map modal entry nor in
            pages::EXCLUDED_MODALS, the first modal of an action any other crawled page draws
            and any other fetched modal answering non-2xx, a toast, no .scrim root or a .scrim
            with no .modal, an EXCLUDED_MODALS entry its page does not draw (with or without
            --allow-empty) and an EXCLUDED_MODALS entry that is malformed, listed twice, mapped
            too or hosted by no compared page or tab, menu target of one or EXCLUDED_MENUS menu
            (which stops the crawl first); a tab target's modals are held to the page rules, an
            exclusion naming the tab (<page>.<tab>) as its host; every menu target's fetched
            menu, on a page or a tab, and every excluded kind's menu of an untabbed compared page
            (host <page>.excluded-menu.<kind>), is held to
            the same modal rules as a page: each modal action it draws is a page map modal entry
            of that menu target or in pages::EXCLUDED_MODALS, an excluded one is fetched, and an
            exclusion it does not draw is stale; any other fetched menu's modals are fetched for
            their status
  mock      render the mockup target through node into target/admindiff/mock/<target>.html; a
            modal target, hosted by a page, a tab or a menu, is the .scrim its page map opener
            PAGES.<opener>(<args>) appends; a
            page or tab target whose page map entry forces the ok state renders with
            UI.state.force = \"ok\", so its panels draw ok, and a menu target opens its menu
            on its host page rendered the same way; the crawl draws the portal's real state
  diff      compare the target's class skeleton with the crawl's: the assembled page, or the
            menu popover of a menu target, or the .modal of a modal target; refuses a crawl
            without its CLEAN marker (one that failed, did not finish or ran with
            --allow-errors) and a menu or modal target the crawl skipped as EMPTY, and fails
            on a difference not in tools/admindiff/accept/<target>.txt, a stale accept entry or
            a page with zero matched panels; prints each difference as ok, NEW or STALE. It
            also fails when a tab the mockup draws on an entity page is neither a page map tab
            entry nor in pages::EXCLUDED_TABS, when a menu the mockup draws on an untabbed
            compared page is not a page map menu entry, and when a menu the mockup registry
            defines is neither a page map menu entry nor in pages::EXCLUDED_MOCK_MENUS. It fails
            on the mockup's modals too: every PAGES.<fn>(...) call in an onclick of a compared page,
            tab or menu target's mockup render is run, as written, on its page in node, and one
            that throws fails by name; one that appends a .scrim is a modal opener, which must be
            a page map modal entry of that host or in pages::EXCLUDED_MOCK_MODALS (an opener drawn
            only on pending or excluded pages is not checked); a modal entry its host does not
            draw, an EXCLUDED_MOCK_MODALS entry its host does not draw, listed twice, with no
            reason or mapped too, and an EXCLUDED_MODALS NoMockOpener reason its mockup host
            contradicts (a modal opener drawn there that is no modal entry of another action
            and not in its others, a host with no compared render, or an others name that is a
            page map modal entry there) fail. A mock-only or portal-only repeat
            item (a table row, list item, ...) ends in (\"<hint>\"): its first badge's text and
            its first cell's, as \"badge · cell\". The hint is not part of the difference: an accept entry
            may keep or drop it and matches either way

  --base-url <url>   portal origin (default http://localhost:8080; split: http://localhost:8085)
  --user <name>      admin login (default admin); the password is $ADMINDIFF_PASSWORD (default admin)
  --page <target>    mockup page: the file stem of UILayout/redesign/<page>.html; <page>.<tab>
                     names a tab target of the page map, <page>[.<tab>].menu.<menu> a menu
                     target and <page>[.<tab>][.menu.<menu>].modal.<modal> a modal target, each
                     diffed against its own accept/<target>.txt
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

/// Every mockup page with its `P.<key>`, after checking the page map and page exclusions of
/// `lists` against them ([`pages::validate_lists`]).
pub fn validated_mockup(repo: &Path, lists: &pages::Lists) -> Result<std::collections::BTreeMap<String, String>> {
    let mockup = pages::mockup_pages(&repo.join("UILayout").join("redesign"))?;
    pages::validate_lists(&mockup, lists.page_map, lists.excluded)?;
    Ok(mockup)
}

/// The mockup render of `entry`'s page, parsed: `P.<key>` with the entry's search and tab,
/// forced ok when the entry forces; a tab entry the render does not select fails.
fn render_page(repo: &Path, key: &str, entry: &pages::Page) -> Result<(String, Vec<html::Node>)> {
    let rendered = mock::render(repo, key, &entry.mock_search(), entry.force)?;
    let doc = html::parse(&rendered).with_context(|| format!("parse the P.{key} render"))?;
    if let Some(tab) = entry.tab {
        mock::check_tab(&doc, key, tab)?;
    }
    Ok((rendered, doc))
}

/// The mockup render of `entry`, parsed: its page ([`render_page`]), for a menu entry the
/// menu its host page opens first ([`mock::menu_arg`]), a host that draws no such menu
/// failing, and for a modal entry the `.scrim` its opener appends, with no page rendered. A
/// menu entry's host is rendered as its `page_map` host entry is, forced ok when that forces,
/// so the menu is the one the compared page draws.
fn render_mock(repo: &Path, page_map: &[pages::Page], key: &str, entry: &pages::Page) -> Result<(String, Vec<html::Node>)> {
    // A modal is opened by its named opener, whether a page or a menu hosts it.
    if let Some(modal) = entry.modal {
        let rendered = mock::render_modal(repo, modal.opener, modal.args)?;
        let doc = html::parse(&rendered).with_context(|| format!("parse the PAGES.{} render", modal.opener))?;
        return Ok((rendered, doc));
    }
    let Some(menu) = entry.menu else { return render_page(repo, key, entry) };
    let host = entry.host();
    let host = page_map.iter().find(|q| q.hosts(&host)).ok_or_else(|| anyhow!("PAGE_MAP {} has no host entry", entry.id()))?;
    let (_, doc) = render_page(repo, key, host)?;
    let arg = mock::menu_arg(&doc, key, menu.mock)?;
    let rendered = mock::render_menu(repo, menu.mock, &arg)?;
    let doc = html::parse(&rendered).with_context(|| format!("parse the {} menu render", menu.mock))?;
    Ok((rendered, doc))
}

/// The root `entry` is compared from in a mockup render (`mock`) or a crawl output: the
/// `.menu` popover of a menu entry, the `.modal` of a modal entry, else the `.app` shell,
/// with that side's strip applied.
fn prepare(entry: &pages::Page, doc: &[html::Node], mock: bool) -> Result<html::Element> {
    let side = if mock { "mock" } else { "portal" };
    let popup = match (entry.menu, entry.modal) {
        (_, Some(_)) => Some(skeleton::MODAL_CLASS),
        (Some(_), None) => Some(skeleton::MENU_CLASS),
        (None, None) => None,
    };
    match (popup, mock) {
        (Some(class), true) => skeleton::prepare_at(doc, class, skeleton::MOCK_MENU_STRIP, side),
        (Some(class), false) => skeleton::prepare_at(doc, class, &[], side),
        (None, true) => skeleton::prepare(doc, skeleton::MOCK_STRIP, side),
        (None, false) => skeleton::prepare(doc, skeleton::PORTAL_STRIP, side),
    }
}

/// The page map entry of `lists` for `page` and its mockup `P.<key>` in `mockup`, which
/// [`validated_mockup`] checked `lists` against.
fn resolve<'m>(mockup: &std::collections::BTreeMap<String, String>, lists: &pages::Lists<'m>, page: &str) -> Result<(&'m pages::Page, String)> {
    let entry = pages::lookup(lists, page)?;
    let key = mockup.get(entry.page).cloned().ok_or_else(|| anyhow!("page {} has no mockup file", entry.page))?;
    Ok((entry, key))
}

pub fn run_mock(repo: &Path, page: &str) -> Result<()> {
    let lists = pages::LISTS;
    let mockup = validated_mockup(repo, &lists)?;
    let (entry, key) = resolve(&mockup, &lists, page)?;
    let (rendered, doc) = render_mock(repo, lists.page_map, &key, entry)?;
    let root = prepare(entry, &doc, true)?;
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

/// Checks the mockup render of every page map entry of `lists` against the lists that name its
/// parts: [`skeleton::REPEAT`] against all of them, the tabs each untabbed `FirstRow` page draws
/// against the page map's tab entries and `excluded_tabs` ([`pages::validate_tabs`]), and the
/// menus the registry defines and each untabbed page that is not pending draws against the
/// page map's menu entries and `excluded_mock_menus` ([`pages::validate_menus`]); a menu
/// entry's render fails when its page draws no such menu, and a modal entry's, a page's or a
/// menu's, when its opener is no function or appends no `.scrim`. The modal openers each page,
/// tab and menu target that is not pending draws (every `onclick` `PAGES.<fn>(…)` its render
/// draws, run in the harness on its page, [`mock::modal_openers`]; one that throws fails) are
/// checked against the page map's modal entries, `excluded_mock_modals` and the mockup facts of
/// `excluded_modals` ([`pages::validate_mock_modals`]). `mockup` is what [`validated_mockup`]
/// returned for `lists`. The mockup is the side checked: most portal pages are not ported yet.
fn validate_renders(repo: &Path, lists: &pages::Lists, mockup: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let page_map = lists.page_map;
    let mut roots = Vec::new();
    let mut drawn = Vec::new();
    let mut menus = Vec::new();
    let mut openers = Vec::new();
    for p in page_map {
        let key = mockup.get(p.page).ok_or_else(|| anyhow!("page {} has no mockup file", p.page))?;
        let (_, doc) = render_mock(repo, page_map, key, p)?;
        if p.route != pages::Route::Pending && p.modal.is_none() {
            // A menu's openers run on its host page, rendered as its host entry is.
            let page = if p.is_page() { p } else { page_map.iter().find(|q| q.hosts(&p.host())).ok_or_else(|| anyhow!("PAGE_MAP {} has no host entry", p.id()))? };
            let drawn = mock::drawn_openers(&doc).with_context(|| format!("the {} mockup render", p.id()))?;
            openers.push((p.id(), key, page, drawn));
        }
        if p.is_page() {
            if matches!(p.route, pages::Route::FirstRow(_)) && p.tab.is_none() {
                drawn.push((p.page, mock::drawn_tabs(&doc).with_context(|| format!("the P.{key} render"))?));
            }
            if p.route != pages::Route::Pending && p.tab.is_none() {
                menus.push((p.id(), mock::drawn_menus(&doc).with_context(|| format!("the P.{key} render"))?));
            }
        }
        roots.push(prepare(p, &doc, true)?);
    }
    skeleton::validate_repeats(&roots)?;
    pages::validate_tabs(&drawn, page_map, lists.excluded_tabs)?;
    pages::validate_menus(&menus, &mock::defined_menus(repo)?, page_map, lists.excluded_mock_menus)?;
    let mut modals = Vec::new();
    for (host, key, page, drawn) in openers {
        let found = mock::modal_openers(repo, &host, key, &page.mock_search(), page.force, &drawn)?;
        modals.push((host, found));
    }
    pages::validate_mock_modals(&modals, lists)
}

/// Runs the diff of target `page` of `lists` (the CLI passes [`pages::LISTS`]); `Ok(false)`
/// when the page fails, which it has printed.
pub fn run_diff(repo: &Path, lists: &pages::Lists, page: &str, portal_dir: &Path, text: bool) -> Result<bool> {
    let mockup = validated_mockup(repo, lists)?;
    let (entry, key) = resolve(&mockup, lists, page)?;
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
        bail!("{} has no {} marker: its crawl failed, did not finish or ran with --allow-errors", portal_dir.display(), crawl::CLEAN_MARKER);
    }
    validate_renders(repo, lists, &mockup)?;
    let portal_file = portal_dir.join(format!("{page}.html"));
    if !portal_file.is_file() {
        let host = entry.host().id();
        if let Some(modal) = entry.modal {
            let action = modal.action_path();
            let dir = portal_dir.display();
            bail!("{dir} has no {page} modal: the crawl skipped it as EMPTY ({host} was skipped or drew no {action} opener) or predates the entry");
        }
        if let Some(menu) = entry.menu {
            let (dir, kind) = (portal_dir.display(), menu.kind);
            bail!("{dir} has no {page} menu: the crawl skipped it as EMPTY ({host} was skipped or drew no {kind} menu) or predates the entry");
        }
        if let pages::Route::FirstRow(list) = entry.route {
            bail!("{} has no {page} page: the crawl found no row of {list} to follow", portal_dir.display());
        }
    }
    let portal_src =
        std::fs::read_to_string(&portal_file).with_context(|| format!("read {} (run crawl first)", portal_file.display()))?;
    let portal_doc = html::parse(&portal_src).with_context(|| format!("parse {}", portal_file.display()))?;
    let (_, mock_doc) = render_mock(repo, lists.page_map, &key, entry)?;
    let mock_page = skeleton::page(&prepare(entry, &mock_doc, true)?, text);
    let portal_page = skeleton::page(&prepare(entry, &portal_doc, false)?, text);
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
    // A menu holds no panel; a page that matches none compared nothing.
    if outcome.matched_panels == 0 && entry.is_page() {
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

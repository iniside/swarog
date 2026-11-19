//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.
//! A menu is rendered the same way: `UI.openMenu` builds it and the `.menu` element it
//! appends to the body is captured.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::html::{find_all, find_in, Node};
use crate::pages::DrawnTabs;

/// The scripts in the order the mockup's html files load them; `catalog.js` only builds
/// the excluded catalog page.
const SCRIPTS: &[&str] = &[
    "UILayout/redesign/assets/data.js",
    "UILayout/design-system/components/bundle.js",
    "UILayout/redesign/assets/boot.js",
    "UILayout/redesign/assets/registry.js",
    "UILayout/redesign/assets/pages-core.js",
    "UILayout/redesign/assets/pages-lore.js",
    "UILayout/redesign/assets/pages-platform.js",
];

// `REG.render` assigns `document.body.innerHTML` right after `P.<key>.render()`; the setter
// throws a sentinel there, so the legend fill-in and the page's `after` hook never run. With
// `input.menu` no page mounts: `UI.openMenu` runs against a stub event and anchor, and the
// element it appends to the body is written as `<div class="menu">` around its HTML.
const HARNESS: &str = r#""use strict";
const fs = require("fs"), vm = require("vm");
const input = JSON.parse(process.env.ADMINDIFF_INPUT);
const CAPTURED = Symbol("captured");
let html = null;
const inert = () => {};
const element = () => ({
  style: {}, dataset: {}, children: [],
  classList: { add: inert, remove: inert, toggle: inert, contains: () => false },
  addEventListener: inert, appendChild: inert, remove: inert, setAttribute: inert, focus: inert,
  querySelector: () => null, querySelectorAll: () => [],
});
const body = element();
Object.defineProperty(body, "innerHTML", {
  get: () => html ?? "",
  set: v => { html = String(v); throw CAPTURED; },
});
globalThis.window = globalThis;
globalThis.document = {
  addEventListener: inert, querySelector: () => null, querySelectorAll: () => [],
  createElement: element, body, documentElement: { dataset: {} }, title: "",
};
globalThis.localStorage = { getItem: () => null, setItem: inert, removeItem: inert };
globalThis.matchMedia = () => ({ matches: false, addEventListener: inert });
globalThis.location = { search: input.search, href: "" };
globalThis.innerWidth = 1280;
globalThis.innerHeight = 800;
for (const file of input.scripts) vm.runInThisContext(fs.readFileSync(file, "utf8"), { filename: file });
if (input.menu) {
  let opened = null;
  body.appendChild = el => { opened = el; };
  const anchor = { getBoundingClientRect: () => ({ left: 0, right: 0, top: 0, bottom: 0 }) };
  UI.openMenu({ stopPropagation: inert, currentTarget: anchor }, input.menu.id, input.menu.arg);
  if (opened === null || opened.className !== "menu") {
    process.stderr.write(`UI.openMenu('${input.menu.id}') appended no .menu element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="menu">${opened.innerHTML ?? ""}</div>`);
} else {
  const page = PAGES[input.key];
  if (!page || typeof page.render !== "function") {
    process.stderr.write(`PAGES.${input.key} has no render()\n`);
    process.exit(2);
  }
  try { REG.mount(page.meta, page.render); } catch (e) { if (e !== CAPTURED) throw e; }
  if (html === null) {
    process.stderr.write("REG.mount returned without assigning document.body.innerHTML\n");
    process.exit(2);
  }
  process.stdout.write(html);
}
"#;

/// The body HTML the mockup page `key` renders with `search` as `location.search`.
pub fn render(repo: &Path, key: &str, search: &str) -> Result<String> {
    run_harness(repo, serde_json::json!({ "key": key, "search": search }), &format!("P.{key}"))
}

/// The `.menu` element `UI.openMenu(event, '<id>', '<arg>')` opens in the mockup.
pub fn render_menu(repo: &Path, id: &str, arg: &str) -> Result<String> {
    let input = serde_json::json!({ "search": "", "menu": { "id": id, "arg": arg } });
    run_harness(repo, input, &format!("menu {id}"))
}

/// Runs [`HARNESS`] with `input` plus the script list; `what` names the render in errors.
fn run_harness(repo: &Path, mut input: serde_json::Value, what: &str) -> Result<String> {
    let scripts: Vec<PathBuf> = SCRIPTS.iter().map(|s| repo.join(s)).collect();
    for script in &scripts {
        if !script.is_file() {
            bail!("mockup script {} is missing", script.display());
        }
    }
    input["scripts"] = serde_json::json!(scripts);
    let mut child = match Command::new("node")
        .arg("-")
        .env("ADMINDIFF_INPUT", input.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("`node` is not on PATH; it is a dev prerequisite for admindiff (docs/reference/testing.md)")
        }
        Err(e) => return Err(e).context("spawn node"),
    };
    child.stdin.take().expect("stdin is piped").write_all(HARNESS.as_bytes()).context("write the harness to node")?;
    let out = child.wait_with_output().context("wait for node")?;
    if !out.status.success() {
        bail!("node failed rendering {what} ({}):\n{}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    String::from_utf8(out.stdout).context("node output is not UTF-8")
}

/// Fails unless the rendered mockup `doc` selects tab `tab`: a `.tab.on` whose `onclick`
/// sets `PAGES.tab='<tab>'`. The mockup falls back to its first tab for an id it does not
/// know, so a render alone does not show the tab was reached.
pub fn check_tab(doc: &[Node], key: &str, tab: &str) -> Result<()> {
    let want = format!("PAGES.tab='{tab}'");
    let on = find_in(doc, &|e| e.has_class("tab") && e.has_class("on") && e.attr("onclick").is_some_and(|c| c.contains(&want)));
    if on.is_none() {
        bail!("P.{key} does not select tab {tab}; the mockup has no such tab");
    }
    Ok(())
}

/// The `UI.openMenu` call that opens a menu in the mockup, up to its id.
const OPEN_MENU: &str = "UI.openMenu(event,'";

/// The argument of the first menu `menu` the rendered mockup `doc` draws: the first element
/// whose `onclick` calls `UI.openMenu(event,'<menu>','<arg>')`. A page that draws none fails,
/// as the crawl fails a portal page that draws no such menu.
pub fn menu_arg(doc: &[Node], key: &str, menu: &str) -> Result<String> {
    let call = format!("{OPEN_MENU}{menu}','");
    let Some(opener) = find_in(doc, &|e| e.attr("onclick").is_some_and(|c| c.contains(&call))) else {
        bail!("P.{key} draws no {menu} menu; the mockup has no such menu");
    };
    let onclick = opener.attr("onclick").unwrap_or_default();
    let at = onclick.find(&call).map_or(0, |i| i + call.len());
    let Some(len) = onclick[at..].find('\'') else { bail!("a menu's onclick {onclick:?} does not close its argument") };
    Ok(onclick[at..at + len].to_string())
}

/// The ids of the menus the rendered mockup `doc` draws, once each, in order: every
/// `UI.openMenu(event,'<id>',…)` in an `onclick`.
pub fn drawn_menus(doc: &[Node]) -> Result<Vec<String>> {
    let mut openers = Vec::new();
    find_all(doc, &|e| e.attr("onclick").is_some_and(|c| c.contains(OPEN_MENU)), &mut openers);
    let mut ids: Vec<String> = Vec::new();
    for o in openers {
        let onclick = o.attr("onclick").unwrap_or_default();
        let at = onclick.find(OPEN_MENU).map_or(0, |i| i + OPEN_MENU.len());
        let Some(len) = onclick[at..].find('\'') else { bail!("a menu's onclick {onclick:?} does not close its menu id") };
        let id = &onclick[at..at + len];
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

/// The tabs the rendered mockup `doc` draws: every `.tab` whose `onclick` sets
/// `PAGES.tab='<id>'`, in order, and the first of them drawn `.on`.
pub fn drawn_tabs(doc: &[Node]) -> Result<DrawnTabs> {
    const SET: &str = "PAGES.tab='";
    let mut buttons = Vec::new();
    find_all(doc, &|e| e.has_class("tab") && e.attr("onclick").is_some_and(|c| c.contains(SET)), &mut buttons);
    let mut drawn = DrawnTabs::default();
    for b in buttons {
        let onclick = b.attr("onclick").unwrap_or_default();
        let at = onclick.find(SET).map_or(0, |i| i + SET.len());
        let Some(len) = onclick[at..].find('\'') else { bail!("a tab's onclick {onclick:?} does not close its PAGES.tab id") };
        let id = onclick[at..at + len].to_string();
        if b.has_class("on") && drawn.selected.is_none() {
            drawn.selected = Some(id.clone());
        }
        drawn.tabs.push(id);
    }
    Ok(drawn)
}

#[cfg(test)]
#[path = "mock_tests.rs"]
pub(crate) mod mock_tests;

//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.

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
// throws a sentinel there, so the legend fill-in and the page's `after` hook never run.
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
for (const file of input.scripts) vm.runInThisContext(fs.readFileSync(file, "utf8"), { filename: file });
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
"#;

/// The body HTML the mockup page `key` renders with `search` as `location.search`.
pub fn render(repo: &Path, key: &str, search: &str) -> Result<String> {
    let scripts: Vec<PathBuf> = SCRIPTS.iter().map(|s| repo.join(s)).collect();
    for script in &scripts {
        if !script.is_file() {
            bail!("mockup script {} is missing", script.display());
        }
    }
    let input = serde_json::json!({ "key": key, "search": search, "scripts": scripts });
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
        bail!("node failed rendering P.{key} ({}):\n{}", out.status, String::from_utf8_lossy(&out.stderr).trim());
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

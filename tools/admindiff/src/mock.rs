//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.
//! A menu is rendered the same way: `UI.openMenu` builds it and the `.menu` element it
//! appends to the body is captured. A modal is rendered without a page too: the named
//! `PAGES.<opener>` runs with the page map entry's arguments and the `.scrim` element it
//! appends to the body is captured. The harness also records every menu id the mockup's
//! registry defines through `UI.defineMenu` ([`defined_menus`]). A page render can force a
//! panel state: the harness sets `UI.state.force`, as the mockup's state picker
//! (`REG.setForce`) does, before `REG.mount`; the states the mockup knows are the labels of
//! its Panel states page ([`panel_states`]).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::html::{find_all, find_in, parse, Node};
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
// element it appends to the body is written as `<div class="menu">` around its HTML. With
// `input.modal` no page mounts either: `PAGES[opener]` is called with the entry's arguments,
// and the first element it appends to the body whose class is `scrim` (`UI.modal`, which
// `UI.confirm` calls) is written as `<div class="scrim">` around its HTML. The
// global `UI` is an accessor that wraps the `defineMenu` of whatever object a script assigns
// to it, before any script can call it, so every defined menu id is recorded; with
// `input.registry` no page mounts and the ids are written as JSON. With `input.force` the page
// render sets `UI.state.force` before it mounts, after `boot.js` has read `?state=`, so the
// entry's force is the state every forceable panel and tile draws in.
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
const defined = [];
let ui;
Object.defineProperty(globalThis, "UI", {
  configurable: true, enumerable: true,
  get: () => ui,
  set: v => {
    ui = v;
    const define = v && v.defineMenu;
    if (typeof define === "function") {
      v.defineMenu = function (id, ...rest) { defined.push(String(id)); return define.call(this, id, ...rest); };
    }
  },
});
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
if (input.registry) {
  process.stdout.write(JSON.stringify(defined));
} else if (input.menu) {
  let opened = null;
  body.appendChild = el => { opened = el; };
  const anchor = { getBoundingClientRect: () => ({ left: 0, right: 0, top: 0, bottom: 0 }) };
  UI.openMenu({ stopPropagation: inert, currentTarget: anchor }, input.menu.id, input.menu.arg);
  if (opened === null || opened.className !== "menu") {
    process.stderr.write(`UI.openMenu('${input.menu.id}') appended no .menu element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="menu">${opened.innerHTML ?? ""}</div>`);
} else if (input.modal) {
  const open = PAGES[input.modal.opener];
  if (typeof open !== "function") {
    process.stderr.write(`PAGES.${input.modal.opener} is not a function\n`);
    process.exit(2);
  }
  const appended = [];
  body.appendChild = el => { appended.push(el); };
  open(...input.modal.args);
  const scrim = appended.find(el => el && el.className === "scrim");
  if (!scrim) {
    process.stderr.write(`PAGES.${input.modal.opener}() appended no .scrim element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="scrim">${scrim.innerHTML ?? ""}</div>`);
} else {
  const page = PAGES[input.key];
  if (!page || typeof page.render !== "function") {
    process.stderr.write(`PAGES.${input.key} has no render()\n`);
    process.exit(2);
  }
  if (input.force != null) {
    if (!UI.state || typeof UI.state !== "object") {
      process.stderr.write(`the mockup has no UI.state to force ${input.force} on\n`);
      process.exit(2);
    }
    UI.state.force = input.force;
  }
  try { REG.mount(page.meta, page.render); } catch (e) { if (e !== CAPTURED) throw e; }
  if (html === null) {
    process.stderr.write("REG.mount returned without assigning document.body.innerHTML\n");
    process.exit(2);
  }
  process.stdout.write(html);
}
"#;

/// The body HTML the mockup page `key` renders with `search` as `location.search`, and with
/// `UI.state.force` set to `force` when it is given.
pub fn render(repo: &Path, key: &str, search: &str, force: Option<&str>) -> Result<String> {
    let what = match force {
        Some(force) => format!("P.{key} forced {force}"),
        None => format!("P.{key}"),
    };
    run_harness(repo, serde_json::json!({ "key": key, "search": search, "force": force }), &what)
}

/// The mockup's Panel states page, `P.states` (`states.html`), which draws a panel in each
/// state the panel chrome owns under a `.cat-state-label` naming it.
const STATES_KEY: &str = "states";

/// The panel states the mockup knows, once each, in order: the `.cat-state-label` texts its
/// Panel states page draws. A page map entry may force only one of them. A page that labels
/// none fails: the harness no longer reads the mockup's states.
pub fn panel_states(repo: &Path) -> Result<Vec<String>> {
    let rendered = render(repo, STATES_KEY, "", None)?;
    let doc = parse(&rendered).with_context(|| format!("parse the P.{STATES_KEY} render"))?;
    let mut labels = Vec::new();
    find_all(&doc, &|e| e.has_class("cat-state-label"), &mut labels);
    let mut states: Vec<String> = Vec::new();
    for label in labels {
        let state = label.text().trim().to_string();
        if !states.contains(&state) {
            states.push(state);
        }
    }
    if states.is_empty() {
        bail!("P.{STATES_KEY} labels no panel state: the harness reads no .cat-state-label");
    }
    Ok(states)
}

/// The `.menu` element `UI.openMenu(event, '<id>', '<arg>')` opens in the mockup.
pub fn render_menu(repo: &Path, id: &str, arg: &str) -> Result<String> {
    let input = serde_json::json!({ "search": "", "menu": { "id": id, "arg": arg } });
    run_harness(repo, input, &format!("menu {id}"))
}

/// The `.scrim` element `PAGES.<opener>(<args>)` appends in the mockup, its modal inside.
pub fn render_modal(repo: &Path, opener: &str, args: &[&str]) -> Result<String> {
    let input = serde_json::json!({ "search": "", "modal": { "opener": opener, "args": args } });
    run_harness(repo, input, &format!("PAGES.{opener}"))
}

/// Every menu id the mockup's registry defines through `UI.defineMenu`, once each, in order.
/// A registry that defines none fails: the harness no longer sees its `UI.defineMenu` calls.
pub fn defined_menus(repo: &Path) -> Result<Vec<String>> {
    let out = run_harness(repo, serde_json::json!({ "search": "", "registry": true }), "the menu registry")?;
    let all: Vec<String> = serde_json::from_str(&out).context("the harness's menu registry is not a JSON string list")?;
    let mut ids: Vec<String> = Vec::new();
    for id in all {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        bail!("the mockup defines no menu: the harness recorded no UI.defineMenu call");
    }
    Ok(ids)
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

//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.
//! A menu is rendered the same way: `UI.openMenu` builds it and the `.menu` element it
//! appends to the body is captured. A modal is rendered without a page too: the named
//! `PAGES.<opener>` runs with the page map entry's arguments and the `.scrim` element it
//! appends to the body is captured. The harness also records every menu id the mockup's
//! registry defines through `UI.defineMenu` ([`defined_menus`]). A page render can force the
//! ok state: the harness sets `UI.state.force = "ok"`, as the mockup's state picker
//! (`REG.setForce`) does, before `REG.mount`. The mockup's modal inventory comes from the
//! renders too: [`drawn_openers`] reads every `PAGES.<fn>(…)` call in a render's `onclick`s, and
//! [`modal_openers`] runs each, as written, on its page mounted in the harness and keeps the
//! ones that append a `.scrim`; an opener that throws fails by name.

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
// element it appends to the body is written as `<div class="menu">` around its HTML. With
// `input.modal` no page mounts either: `PAGES[opener]` is called with the entry's arguments,
// and the first element it appends to the body whose class is `scrim` (`UI.modal`, which
// `UI.confirm` calls) is written as `<div class="scrim">` around its HTML. The
// global `UI` is an accessor that wraps the `defineMenu` of whatever object a script assigns
// to it, before any script can call it, so every defined menu id is recorded; with
// `input.registry` no page mounts and the ids are written as JSON. With `input.force` the page
// render sets `UI.state.force = "ok"` before it mounts, after `boot.js` has read `?state=`, so
// every forceable panel and tile draws ok. With `input.probe` the page mounts as for a render,
// then each call in the list runs as an `onclick` body (`this` a stub element, `event` a stub
// event) and is written as `{modal}`, whether it appended a `.scrim` to the body, or as
// `{error}` when it threw; a call that re-renders the page throws the sentinel, which is no
// error. The calls share the one mounted page, in order.
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
  mount();
  if (input.probe) {
    const anchor = { getBoundingClientRect: () => ({ left: 0, right: 0, top: 0, bottom: 0 }) };
    const results = [];
    for (const call of input.probe) {
      const appended = [];
      body.appendChild = el => { appended.push(el); };
      let error = null;
      try {
        new Function("event", call).call(element(), { stopPropagation: inert, preventDefault: inert, currentTarget: anchor, target: anchor });
      } catch (e) {
        if (e !== CAPTURED) error = String(e && e.message !== undefined ? e.message : e);
      }
      results.push(error === null ? { modal: appended.some(el => el && el.className === "scrim") } : { error });
    }
    process.stdout.write(JSON.stringify(results));
  } else {
    process.stdout.write(html);
  }
}
function mount() {
  const page = PAGES[input.key];
  if (!page || typeof page.render !== "function") {
    process.stderr.write(`PAGES.${input.key} has no render()\n`);
    process.exit(2);
  }
  if (input.force) {
    if (!UI.state || typeof UI.state !== "object") {
      process.stderr.write("the mockup has no UI.state to force ok on\n");
      process.exit(2);
    }
    UI.state.force = "ok";
  }
  try { REG.mount(page.meta, page.render); } catch (e) { if (e !== CAPTURED) throw e; }
  if (html === null) {
    process.stderr.write("REG.mount returned without assigning document.body.innerHTML\n");
    process.exit(2);
  }
}
"#;

/// The body HTML the mockup page `key` renders with `search` as `location.search`, and with
/// `UI.state.force = "ok"` when `force`.
pub fn render(repo: &Path, key: &str, search: &str, force: bool) -> Result<String> {
    let what = if force { format!("P.{key} forced ok") } else { format!("P.{key}") };
    run_harness(repo, serde_json::json!({ "key": key, "search": search, "force": force }), &what)
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

/// A `PAGES.<name>(…)` call a mockup render draws in an `onclick`, as written there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opener {
    pub name: String,
    pub call: String,
}

/// Every `PAGES.<name>(…)` call in an `onclick` of the rendered mockup `doc`, once each by its
/// text, in order. A call is cut at its closing parenthesis, outside the string literals of its
/// arguments; one that never closes fails. `PAGES.<name>=…` (a tab or filter switch) is no call.
pub fn drawn_openers(doc: &[Node]) -> Result<Vec<Opener>> {
    const PREFIX: &str = "PAGES.";
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut elements = Vec::new();
    find_all(doc, &|e| e.attr("onclick").is_some_and(|c| c.contains(PREFIX)), &mut elements);
    let mut openers: Vec<Opener> = Vec::new();
    for e in elements {
        let onclick = e.attr("onclick").unwrap_or_default();
        let bytes = onclick.as_bytes();
        let mut from = 0;
        while let Some(at) = onclick[from..].find(PREFIX).map(|i| from + i) {
            from = at + PREFIX.len();
            if at > 0 && (ident(bytes[at - 1]) || bytes[at - 1] == b'.') {
                continue;
            }
            let name_end = from + bytes[from..].iter().take_while(|b| ident(**b)).count();
            if name_end == from || bytes.get(name_end) != Some(&b'(') {
                continue;
            }
            let Some(close) = closing_paren(&onclick[name_end..]) else {
                bail!("an onclick {onclick:?} does not close its {} call", &onclick[at..name_end]);
            };
            let call = onclick[at..name_end + close + 1].to_string();
            from = name_end + close + 1;
            if !openers.iter().any(|o| o.call == call) {
                openers.push(Opener { name: onclick[at + PREFIX.len()..name_end].to_string(), call });
            }
        }
    }
    Ok(openers)
}

/// The offset of the `)` that closes the `(` `s` starts with, skipping single-quoted,
/// double-quoted and backtick string literals and their escapes; `None` when it never closes.
fn closing_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (i, b) in s.bytes().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => quote = Some(b),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The names of the `openers` a mockup render of host `host` draws that open a modal: each call
/// runs, as written, on `P.<key>` mounted with `search` and forced ok when `force` (the
/// render that drew it, or for a menu its host page's), and a call that appends a `.scrim` is
/// a modal opener. Once each, in order. A call that throws fails with its name and text.
pub fn modal_openers(repo: &Path, host: &str, key: &str, search: &str, force: bool, openers: &[Opener]) -> Result<Vec<String>> {
    if openers.is_empty() {
        return Ok(Vec::new());
    }
    let calls: Vec<&str> = openers.iter().map(|o| o.call.as_str()).collect();
    let input = serde_json::json!({ "key": key, "search": search, "force": force, "probe": calls });
    let out = run_harness(repo, input, &format!("the openers {host} draws"))?;
    let results: Vec<serde_json::Value> = serde_json::from_str(&out).context("the harness's opener probe is not a JSON list")?;
    if results.len() != openers.len() {
        bail!("the harness probed {} of the {} openers {host} draws", results.len(), openers.len());
    }
    let mut thrown = Vec::new();
    let mut modals: Vec<String> = Vec::new();
    for (opener, result) in openers.iter().zip(&results) {
        if let Some(error) = result.get("error") {
            thrown.push(format!("PAGES.{} drawn on {host} threw in the harness: {} ({})", opener.name, opener.call, error.as_str().unwrap_or_default()));
        } else if result.get("modal").and_then(serde_json::Value::as_bool).context("an opener probe result has no modal flag")? {
            if !modals.contains(&opener.name) {
                modals.push(opener.name.clone());
            }
        }
    }
    if !thrown.is_empty() {
        bail!("mockup opener failed:\n  {}", thrown.join("\n  "));
    }
    Ok(modals)
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

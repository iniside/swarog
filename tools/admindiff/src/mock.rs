//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.
//! A menu is rendered the same way: `UI.openMenu` builds it and the `.menu` element it
//! appends to the body is captured. A modal is rendered without a page too: the named
//! `PAGES.<opener>` runs with the page map entry's arguments and the `.scrim` element it
//! appends to the body is captured. The harness also records every menu id the mockup's
//! registry defines through `UI.defineMenu` ([`defined_menus`]). A page render can force the
//! ok state: the harness sets `UI.state.force = "ok"`, as the mockup's state picker
//! (`REG.setForce`) does, before `REG.mount`. The modals, menus and tabs a render draws come from
//! the renders too: [`probe`] runs every `onclick` body a render draws ([`onclicks`]), as
//! written, on its page mounted in a fresh harness mockup, and records what it does: each `.scrim`
//! it appends, named by the `PAGES` function whose call appended it or by the body, each
//! `UI.openMenu` call and each `PAGES.tab` assignment; a body that throws fails by name. No
//! `onclick` text is parsed, so every spelling of a call is seen.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::html::{find_all, Node};
use crate::pages::{DrawnTabs, MockOpener, Page};

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

// Each render boots a fresh mockup: a new context holding stub browser globals, in which the
// mockup's scripts run. A stub element finds a fresh stub for every `querySelector` and
// `closest`, holds the non-empty `value` "stub" and a `classList` that edits its `className`, so a
// guard on an ancestor or a typed value takes the path a user reaches; `checked` stays falsy, so
// only one branch of a guard on it runs. Outside a probe timers never fire. `REG.render` assigns
// `document.body.innerHTML` right after `P.<key>.render()`; the setter throws a sentinel there,
// so the legend fill-in and the page's `after` hook never run. With
// `input.menu` no page mounts: `UI.openMenu` runs against a stub event and anchor, and the
// element it appends to the body is written as `<div class="menu">` around its HTML. With
// `input.modal` no page mounts either: `PAGES[opener]` is called with the entry's arguments,
// and the first element it appends to the body with the class `scrim` (`UI.modal`, which
// `UI.confirm` calls) is written as `<div class="scrim">` around its HTML. The
// global `UI` is an accessor that wraps the `defineMenu` of whatever object a script assigns
// to it, before any script can call it, so every defined menu id is recorded; with
// `input.registry` no page mounts and the ids are written as JSON. With `input.force` the page
// render sets `UI.state.force = "ok"` before it mounts, after `boot.js` has read `?state=`, so
// every forceable panel and tile draws ok. With `input.probe`, a list of `onclick` bodies, each
// body runs verbatim on its own fresh mockup with the page mounted as for a render (`this` a stub
// element, `event` a click on it), after every function of `PAGES` is wrapped to record its name
// while a call the body made to it runs, and every function of `REG` is wrapped to return
// normally from the sentinel, so a body that re-renders the page goes on running; a body
// assignment outside every `REG` call is an error. Timer callbacks queue, each with the outermost
// `PAGES` call running when it was scheduled (an interval runs once), and run after the body,
// for at most `MAX_TIMER_ROUNDS` rounds. While it runs, `PAGES.tab` is an accessor recording each
// value assigned to it, and `UI.openMenu` is wrapped to record each call's menu id and argument
// (null when it has none) before it opens the menu. Each body is written as `{scrims, menus, tabs}`:
// one `scrims` entry per `.scrim` it appended to the body, the name of the `PAGES` function that
// appended it or null when it was appended outside every such call; each `UI.openMenu` call as
// `[id, arg]`; each `PAGES.tab` value; or as `{error}` when it threw.
const HARNESS: &str = r#""use strict";
const fs = require("fs"), vm = require("vm");
const input = JSON.parse(process.env.ADMINDIFF_INPUT);
const CAPTURED = Symbol("captured");
const scripts = input.scripts.map(file => new vm.Script(fs.readFileSync(file, "utf8"), { filename: file }));
const inert = () => {};
const element = () => {
  const el = {
    style: {}, dataset: {}, children: [], className: "", value: "stub",
    addEventListener: inert, appendChild: inert, remove: inert, setAttribute: inert, focus: inert,
    querySelector: () => element(), querySelectorAll: () => [], closest: () => element(),
    getBoundingClientRect: () => ({ left: 0, right: 0, top: 0, bottom: 0 }),
  };
  const tokens = () => String(el.className).split(/\s+/).filter(Boolean);
  el.classList = {
    add: (...names) => { el.className = [...new Set([...tokens(), ...names])].join(" "); },
    remove: (...names) => { el.className = tokens().filter(t => !names.includes(t)).join(" "); },
    toggle: (name, force) => {
      const on = force === undefined ? !tokens().includes(name) : !!force;
      if (on) el.classList.add(name); else el.classList.remove(name);
      return on;
    },
    contains: name => tokens().includes(name),
  };
  return el;
};
const hasClass = (el, name) => !!el && typeof el.className === "string" && el.className.split(/\s+/).includes(name);
const MAX_TIMER_ROUNDS = 10;
// A click on `el`: its target and current target.
const click = el => ({ stopPropagation: inert, preventDefault: inert, currentTarget: el, target: el });
// A fresh mockup: `html` is what the body was last assigned, `appended` each element appended to
// the body with the `PAGES` function running when it was (`opener`, null outside every recorded
// call), `defined` each menu id `UI.defineMenu` was called with, `ctx` the context the scripts ran in.
function boot() {
  const m = { html: null, appended: [], defined: [], opener: null, ctx: null, timers: [], cleared: new Set(), probing: false, rendering: 0 };
  const body = element();
  Object.defineProperty(body, "innerHTML", {
    get: () => m.html ?? "",
    set: v => {
      if (m.probing && m.rendering === 0) throw new Error("assigned document.body.innerHTML outside every REG call");
      m.html = String(v);
      throw CAPTURED;
    },
  });
  let timerId = 0;
  const schedule = (fn, ...args) => {
    timerId += 1;
    if (typeof fn === "function") m.timers.push({ id: timerId, fn, args, opener: m.opener });
    return timerId;
  };
  const clear = id => { m.cleared.add(id); };
  body.appendChild = el => { m.appended.push({ el, opener: m.opener }); };
  const ctx = {
    console, process, URL, URLSearchParams,
    setTimeout: schedule, clearTimeout: clear, setInterval: schedule, clearInterval: clear,
    document: {
      addEventListener: inert, querySelector: () => null, querySelectorAll: () => [],
      createElement: element, body, documentElement: { dataset: {} }, title: "",
    },
    localStorage: { getItem: () => null, setItem: inert, removeItem: inert },
    matchMedia: () => ({ matches: false, addEventListener: inert }),
    location: { search: input.search, href: "" },
    innerWidth: 1280,
    innerHeight: 800,
  };
  ctx.window = ctx;
  let ui;
  Object.defineProperty(ctx, "UI", {
    configurable: true, enumerable: true,
    get: () => ui,
    set: v => {
      ui = v;
      const define = v && v.defineMenu;
      if (typeof define === "function") {
        v.defineMenu = function (id, ...rest) { m.defined.push(String(id)); return define.call(this, id, ...rest); };
      }
    },
  });
  vm.createContext(ctx);
  for (const script of scripts) script.runInContext(ctx);
  m.ctx = ctx;
  return m;
}
if (input.registry) {
  process.stdout.write(JSON.stringify(boot().defined));
} else if (input.menu) {
  const m = boot();
  m.ctx.UI.openMenu(click(element()), input.menu.id, input.menu.arg ?? undefined);
  const opened = m.appended.length ? m.appended[m.appended.length - 1].el : null;
  if (!hasClass(opened, "menu")) {
    process.stderr.write(`UI.openMenu('${input.menu.id}') appended no .menu element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="menu">${opened.innerHTML ?? ""}</div>`);
} else if (input.modal) {
  const m = boot();
  const open = m.ctx.PAGES[input.modal.opener];
  if (typeof open !== "function") {
    process.stderr.write(`PAGES.${input.modal.opener} is not a function\n`);
    process.exit(2);
  }
  open(...input.modal.args);
  const scrim = m.appended.map(a => a.el).find(el => hasClass(el, "scrim"));
  if (!scrim) {
    process.stderr.write(`PAGES.${input.modal.opener}() appended no .scrim element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="scrim">${scrim.innerHTML ?? ""}</div>`);
} else if (input.probe) {
  const results = [];
  for (const onclick of input.probe) {
    const m = boot();
    mount(m);
    m.appended.length = 0;
    m.timers.length = 0;
    const reg = m.ctx.REG;
    for (const name of Object.keys(reg)) {
      const fn = reg[name];
      if (typeof fn !== "function") continue;
      reg[name] = function (...args) {
        m.rendering += 1;
        try { return fn.apply(this, args); } catch (e) { if (e !== CAPTURED) throw e; } finally { m.rendering -= 1; }
      };
    }
    const pages = m.ctx.PAGES;
    for (const name of Object.keys(pages)) {
      const fn = pages[name];
      if (typeof fn !== "function") continue;
      pages[name] = function (...args) {
        if (m.opener !== null) return fn.apply(this, args);
        m.opener = name;
        try { return fn.apply(this, args); } finally { m.opener = null; }
      };
    }
    const menus = [], tabs = [];
    let tab = pages.tab;
    Object.defineProperty(pages, "tab", {
      configurable: true, enumerable: true,
      get: () => tab,
      set: v => { tab = v; tabs.push(String(v)); },
    });
    const ui = m.ctx.UI;
    if (ui && typeof ui.openMenu === "function") {
      const open = ui.openMenu;
      ui.openMenu = function (ev, id, arg, ...rest) {
        menus.push([String(id), arg === undefined || arg === null ? null : String(arg)]);
        return open.call(this, ev, id, arg, ...rest);
      };
    }
    let error = null;
    m.probing = true;
    try {
      const clicked = element();
      vm.runInContext(`(function (event) {\n${onclick}\n})`, m.ctx).call(clicked, click(clicked));
      for (let round = 0; m.timers.length; round++) {
        if (round === MAX_TIMER_ROUNDS) throw new Error(`timers still pending after ${MAX_TIMER_ROUNDS} rounds`);
        for (const timer of m.timers.splice(0)) {
          if (m.cleared.has(timer.id)) continue;
          m.opener = timer.opener;
          try { timer.fn(...timer.args); } finally { m.opener = null; }
        }
      }
    } catch (e) {
      error = String(e && e.message !== undefined ? e.message : e);
    }
    const scrims = m.appended.filter(a => hasClass(a.el, "scrim")).map(a => a.opener);
    results.push(error === null ? { scrims, menus, tabs } : { error });
  }
  process.stdout.write(JSON.stringify(results));
} else {
  const m = boot();
  mount(m);
  process.stdout.write(m.html);
}
function mount(m) {
  const page = m.ctx.PAGES[input.key];
  if (!page || typeof page.render !== "function") {
    process.stderr.write(`PAGES.${input.key} has no render()\n`);
    process.exit(2);
  }
  if (input.force) {
    const ui = m.ctx.UI;
    if (!ui.state || typeof ui.state !== "object") {
      process.stderr.write("the mockup has no UI.state to force ok on\n");
      process.exit(2);
    }
    ui.state.force = "ok";
  }
  try { m.ctx.REG.mount(page.meta, page.render); } catch (e) { if (e !== CAPTURED) throw e; }
  if (m.html === null) {
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

/// The `.menu` element `UI.openMenu(event, '<id>', '<arg>')` opens in the mockup; with no `arg`
/// the call passes none.
pub fn render_menu(repo: &Path, id: &str, arg: Option<&str>) -> Result<String> {
    let input = serde_json::json!({ "search": "", "menu": { "id": id, "arg": arg } });
    run_harness(repo, input, &format!("menu {id}"))
}

/// The `.scrim` element `PAGES.<opener>(<args>)` appends in the mockup, its modal inside.
pub fn render_modal(repo: &Path, opener: &str, args: &[&str]) -> Result<String> {
    let input = serde_json::json!({ "search": "", "modal": { "opener": opener, "args": args } });
    run_harness(repo, input, &format!("PAGES.{opener}"))
}

/// Every `onclick` body the rendered mockup `doc` draws, verbatim, once each, in order.
pub fn onclicks(doc: &[Node]) -> Vec<String> {
    let mut elements = Vec::new();
    find_all(doc, &|e| e.attr("onclick").is_some(), &mut elements);
    let mut bodies: Vec<String> = Vec::new();
    for e in elements {
        let body = e.attr("onclick").unwrap_or_default();
        if !bodies.iter().any(|b| b == body) {
            bodies.push(body.to_string());
        }
    }
    bodies
}

/// Which `onclick` bodies of a render [`probe`] runs: every one, or only those of its `.tab`
/// elements when only the tabs are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Onclicks {
    All,
    Tabs,
}

/// What the `onclick` bodies a mockup render draws did when the harness ran them on its page
/// ([`probe`]): the mockup's calls, whatever their spelling, are the only source of the modals,
/// menus and tabs it draws.
#[derive(Debug)]
pub struct Probe {
    key: String,
    /// Each `.scrim` a body appended, named by the `PAGES` function whose call by the body
    /// appended it ([`MockOpener::Pages`]), or by the body itself when no such call did
    /// ([`MockOpener::Onclick`]); a body that calls two `PAGES` functions has a modal of each one
    /// that appended a `.scrim`. Once each, in order.
    pub modals: Vec<MockOpener<String>>,
    /// Each menu id a body opened through `UI.openMenu`, once each, in the order of the bodies,
    /// with the argument of the first such call (none when it passed none).
    pub menus: Vec<(String, Option<String>)>,
    /// Every `.tab` element whose body assigned `PAGES.tab`, as the first value it assigned, in
    /// order, and the first of them drawn `.on`.
    pub tabs: DrawnTabs,
}

impl Probe {
    /// The ids of [`Probe::menus`].
    pub fn menu_ids(&self) -> Vec<String> {
        self.menus.iter().map(|(id, _)| id.clone()).collect()
    }

    /// The argument the first body that opens menu `menu` passes; a render that opens no such
    /// menu fails, as the crawl fails a portal page that draws no such menu.
    pub fn menu_arg(&self, menu: &str) -> Result<Option<&str>> {
        match self.menus.iter().find(|(id, _)| id == menu) {
            Some((_, arg)) => Ok(arg.as_deref()),
            None => bail!("P.{} draws no {menu} menu; the mockup has no such menu", self.key),
        }
    }

    /// Fails unless the render selects tab `tab` ([`DrawnTabs::selected`]). The mockup falls back
    /// to its first tab for an id it does not know, so a render alone does not show the tab was
    /// reached.
    pub fn check_tab(&self, tab: &str) -> Result<()> {
        if self.tabs.selected.as_deref() != Some(tab) {
            bail!("P.{} does not select tab {tab}; the mockup has no such tab", self.key);
        }
        Ok(())
    }
}

/// Runs the `onclick` bodies (`scope`) the mockup render `doc` of `P.<key>` draws, each verbatim
/// on its own fresh `P.<key>` mounted as `page` renders it (its search, forced ok when it
/// forces; for a menu its host page's), and returns what they did; `host` names the render in
/// errors. A body that throws fails with its host and its text; one that re-renders the page is
/// no error.
pub fn probe(repo: &Path, host: &str, key: &str, page: &Page, doc: &[Node], scope: Onclicks) -> Result<Probe> {
    let bodies = match scope {
        Onclicks::All => onclicks(doc),
        Onclicks::Tabs => tab_onclicks(doc),
    };
    let mut probe = Probe { key: key.to_string(), modals: Vec::new(), menus: Vec::new(), tabs: DrawnTabs::default() };
    if bodies.is_empty() {
        return Ok(probe);
    }
    let input = serde_json::json!({ "key": key, "search": page.mock_search(), "force": page.force, "probe": bodies });
    let out = run_harness(repo, input, &format!("the onclicks {host} draws"))?;
    let results: Vec<serde_json::Value> = serde_json::from_str(&out).context("the harness's onclick probe is not a JSON list")?;
    if results.len() != bodies.len() {
        bail!("the harness ran {} of the {} onclicks {host} draws", results.len(), bodies.len());
    }
    let mut thrown = Vec::new();
    // Each body that assigned `PAGES.tab`, with the first value it assigned.
    let mut tab_of: Vec<(&str, String)> = Vec::new();
    for (body, result) in bodies.iter().zip(&results) {
        if let Some(error) = result.get("error") {
            thrown.push(format!("onclick {body:?} drawn on {host} threw in the harness: {}", error.as_str().unwrap_or_default()));
            continue;
        }
        let list = |field: &str| {
            result.get(field).and_then(serde_json::Value::as_array).with_context(|| format!("an onclick probe result has no {field} list"))
        };
        for scrim in list("scrims")? {
            let opener = match scrim.as_str() {
                Some(name) => MockOpener::Pages(name.to_string()),
                None => MockOpener::Onclick(body.clone()),
            };
            if !probe.modals.contains(&opener) {
                probe.modals.push(opener);
            }
        }
        for call in list("menus")? {
            let id = call.get(0).and_then(serde_json::Value::as_str).context("a recorded UI.openMenu call has no menu id")?;
            let arg = call.get(1).and_then(serde_json::Value::as_str).map(str::to_string);
            if !probe.menus.iter().any(|(m, _)| m == id) {
                probe.menus.push((id.to_string(), arg));
            }
        }
        if let Some(tab) = list("tabs")?.first() {
            tab_of.push((body.as_str(), tab.as_str().context("a recorded PAGES.tab value is no string")?.to_string()));
        }
    }
    if !thrown.is_empty() {
        bail!("mockup onclick failed:\n  {}", thrown.join("\n  "));
    }
    let mut buttons = Vec::new();
    find_all(doc, &|e| e.has_class("tab") && e.attr("onclick").is_some(), &mut buttons);
    for b in buttons {
        let body = b.attr("onclick").unwrap_or_default();
        let Some((_, id)) = tab_of.iter().find(|(set, _)| *set == body) else { continue };
        if b.has_class("on") && probe.tabs.selected.is_none() {
            probe.tabs.selected = Some(id.clone());
        }
        probe.tabs.tabs.push(id.clone());
    }
    Ok(probe)
}

/// Every `onclick` body of a `.tab` element the rendered mockup `doc` draws, once each, in order.
fn tab_onclicks(doc: &[Node]) -> Vec<String> {
    let mut elements = Vec::new();
    find_all(doc, &|e| e.has_class("tab") && e.attr("onclick").is_some(), &mut elements);
    let mut bodies: Vec<String> = Vec::new();
    for e in elements {
        let body = e.attr("onclick").unwrap_or_default();
        if !bodies.iter().any(|b| b == body) {
            bodies.push(body.to_string());
        }
    }
    bodies
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

#[cfg(test)]
#[path = "mock_tests.rs"]
pub(crate) mod mock_tests;

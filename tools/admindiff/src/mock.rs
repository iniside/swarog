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
//! written, on its page mounted in a fresh harness mockup, and records what it does: each modal
//! it appends (an element a `UI.modal`, `UI.confirm` or `REG.openSearch` call drew), named by the
//! `PAGES` function whose call appended it or by the body, each
//! `UI.openMenu` call, each `PAGES` key it assigns and each element it appends that no recorded
//! opener draws; a body that throws fails by name. No `onclick` text is parsed, so every spelling
//! of a call is seen.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::html::{collapse, find_all, Node};
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
// `UI.confirm` calls) is written as `<div class="scrim">` around its HTML. The global `UI` is an
// accessor that wraps every function of whatever object a script assigns to it, before any script
// can call or alias them, so each call through `UI` or an alias taken from it is a drawer of what
// it appends, every defined menu id is recorded, and so is each `openMenu` call; the global `REG`
// is an accessor that wraps every function of its object the same way, as `REG.<name>`; with
// `input.registry` no page mounts and the ids are written as JSON. The global `PAGES` is an
// accessor that hands out a proxy of the object a script assigns to it, so every key written
// through it or an alias taken from it after the scripts load is recorded. With `input.force` the
// page render sets `UI.state.force = "ok"` before it mounts, after `boot.js` has read `?state=`,
// so every forceable panel and tile draws ok. Every recorder reads one attribution context
// (`at`): the phase (`mount` while the page mounts, `render` inside a `REG` call of a probe,
// `body` while a probed body runs), the outermost `PAGES` call running and the `UI` calls
// running. A timer saves the context it was scheduled in and runs in it. With `input.probe`, a list
// of `onclick` bodies, each body runs verbatim on its own fresh mockup with the page mounted as for
// a render (`this` a stub element, `event` a click on it), after every function of `PAGES` is
// wrapped to name itself the opener while a call the body made to it runs, and every function of
// `REG` is wrapped to return normally from the sentinel, so a body that re-renders the page goes on
// running; a body assignment outside every `REG` call is an error. The timers the mount scheduled
// run before the body, the body's after it, for at most `MAX_TIMER_ROUNDS` rounds each. A `PAGES`
// key written in the body phase is the body's own (`tab` with its value), one written in the mount
// or render phase is assigned by a render. A probe of a mockup whose `UI.openMenu` is not the
// recording wrapper (none, or replaced after `UI` was set) fails. Each body is written as
// `{scrims, menus, tabs, keys, loose, excluded, rendered}`: one `scrims` entry per element a
// `MODAL_OPENERS` call drew that it appended to the body, the name of the `PAGES` function that appended it or null when it was
// appended outside every such call; each `UI.openMenu` call as `[id, arg]` (arg null when it has
// none); each `PAGES.tab` value; `keys` each other `PAGES` key it assigned, once each; `loose`
// each element appended to the body, the mount's included, that no `MENU_OPENERS` or
// `MODAL_OPENERS` call drew and whose drawers name none of `input.appenders`, whatever its class,
// with whether
// the mount appended it and its drawers; `excluded` each of `input.appenders` that drew an
// element, the innermost drawer named; `rendered` each of `tab` and the listed keys a render
// assigned, once each; or as `{error, rendered}` when it threw.
const HARNESS: &str = r#""use strict";
const fs = require("fs"), vm = require("vm");
const input = JSON.parse(process.env.ADMINDIFF_INPUT);
const CAPTURED = Symbol("captured");
const RECORDS = Symbol("records");
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
// The calls whose appended element is a menu or a popover, and those whose appended element is a
// modal (`UI.confirm` calls the bundle's `modal` directly, past the wrapper).
const MENU_OPENERS = ["UI.openMenu", "UI.popover"];
const MODAL_OPENERS = ["UI.modal", "UI.confirm", "REG.openSearch"];
// A click on `el`: its target and current target.
const click = el => ({ stopPropagation: inert, preventDefault: inert, currentTarget: el, target: el });
// The attribution context outside every recorded call: the scripts loading, the harness setting up.
const LOADING = { phase: "load", opener: null, drawers: [] };
// A fresh mockup: `html` is what the body was last assigned, `at` the attribution context every
// recorder reads (`phase` load, mount, render or body; `opener` the outermost `PAGES` call running;
// `drawers` each `UI` call running, as `UI.<name>`, outermost first), `appended` each element
// appended to the body with the context it was appended in, `defined` each menu id
// `UI.defineMenu` was called with, `menus` each `UI.openMenu` call as `[id, arg]` with its
// context, `writes` each key the `PAGES` object was assigned outside the load with its value and
// context, `pages` the object the scripts assigned to `PAGES`, `ctx` the context the scripts ran
// in.
function boot() {
  const m = { html: null, appended: [], defined: [], menus: [], writes: [], at: LOADING, pages: null, ctx: null, timers: [], cleared: new Set() };
  const body = element();
  Object.defineProperty(body, "innerHTML", {
    get: () => m.html ?? "",
    set: v => {
      if (m.at.phase === "body") throw new Error("assigned document.body.innerHTML outside every REG call");
      m.html = String(v);
      throw CAPTURED;
    },
  });
  let timerId = 0;
  const schedule = (fn, ...args) => {
    timerId += 1;
    if (typeof fn === "function") m.timers.push({ id: timerId, fn, args, at: m.at });
    return timerId;
  };
  const clear = id => { m.cleared.add(id); };
  body.appendChild = el => { m.appended.push({ el, at: m.at }); };
  const createElement = tag => {
    const el = element();
    el.tagName = String(tag).toLowerCase();
    return el;
  };
  const ctx = {
    console, process, URL, URLSearchParams,
    setTimeout: schedule, clearTimeout: clear, setInterval: schedule, clearInterval: clear,
    document: {
      addEventListener: inert, querySelector: () => null, querySelectorAll: () => [],
      createElement, body, documentElement: { dataset: {} }, title: "",
    },
    localStorage: { getItem: () => null, setItem: inert, removeItem: inert },
    matchMedia: () => ({ matches: false, addEventListener: inert }),
    location: { search: input.search, href: "" },
    innerWidth: 1280,
    innerHeight: 800,
  };
  ctx.window = ctx;
  const drawers = (v, prefix) => {
    if (!v || typeof v !== "object") return;
    for (const name of Object.keys(v)) {
      const fn = v[name];
      if (typeof fn !== "function" || fn[RECORDS]) continue;
      const drawer = prefix + "." + name;
      const wrapped = function (...args) {
        if (drawer === "UI.defineMenu") m.defined.push(String(args[0]));
        if (drawer === "UI.openMenu") {
          const arg = args[2];
          m.menus.push({ call: [String(args[1]), arg === undefined || arg === null ? null : String(arg)], at: m.at });
        }
        const outer = m.at;
        m.at = { ...outer, drawers: [...outer.drawers, drawer] };
        try { return fn.apply(this, args); } finally { m.at = outer; }
      };
      wrapped[RECORDS] = true;
      v[name] = wrapped;
    }
  };
  for (const prefix of ["UI", "REG"]) {
    let held;
    Object.defineProperty(ctx, prefix, {
      configurable: true, enumerable: true,
      get: () => held,
      set: v => { held = v; drawers(v, prefix); },
    });
  }
  let pages;
  Object.defineProperty(ctx, "PAGES", {
    configurable: true, enumerable: true,
    get: () => pages,
    set: v => {
      if (v === pages) return;
      if (!v || typeof v !== "object") { pages = v; m.pages = null; return; }
      m.pages = v;
      pages = new Proxy(v, {
        set: (target, k, value) => {
          if (typeof k === "string" && m.at.phase !== "load") m.writes.push({ k, value, at: m.at });
          target[k] = value;
          return true;
        },
      });
    },
  });
  vm.createContext(ctx);
  for (const script of scripts) script.runInContext(ctx);
  m.ctx = ctx;
  return m;
}
// Runs the queued timers, each in the context it was scheduled in, until none is left; an
// interval runs once. A render a timer runs that assigns the body returns there.
function runTimers(m) {
  for (let round = 0; m.timers.length; round++) {
    if (round === MAX_TIMER_ROUNDS) throw new Error(`timers still pending after ${MAX_TIMER_ROUNDS} rounds`);
    for (const timer of m.timers.splice(0)) {
      if (m.cleared.has(timer.id)) continue;
      const outer = m.at;
      m.at = timer.at;
      try { timer.fn(...timer.args); } catch (e) { if (e !== CAPTURED) throw e; } finally { m.at = outer; }
    }
  }
}
const message = e => String(e && e.message !== undefined ? e.message : e);
const describe = el => `<${el.tagName || "element"}${el.className ? ` class="${el.className}"` : ""}>`;
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
    const reg = m.ctx.REG;
    for (const name of Object.keys(reg)) {
      const fn = reg[name];
      if (typeof fn !== "function") continue;
      reg[name] = function (...args) {
        const outer = m.at;
        m.at = { ...outer, phase: "render" };
        try { return fn.apply(this, args); } catch (e) { if (e !== CAPTURED) throw e; } finally { m.at = outer; }
      };
    }
    for (const name of Object.keys(m.pages)) {
      const fn = m.pages[name];
      if (typeof fn !== "function") continue;
      m.pages[name] = function (...args) {
        if (m.at.opener !== null) return fn.apply(this, args);
        const outer = m.at;
        m.at = { ...outer, opener: name };
        try { return fn.apply(this, args); } finally { m.at = outer; }
      };
    }
    const ui = m.ctx.UI;
    if (!ui || typeof ui.openMenu !== "function" || !ui.openMenu[RECORDS]) {
      process.stderr.write("the mockup's UI.openMenu is not the harness's recording wrapper: it is missing or was replaced after UI was set\n");
      process.exit(2);
    }
    let error = null;
    try {
      runTimers(m);
    } catch (e) {
      error = `a timer the mount scheduled threw: ${message(e)}`;
    }
    if (error === null) {
      m.at = { phase: "body", opener: null, drawers: [] };
      try {
        const clicked = element();
        vm.runInContext(`(function (event) {\n${onclick}\n})`, m.ctx).call(clicked, click(clicked));
        runTimers(m);
      } catch (e) {
        error = message(e);
      }
      m.at = LOADING;
    }
    const rendered = [], tabs = [], keys = [];
    for (const w of m.writes) {
      if (w.at.phase !== "body") {
        if ((w.k === "tab" || input.listed.includes(w.k)) && !rendered.includes(w.k)) rendered.push(w.k);
      } else if (w.k === "tab") {
        tabs.push(String(w.value));
      } else if (!keys.includes(w.k)) {
        keys.push(w.k);
      }
    }
    if (error !== null) {
      results.push({ error, rendered });
      continue;
    }
    const scrims = [], loose = [], excluded = [];
    for (const a of m.appended) {
      const drawers = a.at.drawers;
      if (drawers.some(d => MENU_OPENERS.includes(d))) continue;
      if (drawers.some(d => MODAL_OPENERS.includes(d))) {
        if (a.at.phase !== "mount") scrims.push(a.at.opener);
        continue;
      }
      const named = [...drawers].reverse().find(d => input.appenders.includes(d));
      if (named !== undefined) {
        if (!excluded.includes(named)) excluded.push(named);
        continue;
      }
      loose.push({ element: describe(a.el), mount: a.at.phase === "mount", drawers });
    }
    const menus = m.menus.filter(c => c.at.phase !== "mount").map(c => c.call);
    results.push({ scrims, menus, tabs, keys, loose, excluded, rendered });
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
  m.at = { phase: "mount", opener: null, drawers: [] };
  try { m.ctx.REG.mount(page.meta, page.render); } catch (e) { if (e !== CAPTURED) throw e; } finally { m.at = LOADING; }
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
    /// Each modal (an element a `UI.modal`, `UI.confirm` or `REG.openSearch` call drew) a body
    /// appended, named by the `PAGES` function whose call by the body
    /// appended it ([`MockOpener::Pages`]), or by the body itself when no such call did
    /// ([`MockOpener::Onclick`]); a body that calls two `PAGES` functions has a modal of each one
    /// that appended one. Once each, in order.
    pub modals: Vec<MockOpener<String>>,
    /// Each menu id a body opened through `UI.openMenu`, once each, in the order of the bodies,
    /// with the argument of the first such call (none when it passed none).
    pub menus: Vec<(String, Option<String>)>,
    /// Every `.tab` element whose body assigned `PAGES.tab`, as the first value it assigned, in
    /// order, and the first of them drawn `.on`.
    pub tabs: DrawnTabs,
    /// Each key of [`Declared::non_tab_keys`] a body assigned itself, outside every render, once
    /// each, in order.
    pub non_tab_keys: Vec<String>,
    /// Each of [`Declared::appenders`] that drew an element a body appended to the body, once
    /// each, in order.
    pub appenders: Vec<String>,
}

/// What a probe accepts beside the page map's `PAGES.tab` and the recorded openers: the
/// `PAGES` keys other than `tab` an `onclick` body may assign (the production list's keys are
/// [`crate::pages::NON_TAB_KEYS`]), and the `UI.<name>` calls that may append an element to the
/// body that no recorded opener draws (the production list's names are
/// [`crate::pages::EXCLUDED_MOCK_APPENDS`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Declared<'a> {
    pub non_tab_keys: &'a [&'a str],
    pub appenders: &'a [&'a str],
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
/// no error. A body that assigns a `PAGES` key other than `tab` that is not in
/// `declared.non_tab_keys` fails naming its host, the key and its text. An element appended to
/// the body that no recorded opener drew (a menu or popover of `UI.openMenu` or `UI.popover`, a
/// modal of `UI.modal`, `UI.confirm` or `REG.openSearch`), whatever its class, and that no `UI`
/// call in `declared.appenders` drew fails naming its host, the element and the body, or the mount when the mount appended it. A `.tab` whose body assigns no
/// `PAGES.tab` and no declared key, a guarded assignment that does not run among them, fails, as
/// does a `.tab` with no `onclick`, by host and element. A render (the mount, a timer it
/// schedules, a re-render, a timer one schedules) that assigns `PAGES.tab` or a
/// `declared.non_tab_keys` key fails: only a body's own assignments say which tab it is.
pub fn probe(repo: &Path, host: &str, key: &str, page: &Page, doc: &[Node], scope: Onclicks, declared: &Declared) -> Result<Probe> {
    let bodies = match scope {
        Onclicks::All => onclicks(doc),
        Onclicks::Tabs => tab_onclicks(doc),
    };
    let mut probe =
        Probe { key: key.to_string(), modals: Vec::new(), menus: Vec::new(), tabs: DrawnTabs::default(), non_tab_keys: Vec::new(), appenders: Vec::new() };
    let mut failed = Vec::new();
    let mut bare = Vec::new();
    find_all(doc, &|e| e.has_class("tab") && e.attr("onclick").is_none(), &mut bare);
    for e in bare {
        let attrs: String = e.attrs.iter().map(|(k, v)| format!(" {k}={v:?}")).collect();
        failed.push(format!("<{}{attrs}> ({:?}) drawn on {host} is a .tab with no onclick", e.tag, collapse(&e.text())));
    }
    if bodies.is_empty() {
        if !failed.is_empty() {
            bail!("mockup onclick failed:\n  {}", failed.join("\n  "));
        }
        return Ok(probe);
    }
    let input = serde_json::json!({ "key": key, "search": page.mock_search(), "force": page.force, "probe": bodies, "listed": declared.non_tab_keys, "appenders": declared.appenders });
    let out = run_harness(repo, input, &format!("the onclicks {host} draws"))?;
    let results: Vec<serde_json::Value> = serde_json::from_str(&out).context("the harness's onclick probe is not a JSON list")?;
    if results.len() != bodies.len() {
        bail!("the harness ran {} of the {} onclicks {host} draws", results.len(), bodies.len());
    }
    // Each body that assigned `PAGES.tab`, with the first value it assigned.
    let mut tab_of: Vec<(&str, String)> = Vec::new();
    // Each body that did not throw, with the listed keys it assigned.
    let mut keys_of: Vec<(&str, Vec<String>)> = Vec::new();
    // Each of `tab` and the listed keys a render assigned.
    let mut rendered: Vec<String> = Vec::new();
    for (body, result) in bodies.iter().zip(&results) {
        let by_render = result
            .get("rendered")
            .and_then(serde_json::Value::as_array)
            .with_context(|| format!("the probe of onclick {body:?} drawn on {host} has no rendered list"))?;
        for k in by_render {
            let k = k.as_str().with_context(|| format!("a PAGES key a render on {host} assigned was recorded as no string"))?;
            if !rendered.iter().any(|r| r == k) {
                rendered.push(k.to_string());
            }
        }
        if let Some(error) = result.get("error") {
            failed.push(format!("onclick {body:?} drawn on {host} threw in the harness: {}", error.as_str().unwrap_or_default()));
            continue;
        }
        let list = |field: &str| {
            result.get(field).and_then(serde_json::Value::as_array).with_context(|| format!("the probe of onclick {body:?} drawn on {host} has no {field} list"))
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
            let id = call
                .get(0)
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("a UI.openMenu call by onclick {body:?} drawn on {host} was recorded with no menu id"))?;
            let arg = call.get(1).and_then(serde_json::Value::as_str).map(str::to_string);
            if !probe.menus.iter().any(|(m, _)| m == id) {
                probe.menus.push((id.to_string(), arg));
            }
        }
        if let Some(tab) = list("tabs")?.first() {
            let tab = tab.as_str().with_context(|| format!("a PAGES.tab value onclick {body:?} drawn on {host} assigned was recorded as no string"))?;
            tab_of.push((body.as_str(), tab.to_string()));
        }
        let mut keys = Vec::new();
        for k in list("keys")? {
            let k = k.as_str().with_context(|| format!("a PAGES key onclick {body:?} drawn on {host} assigned was recorded as no string"))?;
            if !declared.non_tab_keys.contains(&k) {
                failed.push(format!("onclick {body:?} drawn on {host} assigns PAGES.{k}, which is neither PAGE_MAP's tab nor a NON_TAB_KEYS key"));
            } else if !probe.non_tab_keys.iter().any(|n| n == k) {
                probe.non_tab_keys.push(k.to_string());
            }
            keys.push(k.to_string());
        }
        keys_of.push((body.as_str(), keys));
        for name in list("excluded")? {
            let name = name.as_str().with_context(|| format!("an appender onclick {body:?} drawn on {host} used was recorded as no string"))?;
            if !probe.appenders.iter().any(|a| a == name) {
                probe.appenders.push(name.to_string());
            }
        }
        for loose in list("loose")? {
            let element = loose.get("element").and_then(serde_json::Value::as_str);
            let mount = loose.get("mount").and_then(serde_json::Value::as_bool);
            let drawers = loose.get("drawers").and_then(serde_json::Value::as_array);
            let (Some(element), Some(mount), Some(drawers)) = (element, mount, drawers) else {
                bail!("an element onclick {body:?} drawn on {host} appended was recorded with no element, mount or drawers");
            };
            let drawers: Vec<&str> = drawers.iter().filter_map(serde_json::Value::as_str).collect();
            let calls = if drawers.is_empty() { "no UI call".to_string() } else { drawers.join(" > ") };
            let by = if mount { format!("{host}: the mount") } else { format!("onclick {body:?} drawn on {host}") };
            let error = format!("{by} appended {element} under {calls}, which no recorded opener drew and no EXCLUDED_MOCK_APPENDS entry names");
            if !failed.contains(&error) {
                failed.push(error);
            }
        }
    }
    for k in &rendered {
        failed.push(format!("{host}: a render assigns PAGES.{k}; tab ownership is unattributable"));
    }
    let mut buttons = Vec::new();
    find_all(doc, &|e| e.has_class("tab") && e.attr("onclick").is_some(), &mut buttons);
    for b in &buttons {
        let body = b.attr("onclick").unwrap_or_default();
        // A body that threw has failed already.
        let Some((_, keys)) = keys_of.iter().find(|(set, _)| *set == body) else { continue };
        let error = format!("onclick {body:?} drawn on {host} is a .tab that assigns no PAGES.tab and no NON_TAB_KEYS key");
        if keys.is_empty() && !tab_of.iter().any(|(set, _)| *set == body) && !failed.contains(&error) {
            failed.push(error);
        }
    }
    if !failed.is_empty() {
        bail!("mockup onclick failed:\n  {}", failed.join("\n  "));
    }
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

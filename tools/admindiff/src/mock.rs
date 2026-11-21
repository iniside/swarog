//! Renders a mockup page through `node`: the mockup's own scripts run against stub browser
//! globals, `REG.mount` calls `P.<key>.render()`, and the body HTML it assigns is captured.
//! A menu is rendered the same way: `UI.openMenu` builds it and the first element appended to
//! the body under that call is captured. A modal is rendered without a page too: the named
//! `PAGES.<opener>` runs with the page map entry's arguments and the first element appended to
//! the body under a modal opener's call is captured. The harness also records every menu id the mockup's
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
// so the legend fill-in and the page's `after` hook never run. An appended element is a menu, a
// modal or an `input.appenders` call's by its innermost drawer in `MENU_OPENERS`,
// `MODAL_OPENERS` or `input.appenders` (`kindOf`); with `input.menu`, `input.modal` or
// `input.probe` each listed opener must be a function of the mockup's `UI` or `REG`. With
// `input.menu` no page mounts: `UI.openMenu` runs against a stub event and anchor, and the first
// menu appended to the body is written as `<div class="menu">` around its HTML. With
// `input.modal` no page mounts either: `PAGES[opener]` is called with the entry's arguments,
// and the first modal appended to the body is written as `<div class="scrim">` around its HTML.
// The global `UI` is an
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
// `body` while a probed body runs), the origin (`mount` or `body`, which work it is part of, kept
// inside a `REG` call), the outermost `PAGES` call running and the `UI` calls running. A timer
// saves the context it was scheduled in and runs in it, so what a mount timer appends, opens or
// renders is the mount's, never a body's. With `input.probe`, a list of `onclick` bodies, the page
// first mounts on a fresh mockup as for a render and the timers the mount scheduled run, for at
// most `MAX_TIMER_ROUNDS` rounds; then each body runs verbatim on its own fresh mockup mounted the
// same way (`this` a stub element, `event` a click on it), and the timers it scheduled after it. Before the timers, every function of `PAGES` is wrapped to name
// itself the opener while a call to it runs outside every other such call, and every function of
// `REG` is wrapped to return normally from the sentinel, so a body that re-renders the page goes
// on running; a body assignment outside every `REG` call is an error. A `PAGES` key written in
// the body phase is the body's own (`tab` with its value), one written in the mount or render
// phase is assigned by a render. A probe or menu render of a mockup whose `UI.openMenu` is not the
// recording wrapper (replaced after `UI` was set) fails. The probe is written as `{mount, bodies}`:
// `mount` as `{error, scrims, menus, loose, excluded, rendered}` for what the scripts and the mount
// (its timers and the `REG` calls they make included) opened, appended and assigned, with no body
// run when `error` is set; each body as `{scrims, menus, tabs, keys, loose, excluded, rendered}`
// for what it, its timers and the renders it called did: one `scrims` entry per modal appended,
// the name of the `PAGES` function that appended it or null when it was appended outside every
// such call; each `UI.openMenu` call as `[id, arg]`
// (arg null when it has none); each `PAGES.tab` value; `keys` each other `PAGES` key it
// assigned, once each; `loose` each element appended that is no menu, no modal and no
// `input.appenders` call's, whatever its class, with its drawers; `excluded` each of
// `input.appenders` that drew an element; `rendered` each of `tab` and the listed keys a render
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
// The calls whose appended element is a menu, and those whose appended element is a modal
// (`UI.confirm` calls the bundle's `modal` directly, past the wrapper).
const MENU_OPENERS = ["UI.openMenu"];
const MODAL_OPENERS = ["UI.modal", "UI.confirm", "REG.openSearch"];
const APPENDERS = input.appenders || [];
// What an element appended under `drawers` is, by its innermost listed drawer: "menu", "modal",
// the `APPENDERS` call that drew it, or null when no listed call drew it.
const kindOf = drawers => {
  for (const d of [...drawers].reverse()) {
    if (MENU_OPENERS.includes(d)) return "menu";
    if (MODAL_OPENERS.includes(d)) return "modal";
    if (APPENDERS.includes(d)) return d;
  }
  return null;
};
// A click on `el`: its target and current target.
const click = el => ({ stopPropagation: inert, preventDefault: inert, currentTarget: el, target: el });
// The attribution context outside every recorded call: the scripts loading, the harness setting up.
const LOADING = { phase: "load", origin: null, opener: null, drawers: [] };
// A fresh mockup: `html` is what the body was last assigned, `at` the attribution context every
// recorder reads (`phase` load, mount, render or body; `origin` the work it belongs to, `mount` or
// `body`, kept through every `REG` call and timer, or null outside both; `opener` the outermost
// `PAGES` call running;
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
// Each listed opener must be a function of the mockup.
function checkOpeners(m) {
  for (const name of [...MENU_OPENERS, ...MODAL_OPENERS]) {
    const [prefix, fn] = name.split(".");
    const owner = m.ctx[prefix];
    if (!owner || typeof owner[fn] !== "function") {
      process.stderr.write(`${name}, a listed opener, is no function of the mockup\n`);
      process.exit(2);
    }
  }
}
function checkRecorded(m) {
  if (!m.ctx.UI.openMenu[RECORDS]) {
    process.stderr.write("the mockup's UI.openMenu is not the harness's recording wrapper: it was replaced after UI was set\n");
    process.exit(2);
  }
}
// The elements of `m` appended in a phase `keep` accepts, sorted: `scrims` the opener of each
// modal, `loose` each element no listed call drew, `excluded` each `APPENDERS` call that drew one.
function sort(m, keep) {
  const scrims = [], loose = [], excluded = [];
  for (const a of m.appended) {
    if (!keep(a.at)) continue;
    const kind = kindOf(a.at.drawers);
    if (kind === "menu") continue;
    if (kind === "modal") {
      scrims.push(a.at.opener);
    } else if (kind !== null) {
      if (!excluded.includes(kind)) excluded.push(kind);
    } else {
      loose.push({ element: describe(a.el), drawers: a.at.drawers });
    }
  }
  return { scrims, loose, excluded };
}
// Each of `tab` and the listed keys a render (any phase but the body's) assigned, once each.
const renderedOf = m => {
  const rendered = [];
  for (const w of m.writes) {
    if (w.at.phase !== "body" && (w.k === "tab" || input.listed.includes(w.k)) && !rendered.includes(w.k)) rendered.push(w.k);
  }
  return rendered;
};
// A fresh mockup with the page mounted for a probe, its `REG` and `PAGES` wrapped, and the timers
// the mount scheduled run; `error` what those timers threw, or null.
function settle() {
  const m = boot();
  checkOpeners(m);
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
  checkRecorded(m);
  try {
    runTimers(m);
  } catch (e) {
    return { m, error: `a timer it scheduled threw: ${message(e)}` };
  }
  return { m, error: null };
}
if (input.registry) {
  process.stdout.write(JSON.stringify(boot().defined));
} else if (input.menu) {
  const m = boot();
  checkOpeners(m);
  checkRecorded(m);
  m.ctx.UI.openMenu(click(element()), input.menu.id, input.menu.arg ?? undefined);
  const opened = m.appended.find(a => kindOf(a.at.drawers) === "menu");
  if (!opened) {
    process.stderr.write(`UI.openMenu('${input.menu.id}') appended no element\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="menu">${opened.el.innerHTML ?? ""}</div>`);
} else if (input.modal) {
  const m = boot();
  checkOpeners(m);
  const open = m.ctx.PAGES[input.modal.opener];
  if (typeof open !== "function") {
    process.stderr.write(`PAGES.${input.modal.opener} is not a function\n`);
    process.exit(2);
  }
  open(...input.modal.args);
  const opened = m.appended.find(a => kindOf(a.at.drawers) === "modal");
  if (!opened) {
    process.stderr.write(`PAGES.${input.modal.opener}() appended no element under ${MODAL_OPENERS.join(", ")}\n`);
    process.exit(2);
  }
  process.stdout.write(`<div class="scrim">${opened.el.innerHTML ?? ""}</div>`);
} else if (input.probe) {
  const ofBody = at => at.origin === "body";
  const first = settle();
  const sorted = sort(first.m, () => true);
  const mountMenus = first.m.menus.map(c => c.call);
  const out = {
    mount: { error: first.error, scrims: sorted.scrims, menus: mountMenus, loose: sorted.loose, excluded: sorted.excluded, rendered: renderedOf(first.m) },
    bodies: [],
  };
  for (const onclick of first.error === null ? input.probe : []) {
    const { m, error: settled } = settle();
    let error = settled === null ? null : `the mount threw on this run: ${settled}`;
    if (error === null) {
      m.at = { phase: "body", origin: "body", opener: null, drawers: [] };
      try {
        const clicked = element();
        vm.runInContext(`(function (event) {\n${onclick}\n})`, m.ctx).call(clicked, click(clicked));
        runTimers(m);
      } catch (e) {
        error = message(e);
      }
      m.at = LOADING;
    }
    const rendered = renderedOf(m), tabs = [], keys = [];
    for (const w of m.writes) {
      if (w.at.phase !== "body") continue;
      if (w.k === "tab") tabs.push(String(w.value));
      else if (!keys.includes(w.k)) keys.push(w.k);
    }
    if (error !== null) {
      out.bodies.push({ error, rendered });
      continue;
    }
    const { scrims, loose, excluded } = sort(m, ofBody);
    const menus = m.menus.filter(c => ofBody(c.at)).map(c => c.call);
    out.bodies.push({ scrims, menus, tabs, keys, loose, excluded, rendered });
  }
  process.stdout.write(JSON.stringify(out));
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
  m.at = { phase: "mount", origin: "mount", opener: null, drawers: [] };
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

/// The menu `UI.openMenu(event, '<id>', '<arg>')` opens in the mockup (the first element appended
/// under that call), as `<div class="menu">` around its HTML; with no `arg` the call passes none.
pub fn render_menu(repo: &Path, id: &str, arg: Option<&str>) -> Result<String> {
    let input = serde_json::json!({ "search": "", "menu": { "id": id, "arg": arg }, "appenders": production_appenders() });
    run_harness(repo, input, &format!("menu {id}"))
}

/// The modal `PAGES.<opener>(<args>)` opens in the mockup (the first element appended under a
/// `UI.modal`, `UI.confirm` or `REG.openSearch` call), as `<div class="scrim">` around its HTML.
pub fn render_modal(repo: &Path, opener: &str, args: &[&str]) -> Result<String> {
    let input = serde_json::json!({ "search": "", "modal": { "opener": opener, "args": args }, "appenders": production_appenders() });
    run_harness(repo, input, &format!("PAGES.{opener}"))
}

/// The `UI` calls of [`crate::pages::EXCLUDED_MOCK_APPENDS`], whose elements are no menu or modal.
fn production_appenders() -> Vec<&'static str> {
    crate::pages::EXCLUDED_MOCK_APPENDS.iter().map(|(name, _)| *name).collect()
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
    /// Each of [`Declared::appenders`] that drew an element a body or the mount appended to the body, once
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
/// the body whose innermost listed drawer is none of the recorded openers (a menu of
/// `UI.openMenu`, a modal of `UI.modal`, `UI.confirm` or `REG.openSearch`) and no `UI` call in
/// `declared.appenders`, whatever its class, fails naming its host, the element and the body, or
/// the mount when the scripts, the mount or a timer it scheduled appended it; the mount is checked
/// with no body drawn too, and a timer it scheduled that throws fails once, as the mount's. A
/// modal or menu the mount opens (itself, a timer it scheduled or a `REG` call that timer makes)
/// fails once as the mount's, by host and opener or menu id: no drawn control opens it, so it
/// never counts for a body. A `.tab` whose body assigns no `PAGES.tab` and no declared key, a guarded assignment that does not run among them, fails, as
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
    let input = serde_json::json!({ "key": key, "search": page.mock_search(), "force": page.force, "probe": bodies, "listed": declared.non_tab_keys, "appenders": declared.appenders });
    let out = run_harness(repo, input, &format!("the onclicks {host} draws"))?;
    let out: serde_json::Value = serde_json::from_str(&out).context("the harness's onclick probe is not JSON")?;
    let mount = out.get("mount").with_context(|| format!("the harness's probe of {host} has no mount"))?;
    let results = out.get("bodies").and_then(serde_json::Value::as_array).with_context(|| format!("the harness's probe of {host} has no body list"))?;
    // Each of `tab` and the listed keys a render assigned.
    let mut rendered: Vec<String> = Vec::new();
    let what = format!("the mount of {host}");
    if let Some(error) = mount.get("error").and_then(serde_json::Value::as_str) {
        failed.push(format!("{host}: the mount threw in the harness: {error}"));
    } else if results.len() != bodies.len() {
        bail!("the harness ran {} of the {} onclicks {host} draws", results.len(), bodies.len());
    }
    for k in strings(mount, "rendered", &what)? {
        if !rendered.iter().any(|r| r == k) {
            rendered.push(k.to_string());
        }
    }
    for name in strings(mount, "excluded", &what)? {
        if !probe.appenders.iter().any(|a| a == name) {
            probe.appenders.push(name.to_string());
        }
    }
    for loose in list(mount, "loose", &what)? {
        failed.push(loose_failure(&format!("{host}: the mount"), loose)?);
    }
    for scrim in list(mount, "scrims", &what)? {
        let opener = scrim.as_str().map_or_else(|| "outside every PAGES call".to_string(), |name| format!("in PAGES.{name}"));
        failed.push(format!("{host}: the mount opened a modal {opener}, which no drawn control opens"));
    }
    for call in list(mount, "menus", &what)? {
        let id = call.get(0).and_then(serde_json::Value::as_str).with_context(|| format!("a UI.openMenu call by {what} was recorded with no menu id"))?;
        failed.push(format!("{host}: the mount opened menu {id}, which no drawn control opens"));
    }
    // Each body that assigned `PAGES.tab`, with the first value it assigned.
    let mut tab_of: Vec<(&str, String)> = Vec::new();
    // Each body that did not throw, with the listed keys it assigned.
    let mut keys_of: Vec<(&str, Vec<String>)> = Vec::new();
    for (body, result) in bodies.iter().zip(results) {
        let what = format!("onclick {body:?} drawn on {host}");
        for k in strings(result, "rendered", &what)? {
            if !rendered.iter().any(|r| r == k) {
                rendered.push(k.to_string());
            }
        }
        if let Some(error) = result.get("error") {
            failed.push(format!("onclick {body:?} drawn on {host} threw in the harness: {}", error.as_str().unwrap_or_default()));
            continue;
        }
        for scrim in list(result, "scrims", &what)? {
            let opener = match scrim.as_str() {
                Some(name) => MockOpener::Pages(name.to_string()),
                None => MockOpener::Onclick(body.clone()),
            };
            if !probe.modals.contains(&opener) {
                probe.modals.push(opener);
            }
        }
        for call in list(result, "menus", &what)? {
            let id = call
                .get(0)
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("a UI.openMenu call by onclick {body:?} drawn on {host} was recorded with no menu id"))?;
            let arg = call.get(1).and_then(serde_json::Value::as_str).map(str::to_string);
            if !probe.menus.iter().any(|(m, _)| m == id) {
                probe.menus.push((id.to_string(), arg));
            }
        }
        if let Some(tab) = list(result, "tabs", &what)?.first() {
            let tab = tab.as_str().with_context(|| format!("a PAGES.tab value onclick {body:?} drawn on {host} assigned was recorded as no string"))?;
            tab_of.push((body.as_str(), tab.to_string()));
        }
        let mut keys = Vec::new();
        for k in strings(result, "keys", &what)? {
            if !declared.non_tab_keys.contains(&k) {
                failed.push(format!("onclick {body:?} drawn on {host} assigns PAGES.{k}, which is neither PAGE_MAP's tab nor a NON_TAB_KEYS key"));
            } else if !probe.non_tab_keys.iter().any(|n| n == k) {
                probe.non_tab_keys.push(k.to_string());
            }
            keys.push(k.to_string());
        }
        keys_of.push((body.as_str(), keys));
        for name in strings(result, "excluded", &what)? {
            if !probe.appenders.iter().any(|a| a == name) {
                probe.appenders.push(name.to_string());
            }
        }
        for loose in list(result, "loose", &what)? {
            let error = loose_failure(&what, loose)?;
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

/// The list `field` of the harness's record of `what`.
fn list<'a>(record: &'a serde_json::Value, field: &str, what: &str) -> Result<&'a Vec<serde_json::Value>> {
    record.get(field).and_then(serde_json::Value::as_array).with_context(|| format!("the harness's record of {what} has no {field} list"))
}

/// The string list `field` of the harness's record of `what`.
fn strings<'a>(record: &'a serde_json::Value, field: &str, what: &str) -> Result<Vec<&'a str>> {
    list(record, field, what)?
        .iter()
        .map(|v| v.as_str().with_context(|| format!("the harness's record of {what} has a {field} entry that is no string")))
        .collect()
}

/// The failure of an element `by` appended that no recorded opener and no declared appender drew
/// (`loose`, as the harness recorded it).
fn loose_failure(by: &str, loose: &serde_json::Value) -> Result<String> {
    let element = loose.get("element").and_then(serde_json::Value::as_str);
    let drawers = loose.get("drawers").and_then(serde_json::Value::as_array);
    let (Some(element), Some(drawers)) = (element, drawers) else {
        bail!("an element {by} appended was recorded with no element or drawers");
    };
    let drawers: Vec<&str> = drawers.iter().filter_map(serde_json::Value::as_str).collect();
    let calls = if drawers.is_empty() { "no UI call".to_string() } else { drawers.join(" > ") };
    Ok(format!("{by} appended {element} under {calls}, which no recorded opener drew and no EXCLUDED_MOCK_APPENDS entry names"))
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

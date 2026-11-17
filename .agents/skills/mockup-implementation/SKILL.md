---
name: mockup-implementation
description: Port an admin page to the control-plane portal (core/admin) from its UILayout/redesign mockup without fidelity drift. Use for a provider port's checklist, its view part, a restyle, or a report that the portal diverges from the mockup. Orchestrates checklist extraction → backend part a → view part b on mockup-implementer → golden test, admindiff crawl and diff against the page's accept file.
---

# Mockup Implementation

The mockup is the exact spec for layout, styling and data shape:
`UILayout/redesign/<page>.html`, built by `P.<key>` in
`UILayout/redesign/assets/pages-*.js` from the design-system primitives
(`UILayout/design-system/components/bundle.js`, `bundle.css`, `tokens.json`), with
fake rows in `assets/data.js`. The portal that renders it is `core/admin`.

## Standing decisions (owner-settled; do not re-ask)

- Copy the mockup. No element is dropped, substituted or invented. Every difference
  is fixed or is an accept entry with a reason (below); an undeclared difference is a
  violation.
- Data is decided at checklist extraction from the plan's data-sources table, never
  by the view lane:
  - data a fresh fleet lacks but the module can hold is **seeded** (Seed rule);
  - data the backend has no source for is FAKE per the plan's R1, written in part
    `a`: a pure function of the entity id, explicit-width hash, no clock, no rand,
    marked `// FAKE: <why>`.
- Lanes: `.agents/shared/planning-dispatch.md` **Implementation Mode**. The view part
  runs at the cheap tier on `mockup-implementer`; checklist, contract decisions and
  review stay at the top tier. No fallback to a stronger model: a port that goes
  badly is fixed in this skill, `admindiff` or the golden tests.

## Per port, in order

| # | Who | Does |
|---|---|---|
| 1 | orchestrator, top tier | Checklist (below). Every gap becomes a decision in part `a`. |
| 2 | `core-implementer`, top tier | Part `a`: provider, seed data, the page's `Route::Mapped` entry in `tools/admindiff/src/pages.rs`. |
| 3 | `mockup-implementer`, cheap tier | Part `b`: templates, `admin.js`, `portal.css`, goldens, accept file. |
| 4 | orchestrator | Live sequence (below), then one `core-reviewer` pass at the top tier. |

An open gap is closed in part `a` before `b` starts. If `b` finds one, it stops and
returns it; it is never closed by a stronger `b`.

## 1. Checklist — before part `a`

Read the page's `P.<key>` builder line by line. One row per visible element (example
rows):

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| "Subscriptions" panel title | `pages-platform.js:NN` | `ContributionDecl.title` | ok |
| Lag column bar | `pages-platform.js:NN` | `Cell.bar` + `Tone::Warn` | ok |
| Muted Head value | `pages-platform.js:NN` | none | contract gap → part `a` decides |

- **Carrier** is the `core/controlplane` field that draws it (table below), or the
  template literal.
- **Status** is `ok`, `contract gap`, `data: seed`, `data: FAKE`, or an accept
  kind with its reason.

Per panel, a states row: what the mockup draws in `ok`, `empty`, `readonly`, `stale`
and `error`:

- `empty`: a `Primitive::Empty` title/detail, and the `when_empty` tools.
- `readonly`: `Manifest.writes: false` on a panel with a write affordance
  (`core/admin/src/engine/panels.rs:84-91`).
- `stale`: needs `ContributionDecl.cache_ttl_s`.

Per module: `writes` (true/false) and every tool, meaning each `ActionDecl` with
point, `Emphasis`, `danger`, `implemented` and `then`.

The checklist goes into the part `a` and part `b` prompts and anchors the review.

## 2. Mockup class → carrier

Mapping in `core/admin/src/render.rs` and `templates/primitives.html.tmpl`. Anything
that maps to nothing is a `contract gap` row.

| Mockup markup | Carrier |
|---|---|
| plain value | `CellKind::Text`; `Number` and `Bytes` also render plain (preformatted by the module) |
| `span.mono` | `CellKind::Mono` |
| `span.dim` | `CellKind::Time` |
| `span.tag` | `CellKind::Ref` |
| `span.user-cell` (avatar + name) | `CellKind::User`; avatar colour and initials come from the value |
| `span.badge <ok\|warn\|err\|info>` | `CellKind::Badge` + `Cell.tone`; with no tone the class comes from the value (`render.rs:160-168`) |
| `style="color:var(--<tone>)"` on a value that is not a badge, ref or user | `Cell.tone` |
| `span.lag-bar > i.w\|i.e` + `span.mono` | `Cell.bar` (0–100); `Tone::Warn` → `w`, `Tone::Err` → `e`, else no class |
| `<a href>` around a value | `Cell.link` |
| `th.num` / `td.num` | `Column.align: Align::Right` |
| `tr.sel` | `Row.highlight` |
| `tr.clickable` | `Row.entity` set |
| `btn primary` / `btn` / `btn ghost` | `Emphasis::Primary` / `Default` / `Ghost` |
| `btn danger-ghost` | `ActionDecl.danger: true`, whatever the emphasis |
| disabled `btn` with a title | `implemented: false` (R2); also a denied role or `writes: false` (`engine/actions.rs:80-91`) |
| `kpi-delta span.up\|down\|bad-up\|flat` | `Delta.tone`: `Ok` / `Err` / `Warn` / `Info` or none |
| `svg.spark` colour `--chart-1` / `--chart-2` / `--warn` | `KpiTile.spark`: `Palette::Chart1` / `Chart2` / `Warn` |
| `.kpi .bar > i` | `KpiTile.capacity` |
| `banner <tone>` | `PanelBody.banner` (`Banner.tone`) |
| timeline `span.ico <tone>` | `TimelineEntry.tone` |
| `q-count <tone>` | `QueueState.tone` |
| attention `modal-icon danger\|warn` | `Severity::Err` / `Warn` |
| nav `span.count alert\|warn` | `NavCounter` cell `Tone::Err` / `Warn` |
| service dot and badge | `ServiceState` (`render.rs:1387-1391`) |

## 3. Panel chrome slot order

Mockup: `UILayout/design-system/components/bundle.js:130-149`. Portal:
`core/admin/src/templates/primitives.html.tmpl:371-402` (`panel_fragment`, panel mode).

1. icon: `ContributionDecl.icon`.
2. `.panel-title`: `ContributionDecl.title`, then `: <span class="mono">heading</span>`
   from `PanelBody.heading`.
3. `.panel-meta`: `PanelBody.meta`.
4. `.ro-chip`, in `readonly` only.
5. `.panel-tools`, in this order:
   - chart legend;
   - note (`PanelBody.note`);
   - services value label;
   - `PanelTool` actions, size `sm`.

   Tools draw in `ok`, `stale`, `readonly` and `empty` (the `when_empty` ones only).
6. Banners:
   - the portal's own: `stale` (warn) or `readonly` (info);
   - then the module's `PanelBody.banner` (`ok`/`readonly` only);
   - then a form's error banner.

   The mockup draws only one banner.
7. `.panel-body`, `flush` for a table, timeline, queue, services or attention body
   (`render.rs:701-710`).
8. Footer:
   - a `PropertyList` with `code` puts its actions and note in a row below the code,
     inside the body;
   - a `Timeline` with older entries gets `.panel-foot` with "Load older".

## 4. View-part rules (part `b`)

- Touch only `core/admin/src/templates/*.tmpl`, `admin.js`, `portal.css`, `testdata/`
  and `tools/admindiff/accept/<page>.txt`. Never `core/controlplane`, a module, or
  `pages.rs`: those are part `a`.
- Translate the mockup's JS string builders to minijinja; never copy JS.
- The CSP allows inline `style` but no inline script (`core/admin/src/lib.rs:540`):
  no `<script>` blocks, no `on*=` handlers, no `hx-on:`.
- `admin.js` handles a failure with a delegated `document.addEventListener("htmx:…")`
  listener, as `resetRetries` does for `htmx:sendError` / `htmx:timeout` /
  `htmx:sendAbort` (`admin.js:150-165`, `:441-443`). Never `.then`/`.catch` on an
  `htmx.ajax` promise.
- A new macro, `Primitive` or `PanelState` is not a view change. Stop and return it:
  it lands in part `a`, and its golden case is a gate test written at the top tier.
- Golden test (run the `safe-verification` pre-flight first):
  - for an intended template change, run `ADMIN_BLESS=1 cargo test -p admin golden`.
    It rewrites `core/admin/src/testdata/`, deletes orphans, and then fails by
    design; rerun it without `ADMIN_BLESS`. It refuses to bless when `CI` is set;
  - `cargo test -p admin golden` passes before the commit; commit the `testdata/`
    diff with the template;
  - any other golden failure is a stop: a `MACRO_CASES` signature that no longer
    matches, a case that no longer runs its macro, or a `CellKind` / `Tone` /
    `Emphasis` variant no case draws (`core/admin/src/golden_tests.rs`, drift
    guards in its header). Return it; the case change lands as a top-tier
    test-author step.
- `cargo run -p admindiff -- mock --page <page>` renders the mockup page through
  `node` with no fleet and lists its panels and tiles.
- Never start a fleet: no `devctl up`, no `verifyctl`.

## 5. Seed rule

Part `a` adds the data its page's designed state needs:

- through `demoseed` when the module's public API can produce it;
- otherwise through the module's own operator tool, as `eventctl seed-mockup` does
  for the Event plane.

A `data difference` accept entry is allowed only when neither can produce the data.

## 6. Accept file — `tools/admindiff/accept/<page>.txt`

- Order: `diff` needs a clean crawl (`tools/admindiff/src/lib.rs:170`) and part `b`
  never starts a fleet, so `b`'s first pass commits templates and goldens only. The
  orchestrator then runs crawl and diff (§7) and passes the `NEW` lines in `b`'s
  next prompt.
- One entry per difference: `<difference> | <kind>: <reason>`. `<difference>` is
  the text after the `NEW` tag of a `diff` line, copied verbatim with the tag and its
  padding dropped (`lib.rs:191` prints `  NEW    <difference>`).
- Kinds, as the existing files use them:
  - `data difference`
  - `not ported yet`: names a port step other than the current one. An element the
    module under port owns never takes it (R3, and the pilot needs `diff` clean)
  - `template choice`
  - `contract gap`
  - `declared deviation Dn`
- `#` lines and blank lines are skipped. A line without ` | ` or a duplicate entry is
  an error. An entry that matches no current difference is `STALE` and fails the diff.
- Part `b` writes `not ported yet` entries for elements of later ports, and entries
  of other kinds only when the checklist names them. Any other `NEW` line is fixed
  in the template or returned.
- A port removes the `not ported yet` entries it resolves from every accept file.
  Both `index.txt` and `events.txt` carry the sidebar nav entries; `index.txt` also
  carries the dashboard tiles and the "Recent operator activity" panel.
- The file is the step report's declared-deviations source. The report cites it and
  does not re-list deviations.

## 7. Live sequence — orchestrator only, after the `b` commit

Templates are compiled into the binary (`include_str!`, `render.rs:16-19`), so the
fleet is booted after the commit. Run the `safe-verification` pre-flight first. The
golden test is re-run here as the gate, whatever `b` reported. While the fleet runs,
no Cargo command may start, so every Cargo command comes before it boots. On Windows
use `target\debug\<tool>.exe`; honour `CARGO_TARGET_DIR`.

```
cargo test -p admin golden
cargo build -p demoseed -p eventctl -p admindiff
cargo run -p devctl -- up monolith          # foreground; wait until healthy
target/debug/demoseed
target/debug/eventctl seed-mockup
<the module's operator-tool seed, if part a added one>
target/debug/admindiff crawl                # prints "wrote <path>/target/admindiff/crawl-<secs>"
target/debug/admindiff diff --page <page> --portal <that path>
target/debug/devctl down
```

- Diff every page with a `Route::Mapped` entry, not only the ported one.
- The loop is closed when all three hold:
  - the golden test passes;
  - `crawl` is clean: it wrote the run's `CLEAN` marker, which `diff` refuses to run
    without;
  - `diff` prints `clean` for every mapped page: no `NEW` line, no `STALE` line, at
    least one matched panel.
- `NEW` and `STALE` lines go back to part `b` as a fix round.
- `crawl --base-url http://localhost:8085` checks the split.

## 8. Post-mortem

A post-mortem finds what this skill, the checklist, `admindiff` or the golden tests
missed. Those fixes land before the next port. It runs:

- when the Step 11 (mail) pilot misses any of: `diff` clean, `crawl` clean, a first
  review round that is PASS or comment-only;
- after Step 13 (accounts);
- after any port whose review needs more than one fix round.

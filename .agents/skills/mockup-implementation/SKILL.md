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
  is fixed, is an accept entry with a reason (below), or, for a difference `admindiff`
  cannot see (text, attributes), is a hand-check row (§1); an undeclared difference is
  a violation.
- Data is decided at checklist extraction from the plan's data-sources table, never
  by the view lane:
  - data a fresh fleet lacks but the module can hold is not seeded by a port: it is a
    `data difference` accept entry until plan Step 24b (Seed rule);
  - data the backend has no source for is FAKE per the plan's R1, written in part
    `a`: a pure function of the entity id, explicit-width hash, no clock, no rand,
    marked `// FAKE: <why>`;
  - every `// FAKE:` marker and every `implemented: false` action a port adds gets an
    item in `docs/todo/admin-second-pass.md` in the same part `a` commit; part `a`
    owns it;
  - a port that writes a new `contract gap` accept entry adds its item under
    **Accepted contract gaps** in the same file.
- Lanes: `.agents/skills/implementation-dispatch/SKILL.md`. The view part
  runs at the cheap tier on `mockup-implementer`; checklist, contract decisions and
  review stay at the top tier. No fallback to a stronger model: a port that goes
  badly is fixed in this skill, `admindiff` or the golden tests.

## Per port, in order

| # | Who | Does |
|---|---|---|
| 1 | orchestrator, top tier | Checklist (below). Every gap becomes a decision in part `a`. |
| 2 | `core-implementer`, top tier | Part `a`: provider, the page's `Route::Mapped` entry in `tools/admindiff/src/pages.rs`. |
| 3 | `mockup-implementer`, cheap tier | Part `b`: templates, `admin.js`, `portal.css`, goldens, accept file. |
| 4 | orchestrator | Live sequence (below), then one `core-reviewer` pass at the top tier. |

An open gap is closed in part `a` before `b` starts. If `b` finds one, it stops and
returns it; it is never closed by a stronger `b`.

## 1. Checklist — before part `a`

Read the page's `P.<key>` builder line by line. One row per visible element (example
rows):

- A new page's accept file always needs the shell-frame entries every mapped page
  carries (`tools/admindiff/accept/index.txt`, the `frame div.app > aside.sidebar` and
  `frame div.app > header.topbar` lines):
  - the Sign out form in the sidebar foot (template choice);
  - the theme toggle in the topbar (template choice; the mockup's sits in the devbar,
    which D3 drops);
  - the Design-system nav section (declared deviation D3);
  - the Lore nav section (no Lore module exists);
  - the nav entries and nav sections of ports not landed yet (not ported yet).

  The checklist carries these shell-frame rows and names the `index.txt` entries to
  copy into the new page's file, with their reasons adjusted to that page.

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| "Subscriptions" panel title | `pages-platform.js:NN` | `ContributionDecl.title` | ok |
| Lag column bar | `pages-platform.js:NN` | `Cell.bar` + `Tone::Warn` | ok |
| Muted Head value | `pages-platform.js:NN` | none | contract gap → part `a` decides |

- **Carrier** is the `core/controlplane` field that draws it (table below), or the
  template literal.
- **Status** is `ok`, `contract gap`, `data: FAKE`, or an accept
  kind with its reason.

Per panel, a states row: what the mockup draws in `ok`, `empty`, `readonly`, `stale`
and `error`, plus `narrowed to zero` for a panel with search or filters:

- `empty`: a `Primitive::Empty` title/detail, and the `when_empty` tools.
- `narrowed to zero`: a search or filter leaves no rows; the toolbar must stay. Name
  the golden case that covers it (`table_empty_search` for a `DataTable`); with none,
  it is a gap for part `a`. `crawl` never searches, so `admindiff` cannot see it.
- `readonly`: `Manifest.writes: false` on a panel with a write affordance
  (`core/admin/src/engine/panels.rs:84-91`).
- `stale`: needs `ContributionDecl.cache_ttl_s`.

Per module: `writes` (true/false) and every tool, meaning each `ActionDecl` with
point, `Emphasis`, `danger`, `implemented` and `then`.

Per page, a hand-check section: every element whose difference the §7 `diff` cannot see.
It runs without `--text`, so it compares tags and classes, and `admindiff` never compares
attributes. That covers attributes and inline styles (a `title`, a width), text (a crumb, a
foot count), which header carries the sort arrow, and anything the crawl never opens (the
row-menu popover, the palette). Each row names its check: a golden assertion, or a look at
the live page in §7. A declared deviation no accept entry can carry is declared in this
section, and the step report cites it (§6).

The mock harness never runs a page's `meta.after` hook. On players that hook wraps each
contributed column's header text in a `span` (`pages-core.js:109-112`), so that `th > span`
is never compared. The port that contributes such a column puts its header shape in the
hand-check section.

Per panel, every URL the renderer builds for it (state tabs, search, pager): which
query keys it keeps and which it drops. Every non-pager URL must build from `Ctx.query`
(`core/admin/src/render.rs`, `panel`), which is stripped of the paging keys `cursor`
and `before`; a URL built from the raw `params` instead is a gap for part `a`.

An accept-plan section predicts every accept entry the page needs, each with its kind
and reason: the shell-frame copies above, every declared deviation, contract gap and
data difference, a template-choice pair that repeats another accept file's pair (the
toolbar `span.input-icon` / `form.input-icon` pair of `events.txt`), and a frame
placeholder that the wrapper element the page layout draws (e.g. `div.stack` for
`Layout::Stack`) makes mock-only (`mock-only [panel "<title>"]`). Where the exact `<difference>` text needs a crawl, it names the element
and the kind.

Once the owner decides the checklist's gaps, the checklist file is updated in place
before part `a`: each decided gap gets its decision, and each overruled proposal is
struck through (`~~…~~`). Part `b` reads the checklist literally, so a stale proposal
becomes work.

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
  and `tools/admindiff/accept/*.txt`. Never `core/controlplane`, a module, or
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

There are no per-module seed tools. `demoseed` is the one seeder and writes only
through the public API (plan Step 24b). A port adds no seed. A designed state that a
fresh fleet lacks is a `data difference` accept entry. Step 24b removes the entries its
API seeding resolves. A state no public API can produce, such as a retrying or parked
mail, stays an entry for good. `eventctl seed-mockup` stays as the Event plane's dev
fixture. Do not chase a mockup gap beyond the checklist's decisions; it goes to the next
iteration.

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
    module under port owns never takes it (R3, and the pilot needs `diff` clean).
    An element of a module that does not exist yet says `no <module> module exists`
    instead of a port step, as the Lore entries do
  - `template choice`
  - `contract gap`
  - `declared deviation Dn`
- `#` lines and blank lines are skipped. A line without ` | ` or a duplicate entry is
  an error. An entry that matches no current difference is `STALE` and fails the diff.
- Part `b` writes `not ported yet` entries for elements of later ports. Every other
  entry copies the kind and reason the checklist's accept plan predicts, verbatim; an
  element the checklist names under one kind never gets another.
- A `NEW` line the accept plan does not predict is fixed in the template or returned
  to the orchestrator. The one exception is a line that repeats, word for word apart
  from the panel name, a pair already in another accept file: `b` copies that pair's
  kind and reason and flags the line in its report.
- Part `b` removes, from every accept file, the `not ported yet` entries the port
  resolves. Every mapped page's accept file carries the sidebar and shell-frame
  entries; only `index.txt` carries the dashboard tiles and panels.
- A `STALE` line that is a `not ported yet` entry this port resolves is deleted. Any
  other `STALE` line is returned to the orchestrator.
- The file is the step report's declared-deviations source, together with the
  checklist's hand-check section (§1) for the ones `admindiff` cannot see. The report
  cites both and does not re-list deviations.

## 7. Live sequence — orchestrator only, after the `b` commit

When the generic macros already render the page exactly, `b` changes no template
and makes no commit; this sequence then runs on the part `a` commit.

Templates are compiled into the binary (`include_str!`, `render.rs:16-19`), so the
fleet is booted after the commit. Run the `safe-verification` pre-flight first. The
golden test is re-run here as the gate, whatever `b` reported. While the fleet runs,
no Cargo command may start, so every Cargo command comes before it boots. On Windows
use `target\debug\<tool>.exe`; honour `CARGO_TARGET_DIR`.

```
cargo test -p admin golden
cargo build -p demoseed -p eventctl -p admindiff
cargo run -p devctl -- up monolith          # foreground; wait for `running`
target/debug/demoseed
target/debug/eventctl seed-mockup
target/debug/admindiff crawl                # prints "wrote <path>/target/admindiff/crawl-<secs>"
target/debug/admindiff diff --page <page> --portal <that path>
target/debug/devctl down
```

- Diff every page with a `Route::Mapped` or `Route::FirstRow` entry, not only the ported one.
- The loop is closed when all three hold:
  - the golden test passes;
  - `crawl` is clean: it wrote the run's `CLEAN` marker, which `diff` refuses to run
    without. A `FirstRow` follower that finds no row fails the crawl; on a demoseeded
    fleet that is a defect, and `--allow-empty` is only the opt-out for an empty fleet;
  - `diff` prints `clean` for every mapped and first-row page: no `NEW` line, no `STALE` line, at
    least one matched panel.
- `NEW` and `STALE` lines go back to part `b` as a fix round.
- The split is the at-risk topology and is crawled too: `devctl up split`, then
  `demoseed --base-url http://127.0.0.1:8082` (the split's gateway; its 8080 is
  characters-svc, where `/accounts/register` is a 404), `eventctl seed-mockup`,
  `crawl --base-url http://localhost:8085` and `diff` for every page.

## 8. Post-mortem

A post-mortem finds what this skill, the checklist, `admindiff` or the golden tests
missed. Those fixes land before the next port. It runs:

- when the Step 11 (mail) pilot misses any of: `diff` clean, `crawl` clean, a first
  review round that is PASS or comment-only;
- after Step 13 (accounts);
- after any port whose review needs more than one fix round.

# Plan: navigable admin contribution model (sidebar items, not stacked panels)

**Date:** 2026-06-30 22:37
**Status:** awaiting approval (grumpy-reviewer punch list addressed — see end)

## Problem

The admin portal's contribution model is wrong vs intent and the `UILayout` mockup:

1. **The sidebar is fake.** `modules/admin/admin.html.tmpl:25-65` is a hardcoded static `<nav>` —
   every item (`Players`, `Live Ops`, `Economy`, …) is `muted`/non-clickable; only `Dashboard` is
   `active`. **Contributed modules never appear in the menu.**
2. **Everything stacks on one page.** `modules/admin/admin.go:72-93` (`handleDashboard`, wired to
   `GET /admin` at `admin.go:54`) iterates *all* contributions and renders them piled up
   (`admin.html.tmpl:97 {{range .Sections}}`). So `Characters` + `Inventory`'s KPIs (`Holdings`,
   `Owners`) + tables dump onto a single dashboard — the "Characters, Holdings, Owner, Inventory"
   jumble the user reported.

The contract says "a module = a dashboard panel". The intent is "a module = a clickable sidebar
item under a named section; opening it shows a management view".

## Research (overlapping system mapped — why extend, not add)

We change the existing admin contribution path in place rather than add anything new:

- **Core slot mechanism** (`core/module.go:55,91,97`): `Context.contributions map[string][]any`;
  `Contribute(slot string, v any)` appends; `Contributions(slot string) []any` returns the raw
  slice in **insertion order** (= topological `Init` order), `nil` for an unknown slot. *Why not
  change core:* the slot primitive is exactly right and generic — reused untouched; only the
  contributed *type* changes.
- **Contract** (`modules/admin/adminapi/adminapi.go`): `Slot = "admin.section"` (line 11);
  `Section{Title, Render}` (15); `Content{KPIs, Table*}` (22); `KPI/Table/Cell` (27/33/39). *Why not
  keep `Section` and add `Item` alongside:* the `Section`-as-panel shape is the defect; replace it.
  `Content/KPI/Table/Cell` are reused verbatim — the per-item page still renders them.
- **Sole consumer** (`modules/admin/admin.go:74`): the only `Contributions(adminapi.Slot)` reader.
  Three contributors, all in `Init`, in topological order **accounts → characters → inventory**
  (accounts has no `DependsOn`, registered first in `cmd/server/main.go:55-62`):
  `accounts/accounts.go:118` ("Players"), `characters/characters.go:62` ("Characters"),
  `inventory/inventory.go:88` ("Inventory"). Each `adminSection` is
  `func(ctx context.Context) (adminapi.Content, error)` — shape unchanged by this plan.
- **Routing** (`core/module.go:47-49`, `go.mod:3` = go 1.25): `Context.Mux` is `*http.ServeMux`
  (confirmed). Go 1.22+ patterns work — `"GET /admin/{slug}"` + `r.PathValue("slug")`, no router
  dep. Literal `"GET /admin/theme.css"` outranks `"GET /admin/{slug}"` (strict-subset specificity,
  confirmed — CSS keeps serving). `webui` only owns `GET /` — no `/admin` collision. `gate()`
  (`admin.go:97-112`) wraps a `HandlerFunc` for HTTP Basic; open (with warning) when `ADMIN_USER`
  unset; currently only `GET /admin` is gated.
- **No test references** `adminapi.Section` / `"admin.section"` (confirmed — no test breakage).
- **Stale prose** referencing the old contract: `README.md:90`, `CLAUDE.md:162,171`,
  `admin.go:1-4` package comment, `adminapi.go` package comment — must be updated (Steps 1-2).
- **Lint baseline is currently RED**, not green: `.golangci.yml` (errcheck, standard set) flags the
  existing unchecked `w.Write(themeCSS)` at `admin.go:52` (one of the 15 parked errcheck items).
  This rewrite REPLACES that handler and writes the new admin.go errcheck-clean, so it *reduces* the
  count; we do **not** claim a pre-existing green baseline.

## New model

A contribution becomes a sidebar **Item** grouped under a named **Section**; opening it renders its
management page. First item with a given `Section` creates the group, later items append — the
user's requirement, implemented as grouping by the `Section` string.

New contract (replaces `Section`):

```go
const Slot = "admin.item"   // was "admin.section"

// Item is one clickable entry in the admin sidebar, contributed by a module. The admin groups
// items by Section into the menu; opening an item renders Render() into the content area.
type Item struct {
    Section string // sidebar group label, e.g. "Game Content". First item creates it; rest append.
    Label   string // the clickable menu entry + page title, e.g. "Characters"
    Render  func(ctx context.Context) (Content, error) // the management view (KPIs + Table)
}
```

`Content`, `KPI`, `Table`, `Cell` are **unchanged**.

**Sidebar = dynamic contributed groups + one static "COMING SOON" group** (decision resolving
reviewer #3): rather than trying to merge real modules into the mockup's speculative
MAIN/OPERATIONS/SYSTEM headers (which would split a header across real + stub regions), the unbuilt
mockup items collapse into a single muted **COMING SOON** group. This keeps real, clickable groups
clean and avoids duplicate/!split headers. The visual treatment (theme, fonts, layout) stays
faithful to `UILayout`; only the *taxonomy* of stub items is simplified — recorded here as a
deliberate divergence.

**Section/menu order follows contribution (topological Init) order** — there is no `Order` field
(kept out of the contract for now). With current deps the order is accounts → characters →
inventory, so groups render **Identity (Players)** then **Game Content (Characters, Inventory)**,
and `GET /admin` lands on **`/admin/players`**. If deterministic ordering independent of deps is
needed later, add an `Order int` to `Item` — noted, not done.

Routing: `GET /admin` → 302 to the first item; `GET /admin/{slug}` → that one item's page, its
sidebar entry active, header crumb = its Section, title = its Label.

## Steps

### Step 1 — Rewrite the `adminapi` contract + fix prose refs  `[sonnet]`
- **(a) What:** `modules/admin/adminapi/adminapi.go`: `Slot` → `"admin.item"`; delete `Section`; add
  `Item{Section, Label, Render}`; keep `Content/KPI/Table/Cell` verbatim; update the package doc
  comment ("a module contributes an Item — a sidebar entry"). Also update prose that names the old
  contract: `README.md:90`, `CLAUDE.md:162` and `:171`.
- **(b) Why now:** it's the contract every other file compiles against; changing it first makes the
  compiler enumerate the exact call sites to fix in Steps 2-3 (no missed contributor). Doc fixes ride
  along so the rename never lands half-documented (CLAUDE.md is project law).
- **(c) How:** mechanical type edit per the contract block above; mechanical prose edits
  `Section`→`Item` / `admin.section`→`admin.item` in the three doc spots.
- **(d) Tag:** `[sonnet]`.

### Step 2 — Rebuild the admin handler, routing, and template  `[opus]`
- **(a) What:** `modules/admin/admin.go` + `modules/admin/admin.html.tmpl` (coupled via `pageData`).
- **(b) Why now:** the consumer must match the new contract before contributors are switched, and the
  template's data shape is defined by this handler — they change together.
- **(c) How:**
  - **Delete** the old `handleDashboard` (admin.go:72-93) and `sectionView` (admin.go:66-70) — else
    `unused`/staticcheck U1000 fails the lint gate. Update the admin.go **package doc comment**
    (lines 1-4) to describe the navigable model.
  - New types:
    ```go
    type pageData struct {
        Crumb, Title, Env string
        User   userView
        Groups []navGroup   // dynamic sidebar
        Page   *pageView    // the open item (nil → empty state)
    }
    type navGroup struct { Section string; Items []navItem }
    type navItem  struct { Label, Slug string; Active bool }
    type pageView struct { Title, Err string; KPIs []adminapi.KPI; Table *adminapi.Table }
    ```
    (`pageView.Err` is the explicit error-state field — reviewer #2.)
  - `slugify(string) string`: lowercase; keep `[a-z0-9]`; space/`-`/`_` → `-`; drop others;
    `strings.Trim(…, "-")`.
  - `func (m *Module) items() []resolvedItem`: iterate `m.ctx.Contributions(adminapi.Slot)`,
    type-assert `adminapi.Item` (skip non-matches), compute `slug := slugify(Label)`; if empty →
    `slug = "item"`; **dedupe by looping**: while slug is taken, append/bump a numeric suffix
    (`x`, `x-2`, `x-3`, …) until free (reviewer #7, #9). Keep an ordered slice carrying
    `{section, label, slug, render}`.
  - `buildGroups(items, activeSlug) []navGroup`: preserve first-seen Section order via an ordered
    slice + `section→index` map; set `Active` where `slug == activeSlug`.
  - `handleIndex` (`GET /admin`): if `len(items)==0` render empty-state page (`Page=nil`); else
    `http.Redirect(w, r, "/admin/"+items[0].slug, http.StatusFound)`.
  - `handleItem` (`GET /admin/{slug}`): `slug := r.PathValue("slug")`; find item; if none →
    `http.NotFound(w, r); return`. `content, err := item.render(r.Context())`; on error log + set
    `Page=&pageView{Title:item.label, Err: "failed to load: "+err.Error()}`; else
    `Page=&pageView{Title:item.label, KPIs:content.KPIs, Table:content.Table}`. Build groups with
    `activeSlug=slug`; **check the `m.tmpl.Execute` error** (errcheck).
  - Route registration (`Init`): replace the single `GET /admin` line with
    `ctx.Mux.HandleFunc("GET /admin", m.gate(m.handleIndex))` and
    `ctx.Mux.HandleFunc("GET /admin/{slug}", m.gate(m.handleItem))`. Keep `GET /admin/theme.css`
    public, and **handle its `w.Write` return** (`if _, err := w.Write(themeCSS); err != nil { … }`)
    — this clears the existing errcheck finding at admin.go:52.
  - Template (`admin.html.tmpl`): replace the static `<nav>` body with
    `{{range .Groups}}<div class="nav-group">{{.Section}}</div>{{range .Items}}<a class="nav-item{{if .Active}} active{{end}}" href="/admin/{{.Slug}}"><svg…generic grid icon…/><span>{{.Label}}</span></a>{{end}}{{end}}`,
    then a static **COMING SOON** block: `<div class="nav-group">COMING SOON</div>` + the mockup's
    `Analytics, Live Ops & Events, Economy & Store, Matchmaking & Servers, Leaderboards, Moderation,
    Game Config & Flags` as `nav-item muted` (drop `Dashboard` — obsolete — and `Players` — now a
    real item). Replace the content `{{range .Sections}}` block with a single-page render:
    `{{with .Page}}{{if .Err}}<div class="panel"><div class="empty">{{.Err}}</div></div>{{else}}`
    KPI grid (if `.KPIs`) + one `panel` with `.Title` + table (reusing `{{template "cell"}}`)`{{end}}{{else}}`
    the empty-state panel`{{end}}`.
- **(d) Tag:** `[opus]` — substantive logic (grouping, slug dedupe, routing, error/empty states) +
  template wiring; correctness-critical; session is Opus → separate context = reviewer boundary.

### Step 3 — Switch the three contributors to `Item`  `[sonnet]`
- **(a) What:** one line each:
  - `accounts/accounts.go:118` → `ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "Identity", Label: "Players", Render: m.adminSection})`
  - `characters/characters.go:62` → `…adminapi.Item{Section: "Game Content", Label: "Characters", Render: m.adminSection}`
  - `inventory/inventory.go:88` → `…adminapi.Item{Section: "Game Content", Label: "Inventory", Render: m.adminSection}`
- **(b) Why now:** after the contract + consumer exist, these compile-error until switched; last so
  the compiler confirms completeness. Characters+Inventory share `"Game Content"` → exercises
  section-grouping (two items, one group). `"Identity"`/`"Game Content"` don't collide with the
  COMING-SOON header.
- **(c) How:** mechanical; the `adminSection` methods (`*/admin.go`) are unchanged — still return
  `adminapi.Content`.
- **(d) Tag:** `[sonnet]`.

### Step 4 — Unit tests for the new pure helpers  `[sonnet]`
- **(a) What:** new `modules/admin/admin_test.go`.
- **(b) Why now:** `slugify`, slug-dedupe, and `buildGroups` first-seen ordering are pure,
  correctness-critical, and currently untested (no admin tests exist) — cheap to lock down
  (reviewer #11, guards #1/#7/#9).
- **(c) How:** table tests: `slugify` ("Game Content"→"game-content", "  "→"" , "A/B"→"ab"); dedupe
  (two "Players" → `players`,`players-2`; empty-label → `item`,`item-2`); `buildGroups` preserves
  first-seen section order and sets `Active` on the right slug.
- **(d) Tag:** `[sonnet]` — tests from a specified pattern.

### Step 5 — Verify  `[inline]`
- `go build ./...`; `go vet ./...`; `go test ./...`.
- `go-arch-lint check` → still green (contract stays in `adminapi`; no new cross-module edges).
- `golangci-lint run ./...` → the new `admin.go` is errcheck-clean and clears the old `admin.go:52`
  finding (net −1 vs the parked baseline; we do NOT assert the whole repo is green — the other parked
  errcheck items remain until their sweep).
- Run server against Postgres; `curl`: `/admin` → **302 to `/admin/players`** (accounts is first);
  `/admin/players`, `/admin/characters`, `/admin/inventory` → their pages with the right item active;
  `/admin/nope` → 404; `/admin/` (trailing slash) → 404 (exact `/admin` doesn't subtree-redirect —
  expected, not a bug); `/admin/theme.css` → 200 CSS (literal still outranks `{slug}`). Confirm
  sidebar: **Identity (Players)** then **Game Content (Characters, Inventory)** then muted
  **COMING SOON**, open item highlighted.

## Risks / edge cases
- **Slug collisions / empty label** → resolved: empty → `"item"`; dedupe loops until a free suffix.
- **Empty state** (no contributions) → `/admin` renders empty page, no redirect loop.
- **Render error** on an item → `pageView.Err` error panel, not a 500 or blank.
- **Auth**: both `/admin` and `/admin/{slug}` gated; `theme.css` public.
- **Route specificity / trailing slash**: `/admin/theme.css` keeps ranking above `{slug}`; `/admin/`
  404s (documented, harmless).
- **`html/template` escaping** of `Slug` in `href` — slug is `[a-z0-9-]` only, safe.
- **Menu order** is dep-topology driven (no `Order` field) — acceptable now; documented escape hatch.

## Out of scope
- Kotlin sketch (`experiments/jvm-kotlin-sketch`) — mirror later if desired.
- Per-item custom icons (all contributed items use one generic icon for now).
- The parked golangci-lint `_ =` sweep on the *other* 14 findings + the pending commit.

## Grumpy-reviewer punch list — disposition (think-hard pass)
- #1 verify ordering wrong → **fixed** (accounts first → lands `/admin/players`; Step 5 + New-model corrected).
- #2 `pageView` had no error field → **fixed** (added `Err string` + template branch).
- #3 emergent order / split "Operations" / dropped headers → **resolved** (single COMING-SOON group;
  order = topo, documented; `Order` field noted as future).
- #4 stale docs (README/CLAUDE.md) → **fixed** (folded into Step 1).
- #5 old symbols would trip `unused` → **fixed** (Step 2 explicitly deletes `handleDashboard`/`sectionView`).
- #6 false "green baseline" claim → **fixed** (baseline is RED; rewrite clears admin.go:52, net −1).
- #7 empty-label slug → **fixed** (`"item"` fallback).
- #8 trailing-slash 404 → **documented** in edge cases.
- #9 dedupe re-collision → **fixed** (loop until free).
- #10 admin.go package comment stale → **fixed** (Step 2 updates it).
- #11 no tests for new helpers → **fixed** (Step 4 adds `admin_test.go`).

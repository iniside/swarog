# Plan — Inventory admin: owners list → drill into an owner's items

**Date:** 2026-07-08 00:12
**Status:** DRAFT (pre-review)
**User decision:** Option **B** — separate-page drill-down (not modal; modal deferred as a
later "stress test"). Replace the flat `Owner/OwnerId/Item/Qty` dump with an owners list where
clicking an owner opens that owner's items on its own page.

## Context

### The smell
`inventory.adminSection` (`modules/inventory/admin.go`) renders ONE flat table of up to 50
holdings (`OWNER / OWNER ID / ITEM / QTY`) — a raw dump. The user wants: an owners list →
click an owner → that owner's items.

### What the admin contract supports today (research, 2026-07-08)
`api/admin/adminapi/adminapi.go`: an `Item` = one sidebar entry + one page; `Content` = KPIs +
one `Table` + optional `Form`. **`Cell` has only `Text/Badge/Mono` — no link.** **`Render` is
`func(ctx) (Content, error)` — no request params.** The admin shell (`modules/admin/admin.go`
`handleItem`) matches a slug and calls `cur.render(r.Context())`; no query params reach Render.
So drill-down is impossible without a small extension. The `UILayout/GameOps Admin.dc.html`
mockup provides the visual LANGUAGE (dark theme, cards, tables — already implemented by the
admin shell) but has NO inventory-specific spec, so this is new product design, not a 1:1
mockup translation.

### Why not a different approach (overlap analysis)
- **Multiple flat pages, no contract change:** can't give "this character's items" — a per-owner
  page needs a parameter. Rejected.
- **Modal:** the contract has no modal; needs JS + a fetch endpoint. Deferred (user's stress
  test). Server-rendered separate page fits the declarative model better.
- **Extend `Render` signature** (`func(ctx, params)`): breaks every existing Render. Rejected in
  favour of carrying params via `context` (additive, zero signature change — mirrors how
  `opsapi.WithPlayerID`/`PlayerID` carries identity through ctx).

### The design (single item, param-switched — no second sidebar entry, no back-widget)
ONE inventory item (the existing "Inventory", slug `inventory`) whose Render switches on a
`?owner=` query param: absent → owners list (each owner-id cell links to `inventory?owner=<type>:<id>`);
present → that owner's items. The sidebar "Inventory" link (no param) IS the "back to list"
button. Back-navigation needs no new widget.

## Steps

### Step 1 — Additive `adminapi` extension: `Cell.Link` + ctx-carried params `[opus]`

**(a) What.** `api/admin/adminapi/adminapi.go`:
- Add `Link string` to `Cell` — when set, the admin renders the cell as an anchor to
  `/admin/<Link>` (Link is a slug + optional `?query`, e.g. `inventory?owner=character:123`).
  `Badge`/`Mono` still apply to the link text.
- Add `func WithParams(ctx context.Context, p map[string]string) context.Context` +
  `func Params(ctx context.Context) map[string]string` (a private context-key pair, mirroring
  `opsapi.WithPlayerID`/`PlayerID`). Fully additive — existing `Cell`s (no Link) and existing
  `Render`s (ignore params) are unaffected.

**(b) Why now / order.** The shell (Step 2) and inventory (Step 3) consume both. `adminapi` is a
shared contract (rule 5) — this is ADDITIVE (new field + new funcs, nothing mutated), so apidiff
stays green (rule 6).

**(c) How.** Keep `adminapi` transport-free (imports only `context`/`errors` today; `WithParams`
needs `context` — already imported). Document `Cell.Link` semantics (admin prefixes `/admin/`).

**(d) Dispatch:** `[opus]` — shared-contract change; UI-adjacent (never `[sonnet]`).

### Step 2 — Admin shell: thread query params into Render + render `Cell.Link` `[opus]`

**(a) What.** `modules/admin/admin.go` `handleItem`: before calling `cur.render(...)`, build
`ctx := adminapi.WithParams(r.Context(), flatten(r.URL.Query()))` (flatten `url.Values` to
single values — first value per key) and pass that ctx to render. In the embedded HTML template
(`admin.html.tmpl`, find where a `Cell` renders): when `.Link` is non-empty, wrap the cell text
in `<a href="/admin/{{.Link}}">…</a>` (keep the mono/badge styling on the inner text); else
render as today. Style the link subtly per the existing theme (the GameOps look — a hover
affordance, not a garish link).

**(b) Why now / order.** Depends on Step 1's `WithParams`/`Cell.Link`. Precedes Step 3 (which
relies on params reaching Render + the link rendering).

**(c) How — non-mechanical.** `handleItemPost` also calls `cur.render` (to reach the Form) — it
can pass `r.Context()` unchanged (POST edits don't use `?owner`), OR also thread params for
consistency; thread them for uniformity. The template edit must preserve the existing cell
markup (badge pill / mono span) inside the anchor. Confirm no XSS: `Cell.Link` is
module-authored (not user input) and Go's `html/template` auto-escapes the href — verify the
template uses `html/template` (it should).

**(d) Dispatch:** `[opus]` — template + shell wiring; visual affordance is UI (never `[sonnet]`).

### Step 3 — Inventory: owners list + per-owner drill-down `[opus]`

**(a) What.**
- `modules/inventory/store.go`: add `listOwners(ctx) ([]ownerStat, error)` —
  `SELECT owner_type, owner_id::text, count(*) AS items, coalesce(sum(quantity),0) AS qty
   FROM inventory.holdings GROUP BY owner_type, owner_id ORDER BY owner_type, owner_id LIMIT n`.
  Reuse the EXISTING `list(ctx, Owner)` for the detail page (no new detail query needed).
- `modules/inventory/admin.go` `adminSection(ctx)`: switch on `adminapi.Params(ctx)["owner"]`:
  - **absent** → owners list: KPIs (Holdings, Owners as today) + Table `OWNER / OWNER ID /
    ITEMS / TOTAL QTY`, where the OWNER ID cell has `Link: "inventory?owner=<type>:<id>"`
    (badge on type, mono on id, link on the row's id cell).
  - **present** (parse `"<type>:<id>"`; validate type ∈ {player,character} + id is a uuid, else
    render an error card) → that owner's items via `store.list(ctx, Owner{type,id})`: KPIs
    (Owner type, Owner ID, # items) + Table `ITEM / QTY`. The sidebar "Inventory" link (no
    param) returns to the list — no extra back widget.

**(b) Why now / order.** Depends on Steps 1+2. The visible feature.

**(c) How.** Parse `owner` as `type:id` (split on the first `:`). Guard against a malformed/absent
owner gracefully (error card, not a 500). Keep the KPI/Table declarative — the admin owns the
look. `listOwners` LIMIT to a sane cap (e.g. 200) like `listAll`'s 50.

**(d) Dispatch:** `[opus]` — the store query is small, but the whole feature is one coherent UI
unit; keep it one `[opus]` task (visual/UX judgment on the two views).

### Step 4 — Verify `[opus]`

**(a) What.** `go build ./... && go vet ./... && go-arch-lint check && golangci-lint run ./...`
green (adminapi still in `contracts`, additive — no arch change). Then **drive it live**
(monolith): boot `cmd/server`, register + create a character, grant it an item (dev grant), open
`/admin` → Inventory: confirm the owners list renders, the owner-id links, clicking one opens
`/admin/inventory?owner=character:<id>` showing that character's items, and the sidebar
"Inventory" returns to the list. Screenshot/curl the two pages. apidiff (advisory) must show the
`adminapi` change as ADDITIVE (Cell.Link + funcs) — not breaking.

**(b) Why now / order.** Final proof the drill-down works end to end (the at-risk path is the
admin render, not covered by the split smokes).

**(c) How.** Curl `/admin/inventory` (owners list HTML contains the `?owner=` links) then
`/admin/inventory?owner=character:<id>` (items table). Confirm both render (200 + expected rows).

**(d) Dispatch:** `[opus]` — live UI verification.

## Dispatch summary (for approval)

| Step | Work | Lane |
|---|---|---|
| 1 | `adminapi`: `Cell.Link` + `WithParams`/`Params` (additive) | `[opus]` |
| 2 | admin shell: thread query params + render `Cell.Link` in the template | `[opus]` |
| 3 | inventory: owners list + per-owner drill-down (`listOwners` + param-switch render) | `[opus]` |
| 4 | build/lint + live drill-down verification | `[opus]` |

All `[opus]` — UI work is never `[sonnet]`; the whole thing is one small coherent unit, so it
may be ONE `[opus]` dispatch across the 4 steps rather than four. Commit after the feature is
green + verified.

## Notes / out of scope
- **Modal** — deferred (user's later "stress test").
- **An aggregate "Items" page** (items × total qty across owners) — nice-to-have; NOT in this
  cut (user said "narazie B" = the drill-down). Easy to add later as a second contributed item.
- The `Cell.Link` + `Params` extension is generic — any module's admin can now drill down; this
  isn't inventory-specific plumbing.

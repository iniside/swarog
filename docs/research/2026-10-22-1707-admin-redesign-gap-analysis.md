# Admin redesign — what Swarog must change for the new mockups (research, 2026-10-22)

Input: `UILayout/redesign/` (mock module manifests in `assets/registry.js`, the
contract sketch and open questions in `assets/catalog.js`) on `UILayout/design-system/`.
Compared against the shipped admin (`api/admin/api/src/lib.rs`, `api/admin/rpc/src/lib.rs`,
`modules/admin/src/lib.rs`, `cmd/admin-svc`). Lore screens are out of scope for now (Lore
hosting is parked; see `2026-10-22-1539-lore-hosting-exploration-summary.md`).

Status: findings + proposals for discussion. No decisions recorded yet.

## 1. What the mockups assume

- **Panel envelope.** A module contributes `Contribution { module, point, id, order, title,
  requires: Permission, capabilities.write, fetch(ctx, params) -> Result<Primitive, PanelError>,
  cache: none | last_good(ttl), deadline_ms }`. The portal owns chrome and the seven panel
  states (ok, loading, empty, stale, error, readonly, denied). `Ctx = { operator, entity?, time_range? }`.
- **Closed primitive set** (13): DataTable, EntityDetail, Form, ConfirmAction, SecretReveal,
  KpiTile, Timeline, WorkQueue, PermissionMatrix, ServiceStatus, PropertyList,
  TimeSeriesChart, MenuAction; plus AttentionItem, SearchResult, NavEntry. Compositions
  (EntityTab, SummaryFact, Column, FacetBar, …) are not their own types.
- **Extension points** (18 rows in the catalog): `nav.section/entry`, `search.provider`,
  `dashboard.tile/attention/health/panel`, `notification.source`,
  `entity.<kind>.tab/summary_field/header_action/row_action/list_column`, `page`,
  `SubmitOutcome.reveal`.
- **EntityRef** `kind:id`, with entity kinds declared by their owning module
  (`EntityKind { kind, owner, resolve(ref) }`).
- **ActionDef** `{ id, kind, label, order, danger, multi, when, requires, then: invoke | form | confirm | link }`
  and `SubmitOutcome = Ok{message, reveal?, redirect?} | FieldErrors | Conflict{current_revision}`.

## 2. Shipped admin, verified

- `Content` is a singleton per item (`api/admin/api/src/lib.rs:462-489`): optional kpis, one
  table, one form, one card grid, a context header, a modal point. A module cannot put N
  independent panels on one page.
- One wire method `admin.adminData(params) -> ItemData` (`lib.rs:44-60`) plus the opt-in
  `admin.adminSubmit` (`lib.rs:83-87`). There is no operator identity and no entity context;
  every page's params go to every provider.
- `resolve_items` (`modules/admin/src/lib.rs:1495-1543`) runs on **every** GET/POST and
  fetches **every** remote provider's full content **sequentially** just to learn its section
  and label for the sidebar. There is no per-call deadline beyond the 5 s edge dial
  (`core/edge/src/client.rs:43`) and no cache. One slow peer slows every admin page.
- Failure is already per item (an "unavailable: …" card, `lib.rs:1504-1509, 1783-1787`), and
  a provider without `AdminSubmit` degrades to read-only through `UnknownMethod`.
- `ExtensionKind` is closed: `EntityMenu | ModalActions` (`lib.rs:211`). Only menu entries
  have an order (`priority`); nav sections follow registration order (`build_groups`, L1927).
- Server-side HTML only: minijinja + vendored htmx (modals only) + 61-line `admin.js`, CSP
  `default-src 'self'`, no inline script (`lib.rs:500-514`).
- Operators: `admin.users` (argon2id), sessions with CSRF, lockout; **no roles, no
  permission checks**. Every operator sees and submits everything.
- No player detail page, no global search (the header box is a static decoy,
  `admin.html.tmpl:94-98`), no health view, no charts, no SSE. Audit: `admin.action` events
  for login/logout/form-submit (field names, not values).
- 13 providers are wired into `cmd/admin-svc` (characters, inventory, config, accounts,
  audit, scheduler, apikeys, wallet, notifications, mail, friends, groups, storage). match,
  rating, party and leaderboard have none.

## 3. Contract and portal gaps (the real work)

| # | Gap | Size |
| --- | --- | --- |
| G1 | `Content` singleton → page = layout of N panels, each fetched on its own | large |
| G2 | Split the **manifest** (nav, points, panels, actions, permissions, entity kinds — static) from **data** (per panel, lazy). Today nav needs a full content fetch | large |
| G3 | Wire surface: `manifest()`, `panel(id, ctx, params)`, `invoke(action, ctx, input)`, `search(q, limit)`, optionally `columns(ids)`. Local items should go through the same trait so monolith and split behave identically | medium |
| G4 | Parallel fan-out with a per-panel deadline + portal-memory last-good cache (stale vs error) | medium |
| G5 | `Ctx` carries operator id + granted permissions + EntityRef | small |
| G6 | Operator RBAC: roles on `admin.users`, permission strings declared in manifests, portal checks before fetch (denied), module re-checks on invoke | medium |
| G7 | EntityKind registry + EntityRef; entity detail host with module tabs/facts/actions | medium |
| G8 | Explicit `order` on sections, entries, tabs, tiles | small |
| G9 | Typed primitives instead of HTML strings in deltas/cells (the mock registry passes HTML) | small |
| G10 | Shell: global search palette, attention list, notification bell, breadcrumbs from manifests | medium |
| G11 | CSP: palette, popovers, tabs, row menus, matrix staging need `admin.js` growth (no inline handlers); chart as server-rendered SVG | medium |
| G12 | Design system → minijinja macros + one CSS file. `bundle.js` (HTML strings in the browser) is a mockup runtime, not something to ship | medium |

## 4. Where the mockups conflict with the architecture

- **"dashboard.health — probed by the portal, not the module"** and the service list
  (`wallet-svc`, `mail-svc`, restarts) make the portal topology-aware. In the monolith there
  are no services to probe. Process health belongs to Weles (supervisor); the portal can only
  show per-module reachability derived from manifest/panel fetch outcomes, plus Weles status
  later (its management API is planned, not built).
- **"core.events" as a contributor.** The event plane is core, not a module, and resume/skip
  exist only in `tools/eventctl`. Pausing exists (`core/asyncevents/src/worker.rs:392-419`).
  Someone has to own an admin surface over `asyncevents` subscriptions; candidates: the admin
  module itself (portal-owned panel reading the plane through a core API) or a new ops module.
- **`user:` EntityRefs** assume the player_id → user_id rename (Lore report step 11), which
  has not happened. Today's refs are `player:<uuid>`.
- **Annotation layer and forced states** are mockup tooling; they should not ship (a debug
  toggle at most).

## 5. Domain gaps shown by the screens (backend features that do not exist)

Unverified unless a path is given; to be confirmed per screen before planning.

- accounts: suspend/delete account (no `suspend` in `modules/accounts/src`), sessions list,
  "active players 24 h" aggregate, user detail page.
- wallet: "spent 24 h" aggregate (credit/debit form exists).
- mail: retry/cancel/park/resume — `parked` + requeue exist (`modules/mail/src/store.rs:14, 240`);
  "pause whole queue" does not.
- scheduler: recent fires, fire now, edit (page is read-only today).
- config: revision history view (CAS exists).
- audit: filters by topic/actor.
- notifications: send from admin.
- storage: download archive.
- match: "matches in progress" tile (match has no admin surface).
- Sparklines / time series: no metrics history anywhere; Prometheus `/metrics` is
  point-in-time. Either modules compute daily buckets from their own tables or v1 drops
  sparklines.

## 6. Proposals to discuss

1. **Rendering stays SSR + htmx.** Shell renders immediately; each panel is
   `hx-get="/admin/panel/<module>/<id>?…" hx-trigger="load"`. Independent loading/failure
   falls out of the transport, CSP stays, no SPA build. The design system becomes minijinja
   macros + `theme.css`.
2. **Manifest/data split with a new wire trait** (G2/G3), manifest cached per peer and
   refreshed on reconnect; data fetched only for the visible page, in parallel, with deadlines.
3. **Replace `adminapi` wholesale** (wipe rule: no compat layer). Port modules one per step;
   the open question is whether intermediate commits keep both contracts (temporary dual
   surface) or the port lands as one series that switches the portal at the end.
4. **Closed primitive enum in `adminapi`.** A new primitive is a contract change, not a
   module-side extension.
5. **Operator RBAC in v1**, but minimal: permissions declared by modules, roles stored in
   `admin.users`, portal-side check plus module-side check on invoke.
6. **Health = derived reachability now, Weles later.** Drop the portal probe.
7. **Event plane admin**: decide the owner before planning that screen.
8. **v1 cut**: shell + nav + search + entity detail + DataTable/Form/ConfirmAction/
   SecretReveal/KpiTile/PropertyList/Timeline/WorkQueue. Defer sparklines/charts,
   PermissionMatrix (only Lore and maybe apikeys need it), list_column, notifications bell.
9. **EntityRef kind naming**: introduce `EntityRef` with today's `player` kind, rename in the
   player_id → user_id sweep — or do that rename first.

## 7. Considered: Topcoat (tokio-rs/topcoat)

Checked 2026-10-22 from the README and docs.rs (0.10.0, 2026-10-03, MIT, "early-stage and
experimental, expect breaking changes"). Server-rendered async `#[component]` functions in a
`view!` macro; `$(...)` expressions are compiled to browser JS; `#[shard]` components are
re-rendered on the server and swapped in; `live!`/`emit!` stream partial updates; optional
Datastar transport over SSE; `#[memoize]` dedupes per-request fan-out.

Fit:
- It replaces only the rendering layer (minijinja + htmx today). It does not solve G1–G7:
  components assume in-process data access, while admin-svc reaches modules only over the
  wire, so modules would still return typed primitives.
- It runs its own server (`topcoat::start`/`serve`); docs.rs shows no way to mount it into an
  existing router, and axum is not a listed dependency. Admin is mounted into the shared
  `core/app` HTTP server. Unverified whether embedding is possible.
- Client code is "compiled to JavaScript and included with the page"; whether that is inline
  or uses eval is undocumented. Either would break today's CSP (`default-src 'self'`, no
  inline script, no eval). Unverified.
- Pre-1.0 with breaking changes on a security-sensitive surface.

Worth borrowing as ideas: shard-style per-panel server re-render (what proposal 1 does with
htmx), memoized fan-out per request, SSE patches for live panels (Timeline `live`).

## 8. Owner decisions (2026-10-22)

- **Redesign admin and its API.** Admin is promoted from one module among many to a
  standing platform component (a control plane; not called "backplane", which already
  names the push backplane in `core/remote`). Its contribution seam lives in `core`;
  modules still never import the portal.
- **One consumer.** Nobody will write their own admin panel. Extensibility is inbound only
  (modules contribute panels); there is no outward, UI-neutral or stable data API. Primitives
  may map 1:1 to design-system components; contract changes are wipes.
- **Weles integrates from its own side only.** It exposes its own API; the portal reads it
  for process status. Weles is not a consumer of, or contributor to, the admin contract.
- **Operators are admin-only records.** `admin.users` stays its own identity store, never
  linked to `accounts` or any global identity. What crosses the wire is only: the call came
  from the portal (mTLS edge / local call; an admin-only `AuthReq` variant) and the operator
  login as a plain audit label. Permissions are checked once, in the portal, before fetch or
  invoke; modules declare the permission a panel/action needs and do not re-check it.
- **Fixed roles in v1** (e.g. admin / operator / viewer). Editable roles with permission sets
  (full RBAC) are a separate, later feature.
- **Missing actions are disabled and marked.** Mockup actions with no backend behaviour are
  ported 1:1 but rendered disabled with a "not implemented" hint and a FAKE comment; each
  becomes a per-module follow-up, outside the redesign plan.
- **Lore elements on in-scope screens render as marked fakes** (dashboard tiles, Lore role
  column, user Lore tab, header fact, Lore actions, search provider), strict 1:1.
- **Edge trust stays as is.** Every mTLS peer is equally trusted (`core/edge/src/tls.rs:174`);
  "only the portal may call admin ops" is not enforced per method. Admin ops stay wire-only,
  so the gateway never routes them; the operator login is a plain request field.

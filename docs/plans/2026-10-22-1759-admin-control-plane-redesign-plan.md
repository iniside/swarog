# Admin control plane — redesign of the admin portal and its contract (plan)

## Context

### The owner's shape (his words, condensed)

- "Trzeba zrobić redesign admin i api pod niego" — redesign the admin portal and its API.
  Admin is promoted from one module among many to a standing platform component (a
  **control plane**; "backplane" is taken by the push backplane in `core/remote`).
- "Ktoś może chce sobie napisać własny admin panel. W praktyce nikt tego nie będzie robił" —
  **one consumer**. Extensibility is inbound only (modules contribute panels). No outward,
  UI-neutral, stable data API; contract changes are wipes.
- "Weles może być zintegrowany tylko od drugiej strony. Wystawia swoje API i tyle" — Weles is
  not a contributor or consumer of this contract. Its management API is not built yet.
- "Admin panel dostaje własne rekordy, nie musi być powiązany z żadną globalną tożsamością" —
  operators stay in `admin.users`. Only "the call came from the portal" and the operator login
  (as an audit label) cross the wire. The portal alone checks permissions.
- "Wystarczą stałe na v1, pełny RBAC to duży feature" — fixed roles admin / operator / viewer.
- "Mockup przenosisz 1:1 tak blisko jak się da… jak część informacji nie może być wyświetlona
  w prezentowany sposób, używasz mockowanych danych i je oznaczasz" — **no artistic freedom**.
  Real field wins; otherwise a deterministic fake marked `FAKE` in a code comment.
- Missing actions: **ported 1:1, rendered disabled with a "not implemented" hint, marked**.
- Lore elements on in-scope screens: **render as marked fakes**.

### Rules derived from the owner's shape (apply to every UI step)

- **R1 — data.** Per the data-sources table: REAL renders real; DERIV (a new read over existing
  columns, no new column/index/table) is implemented; FAKE and NEW **data** render as
  deterministic fakes marked `// FAKE: <why>` (pure function of the entity id, explicit-width
  hash, no clock/rand). No new columns, tables or indexes in this plan other than
  `admin.users.role` (Step 4).
- **R2 — actions.** REAL actions are wired. NEW actions are declared with
  `implemented: false` and rendered disabled with the hint "Not implemented yet"; the portal
  never posts them.
- **R3 — fidelity.** Every element of the mockup page is present; every difference is a
  declared deviation in the step's report.

### Exclusions

- Lore screens (`lore-*.html`) and the Lore module. Only Lore fragments on in-scope screens
  render, as fakes.
- New domain behaviour behind NEW actions (suspend, rotate key, scheduler edit, mail
  pause/park, grant item, revoke sessions, download saves, checkpoint rewind…).
- Full RBAC, editable roles; Weles management API; per-method edge caller restriction.
- player_id → user_id rename. Entity refs keep the `player` kind.
- `catalog.html`, `states.html`, the annotation layer and the forced-state picker (mockup tooling).

### Sources (research, committed)

- `docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md` — gaps, conflicts, §8 decisions.
- `docs/research/2026-10-22-1830-admin-redesign-data-sources.md` — per-element
  REAL / DERIV / FAKE / NEW table. **The data-source checklist for every UI step.**
- Mockup `UILayout/redesign/` on `UILayout/design-system/`; contract sketch
  `UILayout/redesign/assets/catalog.js:12-293`.

### Overlap map — why not extend X

| Existing | What it does | Why not extend / depend on it |
| --- | --- | --- |
| `adminapi` + `adminrpc` (`api/admin/*`) | Singleton `Content` per item, `AdminData`/`AdminSubmit`, `EntityMenu`/`ModalActions` | Its shape is the defect: one table + one form per page, nav learned by serially fetching every provider's full content on every request (`modules/admin/src/lib.rs:1495-1543`), no operator, no entity context. Replaced (wipe), not extended; each module's old rail is deleted in its own port step. |
| `opsapi` DESCRIBE + gateway refresh (`modules/gateway/src/lib.rs:1700-1860`, stop grace `:205-270`, `:613`) | Static per-peer manifest, refreshed in one wave every 5 s with a 6 s per-peer timeout, last-good kept | Right pattern for the manifest cache, wrong content. The pattern is copied; the type is not reused. |
| `opsapi::AuthReq` / `Identity` | Gateway bearer gate for `#[http]` ops; envelope identity = player id | Control-plane ops are wire-only, so the gateway never routes them (routes come from `#[http]` op manifests only; the bearer gate is `modules/gateway/src/lib.rs:997-1015`). No `AuthReq` variant (declared deviation D1). Operator login is a request field, never an `Identity`. |
| `tools/eventctl` (`tools/eventctl/src/lib.rs:79-330`) | info, snapshot, retry, pause, resume, retire, skip, bump_generation | The portal needs the same operations; archcheck allows `asyncevents.` SQL only in `core/asyncevents`, `tools/eventctl`, splitproof (`tools/archcheck/src/main.rs:1649-1656`). Relocated into `core/asyncevents`; eventctl becomes a thin CLI. |
| `remote::Stub` + `RemoteFactory` | Per-provider lazy reconnecting caller | Reused as-is. |
| `admin.action` (`api/admin/events/src/lib.rs:21`) | Operator audit trail consumed by `audit` | Reused unchanged. |
| `admin.reveals` | Show-once secret PRG flash | Reused for `SecretReveal`. |

### Verified facts the steps rely on

- Edge calls have no response deadline (`core/edge/src/client.rs:113-154`); the portal wraps
  every provider call in `tokio::time::timeout`.
- Every mTLS peer is equally trusted (`core/edge/src/tls.rs:174-176`); accepted (§8).
- Admin mounts its axum 0.7 router in `init` (`modules/admin/src/lib.rs:388`,
  `core/lifecycle/src/context.rs:137`).
- `asyncevents.subscriptions` holds state, cursor triple, `consecutive_failures`, `last_error`
  (`core/asyncevents/src/store.rs:101-115`).
- `eventctl resume` deliberately leaves the failure count to `retry`
  (`tools/eventctl/src/lib.rs:171-173`); `retry` sets `consecutive_failures = 0,
  next_attempt_at = NULL, last_error = NULL` (`:152-160`). That CLI semantic is kept.
- Config edits are local-only today (`modules/config/src/lib.rs:524-561`); every
  control-plane invoke goes over the wire, which closes this in the split.
- Contract scanners walk only `api/*/api`: `tools/topiccheck/src/golden.rs:520-594`,
  `tools/conformance/src/input_inventory.rs:94-111`, `tools/opscatalog-gen/src/main.rs:185-196`.
- `contrib::Slot::new(` is allowed only in `SLOT_OWNER_FILES` (`tools/archcheck/src/main.rs:60-68`, enforced `:1310`).
- `admin.users` bootstrap callers: `tools/devctl/src/supervisor.rs:405-427`, `install.sh:42`,
  `install.ps1:54`, `weles/fleet.split.toml:39`, `weles/fleet.monolith.toml:24`,
  `tools/splitproof/src/main.rs:965`.
- rpc-macro accepts zero-arg methods and one struct argument with `Result<T, opsapi::Error>`
  (`tools/rpc-macro/src/lib.rs:87-96`); `#[retry_safe]` → `OnceAfterReconnect`, otherwise `Never`.

## Design

### Crates

- `core/controlplane` (package `controlplane`): edge-free vocabulary, `SLOT`, the
  `#[rpc(prefix = "cp")]` trait. Deps: `contrib`, `opsapi`, `rpc-macro`, `serde`, `serde_json`,
  `async-trait`, `thiserror`.
- `core/controlplane-rpc` (package `controlplanerpc`): generated glue, `register`, `remote_factory`.
  Deps: `controlplane`, `opsapi`, `edge`, `remote`, `lifecycle`, `registry`, `rpc-macro`.
- Deleted in Step 3: `api/admin/api`, `api/admin/rpc`. `api/admin/events` stays.

### Wire trait

```rust
#[rpc(prefix = "cp")]
#[async_trait]
pub trait ControlPlane: Send + Sync {
    #[retry_safe] async fn manifest(&self) -> Result<Manifest, opsapi::Error>;
    #[retry_safe] async fn panel(&self, req: PanelRequest) -> Result<Primitive, opsapi::Error>;
    #[retry_safe] async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeOutcome, opsapi::Error>;
}
pub const SLOT: contrib::Slot<Provider> = contrib::Slot::new("controlplane.provider");
#[derive(Clone)] pub struct Provider { pub module: String, pub handler: Arc<dyn ControlPlane> }
```

- `EntityRef { kind: String, id: String }`, rendered `kind:id`.
- `PanelRequest { contribution: String, entity: Option<EntityRef>, params: BTreeMap<String, String> }`.
  No operator and no role: modules do not re-check permissions (§8).
- `InvokeRequest { action: String, entity: Option<EntityRef>, input: BTreeMap<String, String>,
  expected_revision: Option<u64>, reason: Option<String>, operator: String }`.
  `operator` is only an audit label the module may store (e.g. wallet `reason`).
  Idempotency stays a module concern: a form that needs it carries a hidden field
  (`FormSpec.hidden`), as wallet's `_idem_*` does today.
- `InvokeOutcome = Ok { message: String, reveal: Option<SecretReveal>, redirect: Option<EntityRef> }
  | FieldErrors(BTreeMap<String, String>) | Conflict { current_revision: u64 }`.
  HTTP mapping in the portal: `Ok` → 303 PRG; `FieldErrors` → 422 re-rendered form;
  `Conflict` and `opsapi::Status::Conflict` → 409 (the outcome variant carries the revision,
  the status is the transport fallback); `Status::NotFound` → 404; anything else → error toast.
- `SearchRequest { kind: Option<String>, q: String, limit: u32 }`;
  `SearchHit { target: EntityRef, title: String, subtitle: String, badge: Option<String> }`.
- `Role` (portal-side only) = `Viewer < Operator < Admin`.

### Manifest (static; cached by the portal)

```text
Manifest { module, sections: Vec<NavSection>, entries: Vec<NavEntry>, pages: Vec<PageDecl>,
           entity_kinds: Vec<EntityKindDecl>, contributions: Vec<ContributionDecl>,
           actions: Vec<ActionDecl>, search: Vec<SearchDecl>, writes: bool }
NavSection     { id, label, order: i32 }
NavEntry       { section, id, label, icon, order: i32, page, counter: Option<String> } // counter = contribution id
PageDecl       { id, title, subtitle, head_actions: Vec<String>, layout: Layout }
Layout         = Stack(Vec<PanelRef>) | Grid21(Vec<PanelRef>, Vec<PanelRef>)
               | Grid2(Vec<PanelRef>, Vec<PanelRef>) | Tiles(Vec<PanelRef>)
PanelRef       = String   // a contribution id of the same module
EntityKindDecl { kind, label, icon }
ContributionDecl { id, point: Point, order: i32, title, icon, min_role: Role,
                   cache_ttl_s: Option<u32>, deadline_ms: u32 }       // default 2000
Point          = Page(String) | DashboardTile | DashboardAttention | DashboardPanel
               | EntityTab(String) | EntitySummary(String) | EntityListColumn(String) | NavCounter
ActionDecl     { id, point: ActionPoint, label, icon, order: i32, danger: bool, multi: bool,
                 min_role: Role, then: Then, implemented: bool }
ActionPoint    = RowAction(String) | HeaderAction(String) | PageHead(String) | PanelTool(String)
Then           = Invoke | Form(FormSpec) | Confirm(ConfirmSpec) | Link(String)
FormSpec       { fields: Vec<Field>, hidden: BTreeMap<String, String>, submit_label: String, note: Option<String> }
Field          { key, label, kind: FieldKind, required: bool, help: Option<String>,
                 default: Option<String>, readonly: bool, options: Vec<String> }
FieldKind      = String | Text | Int | Float | Bool | Enum | Multi | Json | Ref(String) | Money | Duration
ConfirmSpec    { title, body, tone: Tone, impact: Vec<(String, String)>, reason: ReasonReq,
                 type_to_confirm: Option<String>, confirm_label }       // catalog.js:70-76
SearchDecl     { kind, label, icon, deadline_ms: u32 }                  // default 800
```

`PageDecl` routes are unique per module; `EntityListColumn` is fetched once per table page
with `params["refs"]` = comma-joined ids (batched, catalog.js:249).

### Primitive (closed enum; fields are `catalog.js` "Data it needs", 1:1)

`DataTable` (`catalog.js:17-27`), `PropertyList` (`:176`), `KpiTile` (`:97-103`), `Timeline`
(`:113-118`), `Form(FormSpec)`, `WorkQueue` (`:128-135`), `ServiceStatus` (`:163-167`),
`TimeSeriesChart` (`:185`), `AttentionItems(Vec<AttentionItem>)` (`:210`),
`ColumnCells(BTreeMap<String /*entity id*/, Cell>)`, `Empty { title, detail }`.
`Cell { kind, value, tone, link: Option<EntityRef> }` (`:25-27`). `SecretReveal` (`:86`).
No HTML crosses the wire. `PermissionMatrix` is not built (Lore screens only).

### Portal (modules/admin)

- Routes: `GET /admin` (dashboard), `GET /admin/p/{module}/{page}`,
  `GET /admin/e/{kind}/{id}?tab=`, `GET /admin/_panel/{module}/{contribution}` (htmx fragment,
  `hx-trigger="load"`), `GET /admin/_menu/{kind}/{id}`, `GET /admin/_search?q=&kind=`,
  `GET /admin/_action/{module}/{action}` (modal fragment), `POST /admin/_action/{module}/{action}`
  (CSRF → role → `implemented` → invoke → PRG); unchanged `/admin/login`, `/admin/logout`.
- Manifest cache: a task spawned in `start` refreshes every provider in one wave every 5 s,
  each under a 6 s timeout, keeping last-good (pattern `modules/gateway/src/lib.rs:1700-1860`);
  `stop` aborts it within the module stop budget (pattern `:205-270`, `:613`).
- Panel states: role below `min_role` → `denied`, no call; timeout or error with a last-good
  copy younger than `cache_ttl_s` → `stale`; otherwise → `error`; `Status::NotFound` or a
  zero-item payload → `empty`; `writes == false` → `readonly`; else `ok`.
- Roles: `admin.users.role text NOT NULL CHECK (role IN ('admin','operator','viewer'))` edited
  in place in the DDL (wipe; no default). `adminctl create-user` gains `--role` (default `admin`:
  it is the bootstrap tool) and never changes an existing user's role; `adminctl set-role`
  does. Bootstrap callers pass `--role admin` explicitly.
- Rendering: minijinja macros, one per primitive, markup lifted from
  `UILayout/design-system/components/bundle.js`; CSS served from `include_str!` of
  `UILayout/design-system/tokens.css` and `components/bundle.css`. `admin.js` (data-attribute
  delegation only, CSP unchanged): menus, popovers, Ctrl-K palette, confirm with
  type-to-confirm, reveal acknowledgement, theme toggle.
- Portal-owned provider `portal` inside `modules/admin`: dashboard host, Needs-attention host,
  Event plane page (module label `core.events`, real via `core/asyncevents::admin`), and the
  marked fakes in `modules/admin/src/fakes.rs`: service health list + health pill, notification
  bell, "Matches in progress" tile, every Lore fragment.

## Steps

Intermediate steps gate on `cargo check -p <touched crates>` only; the workspace-wide
`verifyctl` runs once, in Step 24, after the tests are rewritten (Step 22). Steps 3–21 leave
splitproof's admin assertions red by design until Step 22.

### Step 1 — `core/controlplane` + `core/controlplane-rpc` + gates that see them

(a) New `core/controlplane/{Cargo.toml,src/lib.rs}`, `core/controlplane-rpc/{Cargo.toml,src/lib.rs}`;
workspace `Cargo.toml`. Gates:
- `tools/archcheck/src/main.rs:60-68` `SLOT_OWNER_FILES` + `core/controlplane/src/lib.rs`.
- `tools/topiccheck/src/golden.rs`: `rpc_modules` entry labelled `controlplane::control_plane_rpc`
  (the self-check derives labels with `to_snake(trait)`, `:543`); scanner `:520-545` and
  self-check `:569-594` extended to `core/controlplane/src`; `tools/topiccheck/Cargo.toml`.
- `tools/conformance/src/input_inventory.rs:94-111` scans `core/controlplane/src` (and its
  `served_domains` filter admits domain `cp`); `tools/conformance/src/policy.rs` `InputPolicy`
  rows for `cp.panel` params, `cp.search` q, `cp.invoke` input / reason / operator with the
  byte caps `admin.adminSubmit params.<value>` has (`policy.rs:82-86`); keyed exactly as the
  scanner reports nested struct fields (the implementer matches `input_inventory.rs` output).
- `tools/conformance/src/checks.rs`: new tripwire `CONTROL_PLANE_WRITERS` (initially empty) —
  the set of modules whose `modules/<m>/src/admin.rs` declares `writes: true` (text scan), each
  of which must ship `InputByteCaps` coverage for `cp.invoke`; the same shape as
  `ADMIN_SUBMIT_MODULES` (`:100-150`). Writing port steps add their module.
- `tools/opscatalog-gen/src/main.rs:71-72` adds `controlplane` beside `adminapi`.
- `tools/verifyctl/src/stages/public_api.rs:201-209` `discover` also yields `core/controlplane`.
(b) Everything compiles against these types; gates must see `cp.*` before any provider exists.
(c) Types and trait as in **Design**; glue via `rpc_macro::generate_glue` as in
`api/admin/rpc/src/lib.rs:21-27`; `controlplanerpc::register(server, Arc<dyn ControlPlane>)`;
`controlplanerpc::remote_factory(provider)` contributes `Provider { module, handler: Arc::new(Client::new(caller)) }`.
Bless public-api (new `controlplane.txt`) and contract-golden (`cp.*` lines) per `safe-verification`.
(d) `[opus]` core-implementer.

### Step 2 — Relocate event-plane operations into `core/asyncevents`

(a) New `core/asyncevents/src/admin.rs` (`pub mod admin`): `info`, `snapshot`, `retry`, `pause`,
`resume`, `retire`, `skip`, `bump_generation` moved from `tools/eventctl/src/lib.rs:79-330` with
SQL and doc comments unchanged; new `poison_event(pool, id) -> Result<Option<PoisonEvent>>`
(the `skip` predicate `:265-275` without the update) and new `resume_retry(pool, id)` (one
UPDATE: `state = 'active', consecutive_failures = 0, next_attempt_at = NULL, last_error = NULL,
updated_at = now()`). `tools/eventctl` keeps CLI parsing/printing over `asyncevents::admin`.
`tools/eventctl/src/lib_tests.rs` moves to `core/asyncevents/src/admin_tests.rs` (a move of
existing tests, no new assertions). `tools/archcheck/src/main.rs:1656` drops `tools/eventctl`
from the SQL allowlist; doc comments at `:141` and `:1649` updated.
(b) Step 7's Event plane page reads through it.
(c) Move, not copy. `resume` keeps its documented semantic; the CLI does not change.
(d) `[opus]` core-implementer.

### Step 3 — Raze the old admin contract

(a) In every provider (`accounts`, `characters`, `inventory`, `wallet`, `friends`, `groups`,
`storage`, `notifications`, `mail`, `apikeys`, `config`, `scheduler`, `audit`):
`modules/<m>/src/lib.rs` — remove the `adminapi::SLOT` contribution and the `register_admin*`
lines in the `EDGE_SLOT` closure (e.g. `modules/wallet/src/lib.rs:344-345`,
`modules/apikeys/src/lib.rs:322-323`, `modules/groups/src/lib.rs:219-220`,
`modules/mail/src/lib.rs:258,275-276`); delete `modules/<m>/src/admin.rs`; `api/<m>/rpc/src/lib.rs`
— remove the `adminrpc` re-exports; drop `adminapi`/`adminrpc` from `modules/<m>/Cargo.toml`,
`api/<m>/rpc/Cargo.toml`, `api/accounts/api/Cargo.toml:22`, `api/characters/api/Cargo.toml:22`;
delete `PLAYERS_ROW_MENU` (`api/accounts/api/src/lib.rs:282`), `CHARACTERS_CARD_MENU`,
`CHARACTER_MODAL_ACTIONS` (`api/characters/api/src/lib.rs:97,106`); delete the module tests
that exercise the deleted admin code (deletion only; replacements are Step 22).
`modules/admin/src/lib.rs`: delete the old engine (`resolve_items`, `collect_extensions`,
`build_menu`, `page_view`, slug routes `/admin`, `/admin/:slug`) and its tests in
`modules/admin/src/tests.rs`; login, session, CSRF, lockout, rate limit, reveals stay.
`cmd/admin-svc`: stubs temporarily carry no factory (Step 4 adds the new one).
Delete `api/admin/api`, `api/admin/rpc`; `tools/admincheck/src/main.rs` reduced to a stub
`main` that exits 0 (rewritten in Step 5). Tool entries: `tools/archcheck/src/main.rs:60-68`
(drop `api/admin/api/src/lib.rs`), `tools/topiccheck/src/golden.rs:133-144`,
`tools/topiccheck/Cargo.toml:53`, `tools/opscatalog-gen/src/main.rs:71-72` + `Cargo.toml:18`,
`tools/conformance/src/{checks.rs:100-150 (ADMIN_SUBMIT_MODULES), policy.rs:82-86, main.rs:93-97, tests.rs:227-238}`.
Delete `docs/reference/public-api-baseline/adminapi.txt`; re-bless `accountsapi`,
`charactersapi`; contract-golden drops `contracts.txt:336-337`.
(b) Before the new engine, so no step ever has two admin contracts on a provider edge.
(c) Pure deletion plus the minimal edits that keep the touched crates compiling.
(d) `[opus]` core-implementer (it removes a security tripwire and a write surface).

### Step 4 — Portal engine, roles, admin-svc wiring (no visuals)

(a) `modules/admin/src/engine/{manifests.rs,panels.rs,actions.rs,search.rs,roles.rs}`,
`modules/admin/src/routes.rs`; DDL `admin.users.role text NOT NULL CHECK (role IN
('admin','operator','viewer'))` (shared `USERS_DDL`, `modules/admin/src/lib.rs:186-190`).
`tools/adminctl/src/{lib.rs,main.rs}`: `create-user <name> [--role R]` inserts with `R`
(default `admin`); on conflict it resets the password only and **never changes the role**;
new `set-role <name> <role>` changes the role. Bootstrap callers pass `--role admin`:
`tools/devctl/src/supervisor.rs:416`, `install.sh:42`, `install.ps1:54`,
`weles/fleet.split.toml:39`, `weles/fleet.monolith.toml:24`, `tools/splitproof/src/main.rs:965`.
`cmd/admin-svc/src/{lib.rs,main.rs}` + `Cargo.toml`: the 13 stubs use
`controlplanerpc::remote_factory`.
(b) Templates (Step 6) render what the engine resolves; admin-svc switched now makes the pilot
provable in the split (Step 9).
(c) Implements **Portal**: routes, manifest cache task (spawned in `start`, aborted in `stop`),
panel states, invoke mapping, CSRF on every POST, `implemented: false` → 403 without a call,
`admin.action` emit on invoke (`actor` = login, `action` = `{module}.{action}`, `target` = ref,
`detail` = reason), reveal through `admin.reveals`.
(d) `[opus]` core-implementer.

### Step 5 — admincheck rewrite

(a) `tools/admincheck/src/main.rs`, `Cargo.toml`.
(b) Validates every manifest from the first port onward.
(c) Builds every process as today, collects `controlplane::SLOT`, calls `manifest()` on each
local provider; checks: unique ids per module; every `Point`/`ActionPoint` entity kind is
declared by some manifest's `entity_kinds`; every `Layout` `PanelRef` names a contribution of
that module with `Point::Page(page.id)`; every `NavEntry.page` exists; every `NavEntry.counter`
names a `Point::NavCounter` contribution. `--strict` exits 1 on any finding.
(d) `[opus]` core-implementer.

### Step 6 — Shell, primitives, assets

(a) `modules/admin/src/templates/{shell,primitives,modal,login}.html.tmpl`, `modules/admin/src/admin.js`;
delete `modules/admin/src/{theme.css,admin.html.tmpl,modal.html.tmpl,login.html.tmpl}`.
(b) Every screen step renders through these macros.
(c) One macro per primitive plus Panel chrome with the seven states, markup lifted from
`bundle.js`; shell per `registry.js:134-226`. No annotation layer, no state picker.
(d) `[opus]` mockup-implementer (skill `mockup-implementation`; checklist = mockup line ranges + R1–R3).

### Step 7 — Portal-owned screens

(a) `modules/admin/src/{portal_provider.rs,events_page.rs,fakes.rs}`.
(b) Dashboard and Event plane are portal-owned; module tiles plug in at their port steps.
(c) Dashboard (`pages-core.js:45-69`) hosts tiles, attention, activity, health. Event plane
(`pages-platform.js:130-157`): real pause / `resume_retry` / retry / skip via
`asyncevents::admin`; rewind `implemented: false`; FAKE per the data-sources table.
`fakes.rs`: service health list and pill, notification bell, "Matches in progress" tile, every
Lore fragment (values from `data.js`, each `// FAKE`). The portal provider's `writes: true`
adds `admin` to `CONTROL_PLANE_WRITERS`.
(d) `[opus]` mockup-implementer.

### Step 8 — Pilot port: mail

Port-step template (Steps 8, 10–21): (a) new `modules/<m>/src/admin.rs` implementing
`ControlPlane`; `modules/<m>/src/lib.rs` contributes `controlplane::SLOT` and calls
`<m>rpc::register_control_plane` in its `EDGE_SLOT` closure; `api/<m>/rpc/src/lib.rs` adds
`pub use controlplanerpc::register as register_control_plane`; `controlplane`/`controlplanerpc`
deps; a writing module adds itself to `CONTROL_PLANE_WRITERS`. No test files (Step 22).
(b) After Steps 4–7. (c) Panels, actions, data per the data-sources table and R1–R3.
(d) `[opus]` mockup-implementer.

Mail: Outbox page (`pages-platform.js:53-71`), tile, attention. REAL: counts, rows, cancel
(pending), resume parked, requeue all, test send. Disabled: pause sending, retry-now, park,
cancel parked. Writer.

### Step 9 — Pilot gate in the split

(a) No files.
(b) Before Steps 10–21 copy the pilot's pattern.
(c) `safe-verification` pre-flight → `devctl up split` → log in → fetch the mail page, each of
its panel fragments and the row menu; one real invoke (cancel a pending test message) → stop the
fleet. Findings go back to Steps 4/6/8 before any further port.
(d) `[inline]` orchestration.

### Steps 10–21 — Port the remaining providers (template from Step 8)

- **10 accounts** — Users & players list (`pages-core.js:72-112`), kind `player`, detail header,
  Profile & identities tab, search provider `player`, tile active players (DERIV).
- **11 characters** — Characters tab, summary fact, tab count.
- **12 wallet** — Economy page (`pages-platform.js:164-185`; global ledger FAKE per R1), Wallet
  tab, grant/revoke (REAL), Embers list column (DERIV), tile embers spent (DERIV). Writer.
- **13 inventory** — Inventory tab; grant item disabled.
- **14 friends** — Friends tab.
- **15 groups** — Groups tab (DERIV via `page_mine`); promote member to admin (REAL, Q1). Writer.
- **16 storage** — Cloud saves tab (`writes: false`), tile cloud saves used (DERIV).
- **17 notifications** — Inbox tab, Notify header action, Send notification row action. Writer.
- **18 apikeys** — API keys page (`pages-platform.js:31-44`), search provider `apikey`, reveal;
  rotate/expiry disabled. Writer.
- **19 scheduler** — Scheduler page (`pages-platform.js:74-85`); all writes disabled.
- **20 config** — Config & flags page (`pages-platform.js:88-109`); edit via `cp.invoke` with
  `expected_revision` = global revision; Revisions timeline FAKE except current value and
  revision. Writer.
- **21 audit** — Audit log page (`pages-platform.js:112-126`), dashboard "Recent operator
  activity" panel.

Q1 extras (groups promote-admin, wallet create-currency, "Purges pending" KPIs) are kept; see Owner answers.

### Step 22 — Tests

(a) Unit:
- `core/controlplane-rpc/src/tests.rs`: round trip of all four methods over a loopback edge;
  `invoke` not replayed after a connection-fatal error.
- `core/asyncevents/src/admin_tests.rs`: `resume_retry` resets failures and activates;
  `poison_event` returns the first event past the cursor; `resume` still leaves failures.
- `modules/admin/src/engine/*_tests.rs`: deadline → error; deadline with last-good → stale;
  role below `min_role` → denied with zero provider calls; `NotFound` → empty; `writes: false`
  → readonly; `implemented: false` POST → 403 with zero calls; `Conflict` → 409; manifest
  refresh keeps last-good when a peer fails.
- `modules/admin/src/tests.rs` auth suite: user-insert helper (`:1118`) gains a role; login,
  lockout, rate limit, CSRF, reveal assertions kept.
- `tools/adminctl/src/lib_tests.rs`: `--role` on insert; conflict keeps the role; `set-role`.
- `weles/master/src/fleet_toml_tests.rs:883-886`: expected args include `--role admin`.
- One `admin_tests.rs` per ported module covering its panels' data mapping and each REAL
  action's success and failure path (mail, accounts, characters, wallet, inventory, friends,
  groups, storage, notifications, apikeys, scheduler, config, audit).
- `tools/conformance` tests for `CONTROL_PLANE_WRITERS` (a writer without byte-cap coverage fails).
Splitproof (`tools/splitproof/src/main.rs`): rewrite AD0–AD6g, ADX1–ADX3b, WL3–WL6, WL6m, M3
(`:309-343`), ML3/ML4 (`:1300-1577`), FR8 (`:2103`), GR5 (`:2547`), AU3 (`:5232`), NT2
(`:5503-5617`), wallet admin POSTs (`:3555`, `:3759`, `:3968`, `:5730`), helpers
(`:1079-1112`); keep login, lockout, rate limit, CSRF, reveal, audit-trail assertions; add: a
killed provider's panel renders the error fragment while another provider's panel on the same
page returns 200; a viewer gets `denied` and 403 on invoke; config CAS edit through admin-svc →
config-svc succeeds and a stale revision returns 409; one real invoke per writing provider
(mail, wallet, groups, notifications, apikeys, config, event plane `resume_retry`).
(b) After Steps 1–21. (c) Split is the at-risk topology for every engine assertion.
(d) Unit: `[test-author]` (`model: "sonnet"`). Splitproof: `[test-author]` with `model: "opus"`.

### Step 23 — Docs

(a) `.agents/shared/gamebackend.md` (layout, adding-a-module admin section),
`.agents/skills/add-game-module/SKILL.md`, `.agents/skills/mockup-implementation/SKILL.md`,
`.claude/agents/mockup-implementer.md:41-42`, `.grok/agents/mockup-implementer.md`,
`docs/reference/event-plane-ops.md:123-134`, `docs/roadmap/feature-tracker.md`,
`core/remote/src/lib.rs:1649`, `docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md`
(errata D1). (b) Last, against landed code. (d) `[docs]` docs-writer (`model: "sonnet"`).

### Step 24 — Verification and live render comparison

(a) No files. (b) Terminal gate.
(c) Per `safe-verification`: one `verifyctl` manifest (`--all --strict`); then `devctl up
monolith`, log in, fetch every screen and fragment, compare element by element against the
mockup page; then the same through the split fleet. Report lists declared deviations.
(d) `[inline]` orchestration; report per `mockup-implementation` step 6.

## Owner answers

- **Q1 (answered: keep)** — groups "promote member to admin" (`modules/groups/src/admin.rs:49`,
  Step 15, writer), wallet "create currency" (`modules/wallet/src/admin.rs:40`, Step 12) and
  the "Purges pending" KPI (`docs/reference/event-plane-ops.md:125-130`, in each of the 7
  modules' port steps) are kept as extra elements built from existing primitives in the
  mockup's style; declared deviation D6.

## Declared deviations (known before implementation)

- **D1** — No admin-only `AuthReq` variant, although §8 mentions one. Control-plane ops are
  wire-only, so the gateway never routes them and `AuthReq` would be inert; the operator label
  is a request field. Errata line in the research doc (Step 23). The owner was told this when
  the research came back.
- **D2** — `user:` refs render from the `player` kind; "staff" kind FAKE.
- **D3** — No annotation layer, state picker, catalog or states pages.
- **D4** — Real vocabulary in the mockup's layout where real data exists (`ak_` keys, interval
  seconds, `gold/gems`, `namespace`+`key`, `admin|member`, mail `parked` for "failed").
- **D5** — Disabled actions and FAKE data per R1/R2 and the data-sources table.
- **D6** — Extra elements not in the mockup (Q1): groups promote-admin, wallet create-currency,
  "Purges pending" KPIs.

## Open items for implementers (do not change scope)

- HTTP/2 / SSE unverified; nothing here needs them.
- The manifest refresh dials all 13 peers at admin-svc start (today: lazily on first request);
  a down peer is retried every 5 s and logged at debug, as the gateway does.
- `include_str!` into `UILayout/design-system/` couples the admin build to that directory; no
  packaging context excludes it today.
- Gateway apikey header in the reveal hint: read `modules/gateway` for the real header before
  copying the mockup's `Authorization: Bearer` text; the real header wins.

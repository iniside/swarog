# Admin control plane — the admin portal moves into core (plan)

Revision 2 (2026-11-10). Revision 1 (bb8c721f) kept the portal as the `modules/admin`
fortress and moved only the contract into core. The owner corrected that on 2026-11-10:
the whole admin becomes part of core and there is no admin module.

## Context

### The owner's shape (his words, condensed)

- "Trzeba zrobić redesign admin i api pod niego… admin powinien awansować" (2026-10-22), and
  "moim założeniem było to, że admin stanie się częścią core i każdy moduł będzie do niego
  mógł rejestrować rzeczy sam… w tym modelu nie było w ogóle modułu admin" (2026-11-10).
  **Admin is core.** There is no `modules/admin` and no `api/admin/*`. Modules register
  their contributions into core themselves.
- "W splicie admin i tak jest częścią core, więc każdy moduł może mieć do niego dostęp" —
  modules register locally, in every topology. Carrying a split process's registrations to
  the process that serves `/admin` is core's job. Modules never see it.
- `cmd/admin-svc` stays as a **pure composition root with no module**. It is the only split
  process that serves `/admin` (owner choice 1b, 2026-11-10).
- The `admin.action` event contract moves into core (owner, 2026-11-10).
- Sidebar sections: "może być stała, ale opcjonalnie bym umożliwił modułom dodanie swojej
  sekcji i robi match po nazwie" — core ships a fixed section list. A module may declare an
  extra section, and declarations merge by `id`.
- "Ktoś może chce sobie napisać własny admin panel. W praktyce nikt tego nie będzie robił" —
  **one consumer**. Contract changes are wipes. There is no outward stable API.
- "Weles może być zintegrowany tylko od drugiej strony" — Weles neither contributes to nor
  consumes this contract.
- "Admin panel dostaje własne rekordy" — operators stay in `admin.users`. Only the operator
  login crosses the wire, as an audit label. The portal alone checks permissions.
- "Wystarczą stałe na v1" — fixed roles admin / operator / viewer.
- "Mockup przenosisz 1:1… jak część informacji nie może być wyświetlona, używasz mockowanych
  danych i je oznaczasz" — no artistic freedom. Missing actions render disabled with a hint.
  Lore fragments on in-scope screens render as marked fakes.
- Owner choices on the revision-1 review (2026-11-10): raze the old contract first; every
  screen-port step is split into a `core-implementer` part (backend) and a
  `mockup-implementer` part (view).
- Registration: the owner picked "modules call `controlplanerpc` directly" while the portal
  was still a module. Under revision 2, "carrying a split process's registrations… is core's
  job", so modules make **no** rpc call and `app::run` registers the edge face (Step 4).
  This substitution needs the owner's one-line confirmation before Step 2 dispatches.

### Rules derived from the owner's shape (apply to every UI step)

- **R1 — data.** Follow the data-sources table. REAL renders real data. DERIV (a new read
  over existing columns, with no new column, index or table) is implemented. FAKE and NEW
  data render as deterministic fakes marked `// FAKE: <why>`: a pure function of the entity
  id, using an explicit-width hash, with no clock and no rand. This plan adds no columns,
  tables or indexes other than `admin.users.role` (Step 5).
- **R2 — actions.** REAL actions are wired. NEW actions are declared with `implemented: false`
  and render disabled with the hint "Not implemented yet". The portal never posts them.
- **R3 — fidelity.** Every element of the mockup page is present. Every difference is a
  declared deviation in the step's report.

### Exclusions

- Lore screens (`lore-*.html`) and the Lore module. Only Lore fragments on in-scope
  screens render, as fakes.
- New domain behaviour behind NEW actions (suspend, rotate key, scheduler edit, mail
  pause/park, grant item, revoke sessions, download saves, checkpoint rewind…).
- Full RBAC and editable roles; the Weles management API; per-method edge caller restriction.
- The player_id → user_id rename. Entity refs keep the `player` kind.
- `catalog.html`, `states.html`, the annotation layer, the forced-state picker, and `when`
  predicates on actions (mockup tooling; Lore-only predicates).

### Sources (research, committed)

- `docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md`: gaps and §8 decisions.
  §8's "contribution seam lives in core; modules still never import the portal" is
  superseded: the portal itself is core (errata in Step 26).
- `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`: the per-element
  REAL / DERIV / FAKE / NEW table. **This is the data-source checklist for every UI step.**
- The mockup `UILayout/redesign/` on `UILayout/design-system/`. The contract sketch is
  `UILayout/redesign/assets/catalog.js:12-293`, and the mock manifests (sections, points,
  ownership) are `registry.js:6-125`.

### Overlap map — why not extend X

| Existing | What it does | Why not extend / depend on it |
| --- | --- | --- |
| `adminapi` + `adminrpc` (`api/admin/*`) | One `Content` per item, `AdminData`/`AdminSubmit`, `EntityMenu`/`ModalActions` | Its shape is the defect: one table and one form per page, and the nav is learned by serially fetching every provider's full content on every request (`modules/admin/src/lib.rs:1495-1543`). It has no operator and no entity context. It lives in `api/`, which core must not import (archcheck rule 16, `tools/archcheck/src/main.rs:576-606`). Replaced by a wipe in Step 1. |
| `modules/admin` (fortress) | Portal plus login, sessions, CSRF, lockout, rate limit, reveals, schema `admin` | The owner's model has no admin module. The auth and session code is **moved** into `core/admin` (Step 5); it is not rewritten. The old engine dies in Step 1. |
| `core/metrics` | A core crate that is a `lifecycle::Module` and mounts routes in `init` (`core/metrics/src/lib.rs:138-165`) | The precedent for `core/admin`'s shape. No new lifecycle mechanism is needed. |
| `app::run` `__describe` registration (`core/app/src/lib.rs:632`, applied `:909-932`) | Core registers its own edge method in every edge-serving process | The precedent for the per-process `cp.*` face (Step 4). |
| `opsapi` DESCRIBE plus gateway refresh (`modules/gateway/src/lib.rs:1700-1860`, stop `:205-270`, `:613`) | Static per-peer manifest, refreshed in one wave every 5 s with a 6 s per-peer timeout, keeping last-good | The right pattern for the manifest cache. The pattern is copied; the type is not reused. |
| `opsapi::AuthReq` / `Identity` | The gateway bearer gate for `#[http]` ops | The `cp.*` ops are wire-only and the gateway never routes them (`modules/gateway/src/lib.rs:997-1015`). There is no `AuthReq` variant (D1). |
| `tools/eventctl` (`tools/eventctl/src/lib.rs:79-330`) | info, snapshot, retry, pause, resume, retire, skip, bump_generation | The portal needs these ops. `asyncevents.` SQL is allowed only in `core/asyncevents`, `tools/eventctl` and `tools/splitproof` (`tools/archcheck/src/main.rs:1654-1656`). The ops are relocated into `core/asyncevents` (Step 3), and eventctl becomes a thin CLI. |
| `remote::Stub` + `RemoteFactory` | Per-peer lazy reconnecting caller in `cmd` roots | Reused as-is for admin-svc's peer faces. |
| `admin.reveals` | Show-once secret PRG flash | Reused for `SecretReveal`. |

### Verified facts the steps rely on

- Edge calls have no response deadline (`core/edge/src/client.rs:113-154`). The portal
  wraps every remote call in `tokio::time::timeout`.
- Every mTLS peer is equally trusted (`core/edge/src/tls.rs:174-176`). This is accepted (§8).
- `app::run` (`core/app/src/lib.rs:718`):
  - It drains `EDGE_SLOT` and registers `__describe` before any migrate (`:909-932`).
  - It merges `ctx.mount` routers at `:955-972`.
  - It migrates the plane before modules (`:976-978`) and starts the plane after modules (`:995`).
  - An edge-less process (the monolith, admin-svc) silently drops `EDGE_SLOT` (`:602`).
- `Context` exposes `db()`, `contribute`, `contributions`, `mount`, `bus()`, and no plane
  handle (`core/lifecycle/src/context.rs:52-145`).
- Subscription state is read from the DB by every worker: claims require `state = 'active'`
  (`core/asyncevents/src/worker.rs:205`), and pause is a DB update (`:386-397`). A pause or
  resume written from admin-svc's pool takes effect in every process.
- archcheck facts:
  - `classify` maps `/core/` → `Kind::Core` (`tools/archcheck/src/main.rs:181-220`).
  - Rule 10 (no runtime `asyncevents` dependency) applies only to `Kind::Module` (`:395-413`).
  - Rule 12 tolerates a `cmd/*-svc` with no same-named module (`:511-516`).
  - Rule 6 requires `metrics` in every main (`:450-469`).
  - `contrib::Slot::new(` is allowed only in `SLOT_OWNER_FILES` (`:60-68`, `[&str; 7]`).
- Contract scanners walk only `api/*`:
  - topiccheck `rpc_modules_from_fs` (`tools/topiccheck/src/golden.rs:520-563`, self-check
    `:569-594`, label via `to_snake` `:553`), the hand-list `defined_topics` (`main.rs:185-210`),
    and the events drift test (`tests.rs:377-402`);
  - conformance `input_inventory::discover` (`tools/conformance/src/input_inventory.rs:94-119`),
    filtered by `rpc_contract_model::served_domains` (keyed on `modules/<dir>`,
    `tools/rpc-contract-model/src/lib.rs:70-72`);
  - opscatalog-gen hand-list `:71-72`, with self-check `rpc_modules_from_fs_at` (`:182-239`,
    compared `:259-280`).
- The conformance scanner reports nested request fields prefixed by the argument name
  (`input_inventory.rs:163-166`, `:303-321`, `:333`). The committed
  `tools/conformance/input-fields.golden.tsv` is diffed on every run (`main.rs:346-351`).
- rpc-macro (`tools/rpc-macro/src/lib.rs`) accepts multi-argument methods, struct args,
  `Option`, `BTreeMap` and enums. The wire name is `prefix.lowerCamel(method)` (`:162`).
  `#[retry_safe]` → `OnceAfterReconnect`, otherwise `Never`. `generate_glue` emits `Client`,
  `register_server(&mut edge::Server, Arc<dyn Trait>)` (`:464`) and `provide_remote` (`:475`).
- `ADMIN_OPEN=1` disables sessions and CSRF and shows the "Local Admin" placeholder
  (`modules/admin/src/lib.rs:43, 357, 521`).
- These callers use `admin::` directly:
  - `tools/adminctl` (`USERS_DDL` `lib.rs:35`, `hash_password` `:66`, `verify_password`,
    `normalize_username` `:60,109`, `main.rs:75`);
  - `tools/conformance/src/policy.rs:37, 303-332` (the `admin()` entry and
    `admin::conformance::*` probes) and `tests.rs:625-628` (the argon parity test);
  - `cmd/server/src/tests.rs:15`.
- These places name admin-svc, the admin module or its contract:
  - `tools/checkmodules/src/lib.rs:44` (admin-svc's module list);
  - `tools/routecheck/src/main.rs:222-233` (admin-svc listed as edge-less);
  - `tools/topiccheck/src/main.rs:94-96, 202` and `golden.rs:292` (`adminevents`);
  - `modules/audit/src/lib.rs:56, 79` and `Cargo.toml:43` (the `admin.action` subscription);
  - `docs/reference/contract-golden/contracts.txt:40, 336-337`;
  - the public-api baselines `adminapi.txt` and `adminevents.txt`;
  - `tools/splitproof/src/main.rs:251-252, 318-335`.
- `admin.users` bootstrap callers are `tools/devctl/src/supervisor.rs:405-427`,
  `install.sh:42`, `install.ps1:54`, `weles/fleet.split.toml:39`, `weles/fleet.monolith.toml:24`
  and `tools/splitproof/src/main.rs:957-975`.

## Design

### Crates

- **`core/controlplane`** (package `controlplane`): the edge-free contract. Its deps are
  `contrib`, `opsapi`, `bus`, `rpc-macro`, `serde`, `serde_json`, `async-trait` and `thiserror`.
  It holds the types below, the `Provider` trait plus `PROVIDER_SLOT`, the `#[rpc]` trait
  `ControlPlane` plus `SOURCE_SLOT`, `LocalFace`, `SECTIONS`, and the `admin.action`
  descriptor plus `AdminAction` payload (moved verbatim from `api/admin/events`; topic,
  version and `golden_samples` unchanged).
- **`core/controlplane-rpc`** (package `controlplanerpc`): the generated glue plus two
  functions:
  - `register_face(server: &mut edge::Server, providers: Vec<Arc<dyn Provider>>)`;
  - `remote_factory(peer: &'static str) -> remote::RemoteFactory`, which contributes
    `Source { peer, face: Arc::new(Client::new(caller)) }` to `SOURCE_SLOT`.

  Its deps are `controlplane`, `opsapi`, `edge`, `remote`, `lifecycle`, `registry`
  (generated `provide_remote`, `tools/rpc-macro/src/lib.rs:481`), `rpc-macro`,
  `async-trait` and `serde_json`.
- **`core/admin`** (package `admin`): the portal, moved from `modules/admin`. It is a
  `lifecycle::Module` (the `core/metrics` precedent) and owns schema `admin`, the
  routes, sessions, auth, engine, templates, `portal` and `core.events` providers, and
  fakes. Its deps are the current `modules/admin` deps minus `adminapi`/`adminevents`, plus
  `controlplane` and `asyncevents`.
- Deleted: `modules/admin` (moved), `api/admin/api`, `api/admin/rpc`, `api/admin/events`.

### Module-facing seam (what a module writes)

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn manifest(&self) -> Manifest;                       // static, no I/O
    async fn panel(&self, req: PanelRequest) -> Result<Primitive, opsapi::Error>;
    async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeOutcome, opsapi::Error>;
}
pub const PROVIDER_SLOT: contrib::Slot<Arc<dyn Provider>> = contrib::Slot::new("controlplane.provider");
```

A module contributes it in `init`:
`ctx.contribute(controlplane::PROVIDER_SLOT, Arc::new(MailAdmin::new(..)) as Arc<dyn Provider>)`.
That is the module's entire integration. It needs no rpc crate, no `EDGE_SLOT` line and no
topology knowledge.

### Process face (core-only, never written by modules)

```rust
#[rpc(prefix = "cp")]
#[async_trait]
pub trait ControlPlane: Send + Sync {
    #[retry_safe] async fn manifests(&self) -> Result<Vec<Manifest>, opsapi::Error>;
    #[retry_safe] async fn panel(&self, req: PanelRequest) -> Result<Primitive, opsapi::Error>;
    #[retry_safe] async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeOutcome, opsapi::Error>;
}
pub struct LocalFace { /* Vec<Arc<dyn Provider>> */ }    // impl ControlPlane, dispatch on req.module
#[derive(Clone)] pub struct Source { pub peer: String, pub face: Arc<dyn ControlPlane> }
pub const SOURCE_SLOT: contrib::Slot<Source> = contrib::Slot::new("controlplane.source");
```

- `LocalFace::new(providers)` panics on a duplicate `Manifest.module` (startup, before I/O).
  An unknown `req.module` → `opsapi::Status::NotFound`.
- `app::run` registers `LocalFace` over `ctx.contributions(PROVIDER_SLOT)` on every
  edge-serving process, **unconditionally**, even when the slot is empty (Step 4). This
  follows the `__describe` precedent (`core/app/src/lib.rs:626-629`): every peer answers
  `cp.manifests` uniformly, and an empty `Vec` means "no providers", never an error. The
  monolith and admin-svc have no edge server and serve nothing.
- The portal reads `PROVIDER_SLOT` (local providers, wrapped in one `LocalFace` with
  `peer = "local"`) plus `SOURCE_SLOT` (admin-svc's remote peer faces). It routes by
  `Manifest.module` → source. In the monolith, `SOURCE_SLOT` is empty and everything is local.

### Requests and outcomes

- `EntityRef { kind: String, id: String }`, rendered as `kind:id`.
- `Link = Page { module: String, page: String, params: BTreeMap<String, String> } | Entity(EntityRef)`.
- `PanelRequest { module: String, contribution: String, entity: Option<EntityRef>,
  params: BTreeMap<String, String> }`. It carries no operator and no role (§8).
- `SearchRequest { module: String, kind: String, q: String, limit: u32 }`;
  `SearchHit { target: EntityRef, title: String, subtitle: String, badge: Option<String> }`.
- `InvokeRequest { module: String, action: String, entity: Option<EntityRef>,
  input: BTreeMap<String, String>, expected_revision: Option<u64>, reason: Option<String>,
  operator: String }`. `operator` is only an audit label. De-duplication uses
  `InvokeCall.request_id` (Step 8a).
- `InvokeOutcome = Ok { message: String, reveal: Option<SecretReveal>, redirect: Option<Link> }
  | FieldErrors(BTreeMap<String, String>) | Conflict { current_revision: u64 }`.
- Portal HTTP mapping:

  | Outcome | HTTP |
  | --- | --- |
  | `Ok` | 303 PRG |
  | `NoOp` (Step 8a) | 303 PRG with an info flash; audit `detail` `noop: <message>` |
  | `FieldErrors` | 422 with the form re-rendered |
  | `Conflict`, or `Status::Conflict` | 409 |
  | `Status::NotFound` | 404 |
  | anything else | error toast |

- `Role = Viewer | Operator | Admin` (ordered, serde) lives in `controlplane` because
  manifests carry `min_role`. Only the portal compares roles.

### Manifest (static; cached by the portal)

```text
Manifest { module, sections: Vec<NavSection>, entries: Vec<NavEntry>, pages: Vec<PageDecl>,
           entity_kinds: Vec<EntityKindDecl>, contributions: Vec<ContributionDecl>,
           actions: Vec<ActionDecl>, search: Vec<SearchDecl>, writes: bool }
NavSection     { id, label, order: i32 }        // optional extra section
NavEntry       { section, id, label, icon, order: i32, page, counter: Option<String> }
PageDecl       { id, title, subtitle, head_actions: Vec<String>, layout: Layout }
Layout         = Stack(Vec<String>) | Grid21(Vec<String>, Vec<String>)
               | Grid2(Vec<String>, Vec<String>) | Tiles(Vec<String>)     // contribution ids
EntityKindDecl { kind, label, icon }             // the header comes from Point::EntityHeader
ContributionDecl { id, point: Point, order: i32, title, icon, min_role: Role,
                   cache_ttl_s: Option<u32>, deadline_ms: u32 }       // default 2000
Point          = Page(String) | DashboardTile | DashboardAttention | DashboardPanel
               | EntityHeader(String) | EntityTab { kind: String, tab: String, tab_label: String }
               | EntityTabCount { kind: String, tab: String } | EntitySummary(String)
               | EntityListColumn { kind: String, label: String } | NavCounter
ActionDecl     { id, point: ActionPoint, label, icon, order: i32, danger: bool, multi: bool,
                 min_role: Role, then: Then, implemented: bool }
ActionPoint    = RowAction(String) | HeaderAction(String) | PageHead(String) | PanelTool(String)
Then           = Invoke | Form(FormSpec) | Confirm(ConfirmSpec) | Link(Link)
FormSpec       { fields: Vec<Field>, hidden: BTreeMap<String, String>, submit_label: String,
                 note: Option<String>, revision: Option<u64> }
Field          { key, label, kind: FieldKind, required: bool, help: Option<String>,
                 default: Option<String>, readonly: bool }
FieldKind      = String | Text | Int | Float | Bool | Enum(Vec<String>) | Multi(Vec<String>)
               | Json | Ref(String) | Money(Vec<String>) | Duration
ConfirmSpec    { title, body, tone: ConfirmTone, impact: Vec<(String, String)>, reason: ReasonReq,
                 type_to_confirm: Option<String>, confirm_label }
ConfirmTone    = Danger | Warn | Info;   ReasonReq = Required | Optional | None
SearchDecl     { kind, label, icon, deadline_ms: u32 }                  // default 800
```

- Several contributions with the same `EntityTab { kind, tab }` stack inside one tab by
  `order`. `EntityHeader(kind)` and `EntityTabCount` return `Primitive::EntityHeader` and
  `Primitive::Cell`. `EntitySummary` and `NavCounter` return `Primitive::Cell`.
  `EntityListColumn` is fetched once per table page with the typed `PanelQuery.refs`
  (Step 13) and returns `Primitive::ColumnCells`.
- **Amended by Step 8a (owner, 2026-11-10); the step text is authoritative:**
  - `PanelQuery` gains typed fields `search`, `cursor`, `before`, `queue_state`, and later
    `refs` (Step 13);
  - `ActionDecl.primary: bool`;
  - `ActionPoint::PanelForm(contribution)` (the form panel's Save) and a defined
    `PanelTool(contribution)`;
  - `PropertyList.item`;
  - `InvokeOutcome::NoOp { message }`;
  - `InvokeCall.item` and `InvokeCall.request_id`;
  - the portal table `admin.forms` (form instances);
  - `encode_field`/`decode_field` over `FieldValue`.
- **Sections.** `controlplane::SECTIONS` is fixed: `overview` "Overview" 0, `players`
  "Players" 10, `economy` "Economy" 30, `platform` "Platform" 40. A module's
  `Manifest.sections` adds sections, which merge by `id`:
  - a fixed section always wins over a module declaration with the same `id`;
  - among modules, the declaration from the lexicographically smallest `module` wins;
  - admincheck reports every declaration whose `label` or `order` differs from the winner.
  - A `NavEntry.section` that names no section is an admincheck finding. At runtime the
    entry is placed in `platform`.

### Primitive (closed enum; fields follow `catalog.js` "Data it needs")

```text
Tone = Ok | Warn | Err | Info;  Align = Left | Right;  Trend = Up | Down | None
CellKind = Text | Mono | Badge | Ref | Number | Bytes | Time | User
Cell { kind: CellKind, value: String, tone: Option<Tone>, link: Option<Link> }   // :25-26
Column { key, label, kind: CellKind, sortable: bool, align: Align }               // :18
Row { entity: Option<EntityRef>, cells: Vec<Cell> }                               // :19
PageInfo { next_cursor: Option<String>, size: u32, total: Option<u64> }           // :20
FilterKind = Enum(Vec<String>) | Text | Range | Bool | Ref(String)                // :21
FilterDef { key, label, kind: FilterKind }
DataTable { columns, rows, page: PageInfo, filters: Vec<FilterDef>,
            search_placeholder: Option<String>, selectable: bool,
            list_columns_for: Option<String> }        // entity kind whose EntityListColumns apply
PropertyList { items: Vec<(String, Cell)>, columns: u8 }                          // :176
Delta { value: String, period: String, good: Trend }
KpiTile { label, value: String, unit: Option<String>, delta: Option<Delta>,
          series: Vec<f64>, capacity: Option<(f64, f64)>, link: Option<Link> }    // :97-103
TimelineEntry { at: String /*RFC 3339 UTC*/, actor: Option<EntityRef>, verb: String,
                target: Option<EntityRef>, summary: String /*plain text*/,
                detail: Option<String>, icon: String, tone: Option<Tone> }        // :113-118
Timeline { entries: Vec<TimelineEntry>, cursor_before: Option<String>, live: bool }
QueueState { key, label, count: u64, tone: Option<Tone> }
QueueItem { id, entity: Option<EntityRef>, state: String, attempts: u32, max_attempts: u32,
            next_attempt_at: Option<String>, last_error: Option<String>,
            payload_preview: Option<String>, actions: Vec<String> /*ActionDecl ids*/ } // :128-135
WorkQueue { states: Vec<QueueState>, selected: String, items: Vec<QueueItem> }
ServiceState = Running | Degraded | Restarting | Crashed | Down
ServiceItem { name, state: ServiceState, version: Option<String>, p99_ms: Option<u32>,
              uptime: Option<String>, note: Option<String>, history: Vec<(String, ServiceState)> }
ServiceStatus { items: Vec<ServiceItem> }                                         // :163-167
Axis = Left | Right;  Series { label, unit, values: Vec<f64>, axis: Axis }
TimeSeriesChart { buckets: Vec<String>, series: Vec<Series> }                     // :185
Severity = Err | Warn
AttentionItem { severity: Severity, title, text, link: Link, action_label, since: String } // :210
SecretReveal { label, value: String, hint: Option<String>, expires_at: Option<String> } // :86
EntityHeader { title, subtitle, status: Option<Cell>, avatar_seed: String }
Primitive = DataTable(DataTable) | PropertyList(PropertyList) | KpiTile(KpiTile)
          | Timeline(Timeline) | Form(FormSpec) | WorkQueue(WorkQueue)
          | ServiceStatus(ServiceStatus) | TimeSeriesChart(TimeSeriesChart)
          | AttentionItems(Vec<AttentionItem>) | ColumnCells(BTreeMap<String, Cell>)
          | Cell(Cell) | EntityHeader(EntityHeader) | Empty { title: String, detail: String }
```

- No HTML crosses the wire. Every string is rendered escaped. Numbers in KPI and Cell
  values arrive preformatted by the module. `f64` fields (`series`, `capacity`, `values`)
  must be finite: `serde_json` writes NaN as `null`, which then fails to decode.
  `LocalFace` replaces non-finite values with `0.0` before returning.
- `SecretReveal` implements `Debug` with the value redacted.
- `PermissionMatrix` is not built (Lore screens only).

### Portal (`core/admin`)

- **Routes:**
  - `GET /admin` (dashboard)
  - `GET /admin/p/{module}/{page}`
  - `GET /admin/e/{kind}/{id}?tab=`
  - `GET /admin/_panel/{module}/{contribution}` (htmx fragment, `hx-trigger="load"`)
  - `GET /admin/_menu/{kind}/{id}`
  - `GET /admin/_search?q=&kind=`
  - `GET /admin/_action/{module}/{action}` (modal fragment)
  - `POST /admin/_action/{module}/{action}`: CSRF → role → `implemented` → invoke → PRG
  - `GET /admin/assets/{tokens.css,bundle.css,admin.js}`
  - `/admin/login` and `/admin/logout`, unchanged
- **Manifest cache.** A task spawned in `start` refreshes every source in one wave every 5 s,
  each call under a 6 s timeout, keeping the last-good copy. The pattern is
  `modules/gateway/src/lib.rs:1700-1860`. `stop` aborts it within the module stop budget
  (pattern `:205-270`, `:613`). Local providers are read once in `start` (static).
- **Panel states:**
  - role below `min_role` → `denied`, with no call;
  - timeout or error with a last-good copy younger than `cache_ttl_s` → `stale`;
  - otherwise timeout or error → `error`;
  - `Status::NotFound`, or a payload with zero items → `empty`;
  - `writes == false` → `readonly`;
  - else → `ok`.
- **Roles.** `admin.users.role text NOT NULL CHECK (role IN ('admin','operator','viewer'))`,
  edited in place in the DDL (wipe; no default).
  - `adminctl create-user <name> [--role R]` (default `admin`) never changes an existing
    user's role.
  - `adminctl set-role <name> <role>` changes it.
  - Bootstrap callers pass `--role admin`.
  - `ADMIN_OPEN=1` sessions act as role `Admin` with operator label `Local Admin` (the
    existing placeholder).
- **Rendering.** minijinja macros, one per primitive, with markup lifted from
  `UILayout/design-system/components/bundle.js`. CSS is served from `include_str!` of
  `UILayout/design-system/tokens.css` and `components/bundle.css`. `admin.js` does
  data-attribute delegation only (CSP unchanged): menus, popovers, the Ctrl-K palette,
  confirm with type-to-confirm, reveal acknowledgement and the theme toggle.
- **Portal-owned providers inside `core/admin`**, contributed to `PROVIDER_SLOT` like any module:
  - `portal`: the dashboard and Needs-attention hosts, plus the marked fakes in `fakes.rs`
    (service health list and pill, notification bell, "Matches in progress" tile, every Lore
    fragment).
  - `core.events`: the Event plane page, real via `asyncevents::admin` over `ctx.db()`.

## Steps

**Intermediate gate (Steps 1–24).**
- Run `cargo check -p <touched crates> --all-targets` (tests compile too), after the
  `safe-verification` pre-flight (no live cargo or rustc, no fleet).
- The full `verifyctl` manifest runs once, in Step 27, after the tests are rewritten
  (Step 25).
- The bless invocations are allowed where a step names them, one at a time:
  - `cargo run -p verifyctl -- --bless-public-api`
  - `cargo run -p verifyctl -- --bless-contract-golden`
  - `cargo run -p verifyctl -- --bless-input-golden`

  After each bless, `git diff --stat` the baseline directory and restore every file the
  step did not cause.
- The admincheck stub (Steps 1–6) is an empty gate. Nothing runs it before Step 27, and
  Step 7 replaces it.
- **Red by design from Step 1 until Step 25:**
  - splitproof's admin groups;
  - every non-admin group driven through the admin wallet grant: PH3, PH6, PH11, NT4, NT4m
    (`tools/splitproof/src/main.rs`) and PY7 (`tools/splitproof/src/party_checks.rs:291-293`);
  - the M3/M3b monolith-parity admin rows.

  `wallet_form` (`main.rs:1086-1100`) returns empty strings on a missing form, so these
  checks fail rather than abort.

### Step 1 — Raze the old admin contract

(a) What is touched:
- **13 providers** (`accounts`, `characters`, `inventory`, `wallet`, `friends`, `groups`,
  `storage`, `notifications`, `mail`, `apikeys`, `config`, `scheduler`, `audit`):
  - remove the `adminapi::SLOT` contribution and the `register_admin*` lines in the
    `EDGE_SLOT` closure (e.g. `modules/wallet/src/lib.rs:344-345`,
    `modules/apikeys/src/lib.rs:322-323`, `modules/groups/src/lib.rs:219-220`,
    `modules/mail/src/lib.rs:258,275-276`);
  - delete `modules/<m>/src/admin.rs` where it exists. config, scheduler and audit keep
    their admin code inline: delete it from `modules/config/src/lib.rs` (every `adminapi`
    use), `modules/scheduler/src/lib.rs:562-610, 766-789` and
    `modules/audit/src/lib.rs:230-270, 408-430`;
  - delete the probes that import `crate::admin` (`modules/mail/src/conformance.rs:9-12`
    and `modules/groups/src/conformance.rs:16`) and their CapCase rows
    (`tools/conformance/src/policy.rs:576-582` and `:876-882`). Steps 11a and 18a restore
    them;
  - delete every `EDGE_SLOT` contribution that becomes empty: mail `lib.rs:268-277`, audit
    (~`:425-432`) and scheduler (~`:782-791`). A closure that still registers a non-admin
    method keeps only those lines;
  - delete the chains that become orphans: scheduler's admin-only `Service`, its `svc` cell
    and construction (`:551-615`, ~`:715`), and the admin id consts (scheduler ~`:125`,
    audit ~`:102`). Check audit's `self.svc()` use at `:375` first: if the `Service` there
    serves anything besides admin, keep it.
- **rpc and api crates:**
  - `api/<m>/rpc/src/lib.rs`: remove the `adminrpc` re-exports;
  - `api/mail/rpc`, `api/audit/rpc` and `api/scheduler/rpc` contain only that re-export.
    Delete those three crates, their workspace members and `[workspace.dependencies]`
    entries (`Cargo.toml:347, 349, 376` and the members list), and their deps
    (`modules/mail/Cargo.toml:41`, `modules/audit/Cargo.toml:20`,
    `modules/scheduler/Cargo.toml:21`, and every `cmd/*` and `tools/*` Cargo.toml naming
    them);
  - drop `adminapi`/`adminrpc` from `modules/<m>/Cargo.toml`, `api/<m>/rpc/Cargo.toml`,
    `api/accounts/api/Cargo.toml:22` and `api/characters/api/Cargo.toml:22`;
  - delete `PLAYERS_ROW_MENU` (`api/accounts/api/src/lib.rs:282`), `CHARACTERS_CARD_MENU`
    and `CHARACTER_MODAL_ACTIONS` (`api/characters/api/src/lib.rs:97,106`).
- **Tests:** delete the tests that exercise deleted admin code, including those inside mixed
  test files (e.g. `modules/groups/src/service_tests.rs:12, 402, 527` and the `adminapi`
  uses in `modules/config/src/tests.rs`). This is deletion only; replacements come in Step 25.
- **`modules/admin/src/lib.rs`:** delete everything typed on `adminapi`, with its tests in
  `modules/admin/src/tests.rs`:
  - the engine: `resolve_items`, `collect_extensions`, `build_menu`, `page_view`,
    `build_page_view`, `render_modal`, and the `PageView`/`CardView` types
    (`:1822-1916`, `:2053-2111`);
  - the submit chain: `item_post`, `collect_submit_params`, `emit_form_submit`,
    `render_after_submit` (`:1210-1433`);
  - the reveal stash: `stash_outcome`/`take_outcome` (`:801-857`);
  - the slug routes `/admin` and `/admin/:slug`.

  Login, logout, session, CSRF, lockout, rate limit, `REVEAL_DDL` and
  `prune_expired_reveals` stay. Drop the `adminapi` dependency. Until Step 6, `GET /admin`
  returns 404 and a successful login still redirects there.
- **`cmd/admin-svc/src/lib.rs`:** remove the 13 `admin_stub` entries and the `adminrpc`
  dependency. `modules()` = `[metrics, admin]`.
- **Delete** `api/admin/api` and `api/admin/rpc`, together with their root `Cargo.toml`
  members (`:122-123`) and `[workspace.dependencies]` entries (`:344-345`).
  `api/admin/events` stays until Step 2.
- **`tools/admincheck/src/main.rs`:** reduced to a `main` that exits 0 (rewritten in
  Step 7). Drop its `adminapi` dependency (`tools/admincheck/Cargo.toml:30`).
- **Tool entries:**
  - `tools/archcheck/src/main.rs:60-68` (drop `api/admin/api/src/lib.rs`; the array
    becomes `[&str; 6]`) and the stale fixture `tools/archcheck/src/tests.rs:1176-1187`;
  - `tools/topiccheck/src/golden.rs:133-144` and `tools/topiccheck/Cargo.toml:53`;
  - `tools/opscatalog-gen/src/main.rs:71-72` and `Cargo.toml:18`;
  - `tools/conformance/src/{checks.rs:100-150 (ADMIN_SUBMIT_MODULES), policy.rs:82-86,
    main.rs:92-123, tests.rs:146-202, 227-238, 255+}` (every `ADMIN_SUBMIT_MODULES` test);
  - stale comments `policy.rs:836-842, 1165-1172` (they name `admin::apply_submit` and
    `admin.adminSubmit`).
- **Baselines:**
  - delete `docs/reference/public-api-baseline/adminapi.txt`;
  - re-bless `accountsapi` and `charactersapi`;
  - the contract-golden drops `docs/reference/contract-golden/contracts.txt:336-337`;
  - re-bless the input golden (`--bless-input-golden`: the `admin.adminData` and
    `admin.adminSubmit` rows go).

(b) Why now: the owner chose raze-first, so no step has two admin contracts alive.

(c) How: pure deletion, plus the minimal edits that keep the touched crates compiling with
`--all-targets`.

(d) `[opus]` core-implementer. It removes security tripwires and a write surface.

### Step 2 — `core/controlplane` + `core/controlplane-rpc`, the `admin.action` move, and gates that see core

(a) New crates and workspace:
- new `core/controlplane/{Cargo.toml,src/lib.rs}` and `core/controlplane-rpc/{Cargo.toml,src/lib.rs}`;
- the workspace `Cargo.toml` members list plus `[workspace.dependencies]` entries for
  `controlplane` and `controlplanerpc`.

The `admin.action` move:
- move `api/admin/events/src/lib.rs` (descriptor, `AdminAction`, `golden_samples`) into
  `core/controlplane/src/events.rs` (`pub mod events`), then delete `api/admin/events`;
- consumers:
  - `modules/admin`: the `Cargo.toml` swap from `adminevents` to `controlplane`, and the
    emit sites that survive Step 1 (`lib.rs:604, 724, 757, 1041`);
  - `modules/audit/src/lib.rs:56,79`, `modules/audit/src/tests.rs:115`, `modules/audit/Cargo.toml:43`;
- topiccheck: `tools/topiccheck/src/main.rs:202`, `golden.rs:292` and `Cargo.toml:39`. The
  events drift test `tests.rs:377-402` and the Option-None self-check `golden.rs:367-395`
  are extended to scan `core/controlplane/src`;
- `tools/splitproof/src/main.rs:318-335` keeps its SQL. The topic name is unchanged.

Gates that must see core:
- `tools/archcheck/src/main.rs:60-68`: `SLOT_OWNER_FILES` adds `core/controlplane/src/lib.rs`
  (the array becomes `[&str; 7]`).
- topiccheck `golden.rs`: the `rpc_modules` entry is labelled
  `controlplane::control_plane_rpc`. The scanner `:520-563` and self-check `:569-594` also
  walk `core/controlplane/src`.
- opscatalog-gen: the hand-list `:71-72` adds `controlplane`. `rpc_modules_from_fs_at`
  `:182-239` also walks `core/controlplane/src` and treats it as **served**, unconditionally
  (core is served in every edge process). Add the `controlplane` dep to `Cargo.toml`.
- conformance `input_inventory::discover` (`:94-119`) also walks `core/controlplane/src`,
  admitted unconditionally (not through `served_domains`). The `tools/conformance/src/policy.rs`
  `InputPolicy` rows are all Opaque until a writer lands:

  | Method | Keys |
  | --- | --- |
  | `cp.panel` | `req.module`, `req.contribution`, `req.entity.kind`, `req.entity.id`, `req.params.<key>`, `req.params.<value>` |
  | `cp.search` | `req.module`, `req.kind`, `req.q` |
  | `cp.invoke` | `req.module`, `req.action`, `req.entity.kind`, `req.entity.id`, `req.input.<key>`, `req.input.<value>`, `req.reason`, `req.operator` |

  Each row's `rationale` states the invariant, not a stage: "cp.* is wire-only and served
  only to the portal; each writer's caps are proven by its own `CONTROL_PLANE_WRITERS`
  CapCase". If the scanner's real key spelling differs, the implementer uses the scanner's
  output and reports it.
- conformance `CONTROL_PLANE_WRITERS` tripwire in `tools/conformance/src/main.rs`, beside
  `admin_submit_impl_modules` (`:92-123`). It scans **all non-test sources** under
  `modules/<m>/src` and `core/admin/src` (each directory skipped when absent) for a
  manifest with `writes: true`. The entry name is `<m>` for `modules/<m>`, and `admin` for
  `core/admin`. Each writer must have an `InputByteCaps` CapCase for `cp.invoke` in that
  entry. The list starts empty, and writer steps add themselves.
- `tools/verifyctl/src/stages/public_api.rs:201-209`: `discover` also yields `core/controlplane`.

(b) Why now: everything compiles against these types. The gates must see `cp.*` before any
provider exists.

(c) How:
- Types, traits, `LocalFace`, `SOURCE_SLOT`, `PROVIDER_SLOT` and `SECTIONS` as in **Design**.
- The glue uses `rpc_macro::generate_glue` (pattern: the deleted `api/admin/rpc/src/lib.rs:21-27`,
  read via `git show bb8c721f:api/admin/rpc/src/lib.rs`).
- `controlplanerpc::register_face` calls `register_server(server, Arc::new(LocalFace::new(providers)))`.
- `controlplanerpc::remote_factory(peer)` contributes `Source { peer, face: Arc::new(Client::new(caller)) }`.
- Bless public-api (new `controlplane.txt`, delete `adminevents.txt`), contract-golden
  (`cp.*` lines; `contracts.txt:40` keeps `admin.action`) and the input golden, per
  `safe-verification`.

(d) `[opus]` core-implementer.

### Step 3 — Relocate event-plane operations into `core/asyncevents`

(a) What is touched:
- new `core/asyncevents/src/admin.rs` (`pub mod admin`): `info`, `snapshot`, `retry`,
  `pause`, `resume`, `retire`, `skip` and `bump_generation`, moved from
  `tools/eventctl/src/lib.rs:79-330` with SQL and doc comments unchanged;
- the types and helpers they use move with them (`SubInfo`, `StateSnapshot`, `SkipOutcome`,
  `tools/eventctl/src/lib.rs:1-78`). `dsn()` stays in eventctl (CLI env parsing);
- new `poison_event(pool, id) -> Result<Option<PoisonEvent>>`, where
  `PoisonEvent { event_id: i64, topic: String, version: i32, payload: serde_json::Value,
  appended_at: String }`, using the `skip` predicate `:265-275` without the update. The
  implementer matches the column types of `core/asyncevents/src/store.rs` and reports them;
- new `resume_retry(pool, id)`: one UPDATE setting `state = 'active', consecutive_failures = 0,
  next_attempt_at = NULL, last_error = NULL, updated_at = now()`;
- `tools/eventctl` keeps CLI parsing and printing over `asyncevents::admin`;
- `tools/eventctl/src/lib_tests.rs` moves to `core/asyncevents/src/admin_tests.rs`. This is a
  move of existing tests with no new assertions;
- `tools/archcheck/src/main.rs:1656` drops `tools/eventctl` from the SQL skip list, and the
  doc comments at `:141` and `:1649` are updated.

(b) Why now: Step 10a's `core.events` provider reads through it.

(c) How: move, not copy. `resume` keeps its documented semantic (the failure count is left
to `retry`, `tools/eventctl/src/lib.rs:171-173`), and the CLI does not change.

(d) `[opus]` core-implementer.

### Step 4 — `app::run` serves the process `cp.*` face

(a) `core/app/src/lib.rs` and `core/app/Cargo.toml` (deps `controlplane`, `controlplanerpc`).

(b) Why now: after Step 2's types, and before any provider exists, so the first provider
(Step 12a) is reachable in the split the moment it contributes.

(c) How: in the edge block at `:909-932`, after `apply_edge_registrations` and beside
`register_process_describe`, call
`controlplanerpc::register_face(&mut server, ctx.contributions(controlplane::PROVIDER_SLOT))`
unconditionally, with a doc comment naming the `__describe` precedent. A process with no
edge server does nothing.

No static gate sees `cp.*` served:
- `__describe` carries only the `DESCRIBE_SLOT` concat (`:632-637`), and wire-only ops are
  excluded from it by design (`tools/rpc-macro/src/lib.rs:322`);
- routecheck models only `EDGE_SLOT` (`tools/routecheck/src/main.rs:233-238`).

The proof is the Step 25 `core/app` test plus splitproof. The edge-less list at
`tools/routecheck/src/main.rs:222-233` stays `server` / `admin-svc`, and its comment drops
the `admin.adminData` example.

`Manifest.writes` becomes authoritative (owner, 2026-11-10). In `core/controlplane`,
`LocalFace::invoke` returns `opsapi::Status::Forbidden` without calling the provider when
that provider's `manifest().writes` is `false`. `LocalFace` reads the manifests once at
construction. Every local and remote invoke path passes through `LocalFace`, so a provider
that does not declare `writes: true` cannot write.

(d) `[opus]` core-implementer (lifecycle ordering).

### Step 5 — Move the portal into core

(a) Move the crate:
- `git mv modules/admin core/admin`;
- workspace `Cargo.toml` (`:398` members path and the `admin` workspace dependency path);
- `core/admin/Cargo.toml` moves `asyncevents` from dev-dependencies to dependencies and
  deletes its comment at `modules/admin/Cargo.toml:53-55`, because rule 10 does not apply
  to core.

Conformance must keep covering the moved crate. Its drift preflight requires entries to
match `modules/*` on disk, and every monolith module except `CORE_INFRA_MODULES` to have
an entry (`tools/conformance/src/checks.rs:18, 60-78`; run at `main.rs:378-393`,
`tests.rs:664-677`).
- Add `CORE_HOSTED_MODULES: &[&str] = &["admin"]` beside `CORE_INFRA_MODULES`. These
  modules are exempt from the on-disk check but still require an entry.
- The `admin()` entry (`policy.rs:303-332`: login caps, argon parity, probes) stays.

Roles:
- DDL `admin.users.role text NOT NULL CHECK (role IN ('admin','operator','viewer'))` in the
  shared `USERS_DDL` (`lib.rs:186-190`);
- `tools/adminctl/src/{lib.rs,main.rs}`: `create-user <name> [--role R]` (default `admin`).
  On conflict it resets the password only and **never changes the role**. A new
  `set-role <name> <role>` changes the role;
- bootstrap callers pass `--role admin`: `tools/devctl/src/supervisor.rs:416`, `install.sh:42`,
  `install.ps1:54`, `weles/fleet.split.toml:39`, `weles/fleet.monolith.toml:24`,
  `tools/splitproof/src/main.rs:965`.

Direct `admin::` callers update their import path only: `tools/adminctl`,
`tools/conformance` (`policy.rs:37, 303-332`, `tests.rs:625-628`), `cmd/server/src/tests.rs:15`.

Docs and comments:
- `cmd/admin-svc/src/{lib.rs,main.rs}`: doc comments become "composition root: the core
  admin portal plus metrics; no domain module; DB-backed";
- `cmd/admin-svc/Cargo.toml` comment ("pure aggregator: no DB" is false today);
- `tools/checkmodules/src/lib.rs:44` is unchanged (it calls `admin_svc::modules`);
- `tools/archcheck/src/tests.rs:663` comment.

(b) Why now: Step 6's engine is written in `core/admin`. The role column must exist before
role checks.

(c) How: a move. Only the paths, the dependency swap and the role DDL change. The
archcheck schema scan for `modules/` no longer covers `admin`, which is correct: core owns it.

(d) `[opus]` core-implementer.

### Step 6 — Portal engine and admin-svc peer faces (no visuals)

(a) What is touched:
- `core/admin/src/engine/{sources.rs,manifests.rs,panels.rs,actions.rs,search.rs,roles.rs}`
  and `core/admin/src/routes.rs`;
- `cmd/admin-svc/src/lib.rs`: one `remote::Stub` per peer with
  `controlplanerpc::remote_factory(<peer>)`, for the same 13 peers and defaults as the
  deleted `admin_stub` list (`git show bb8c721f:cmd/admin-svc/src/lib.rs:24-40`);
- `cmd/admin-svc/Cargo.toml` (`controlplanerpc`).

(b) Why now: Steps 8, 10 and 12 render what the engine resolves. admin-svc switched now
makes the pilot provable in the split (Step 12c).

(c) How: implements **Portal**:
- sources: local `LocalFace` plus `SOURCE_SLOT`, routed by module;
- the manifest cache task (an empty `cp.manifests` reply is "no providers", not a failure);
- section merge and fallback;
- panel states;
- the invoke mapping;
- CSRF on every POST;
- `implemented: false` → 403 without a call;
- `admin.action` emitted on invoke (`actor` = login, `action` = `{module}.{action}`,
  `target` = ref, `detail` = reason);
- reveal through `admin.reveals`;
- `ADMIN_OPEN` → `Admin` and `Local Admin`.

(d) `[opus]` core-implementer.

### Step 7 — admincheck rewrite

(a) `tools/admincheck/src/main.rs` and `Cargo.toml`.

(b) Why now: it validates every manifest from the first port onward.

(c) How: build the **monolith** module list only (`cmd/server`), where every provider is
local and no edge is dialled. Collect `PROVIDER_SLOT` after `init` and call `manifest()`
(static, no I/O) on each. Checks, over the union of all manifests:
- ids are unique per module;
- every `Point`/`ActionPoint` entity kind is declared in some `entity_kinds`;
- every `Layout` id names a contribution of that module with `Point::Page(page.id)`;
- every `NavEntry.page` exists;
- every `NavEntry.counter` names a `Point::NavCounter` contribution;
- every `NavEntry.section` resolves;
- the section-merge conflicts described in Design.

`--strict` exits 1 on any finding.

The conformance writer check moves to manifests (owner, 2026-11-10):
- `tools/conformance` derives the writer set the same way: build the monolith module list,
  collect `PROVIDER_SLOT`, and take each manifest whose `writes` is `true`. Each writer
  must have an `InputByteCaps` CapCase for `cp.invoke` in its entry, whose cap equals the
  Validated `cp.invoke` `req.input.<value>` row. The entry name is `Manifest.module`, with
  `core.events` and `portal` mapping to the `admin` entry.
- Delete the hand-maintained `CONTROL_PLANE_WRITERS`, `control_plane_writer_dirs` (the
  text scan for `writes: true`), and their findings from `tools/conformance/src/{checks.rs,main.rs}`.
  This leaves one authority: the manifest that `LocalFace` enforces. The `cp.invoke`
  rationale const in `policy.rs` drops its `CONTROL_PLANE_WRITERS` wording in the same commit.
- Steps 9, 11, 15, 18, 20, 21 and 23 no longer add anything to a writer list. They ship
  only their probe and CapCase rows.

(d) `[opus]` core-implementer.

### Step 8 — Shell, primitives, assets

(a) What is touched:
- `core/admin/src/templates/{shell,primitives,modal,login}.html.tmpl` and `core/admin/src/admin.js`;
- the `/admin/assets/*` routes;
- delete `core/admin/src/{theme.css,admin.html.tmpl,modal.html.tmpl}` and the old login template.

(b) Why now: every screen step renders through these macros.

(c) How: one macro per primitive, plus Panel chrome with the seven states, with markup
lifted from `bundle.js`. The shell follows `registry.js:134-226`. No annotation layer, no
state picker.

(d) `[opus]` mockup-implementer (skill `mockup-implementation`; checklist = mockup line ranges + R1–R3).

### Step 8a — Make the portal↔module conventions a typed contract

The owner's rule (2026-11-10): "powinniśmy mieć jakiś kontrakt i w sporej części ten
kontrakt będzie wynikał z mockupów. W miarę możliwości ten kontrakt powinien być
testowalny i/lub statycznie weryfikowalny. Część rzeczy wyjdzie wraz z implementacją
samych mockupów."

**Standing rule for every later step.** A convention that the portal and a module must
both honour is never a string agreed in two places. The step that discovers it adds it to
`core/controlplane` as one of:
- a typed field;
- a pure encode/decode function pair that both sides call;
- a static manifest field that admincheck validates.

It ships with an admincheck rule, or a test in Step 25.

Owner decisions implemented here (2026-11-10):
- typed panel queries;
- a primary flag;
- the form submit target in the manifest;
- a `NoOp` outcome with an info flash;
- server-side form instances (`admin.forms`, an exception to R1's "no new tables": portal
  state, like `admin.reveals`), consumed on success;
- the form id passed to modules as `request_id`;
- item binding;
- an atomic claim, so repeated clicks on forms AND plain buttons are idempotent and never
  an error, including a nervous operator clicking many times a second;
- writer ports use `request_id` as their idempotency key where the module has one; a
  deliberate repeat of a plain button gets a new `request_id` (a click generation), so a
  module never mistakes it for a duplicate;
- a portal `validate_item` guard.

#### A. Contract changes (`core/controlplane/src/lib.rs`)

1. **`PanelQuery`.** Add `search: Option<String>`, `cursor: Option<String>`,
   `before: Option<String>` and `queue_state: Option<String>`. They replace the Step 8
   `params` keys `q`, `cursor`, `before` and `state`; `params` stays for module-specific
   keys only. (Step 13 adds `refs: Vec<String>` under this rule.)
2. **`ActionDecl.primary: bool`.** Replaces the `icon == "plus"` rule.
3. **`ActionPoint`.** `PanelTool(String)` names a contribution id of the same module; the
   action renders in that contribution's panel-tools slot. New variant
   `PanelForm(String)`, also naming a contribution id: the action is that contribution's
   `Primitive::Form` Save and is never rendered as a button anywhere else. This is the
   owner's "submit target in the manifest". It replaces the earlier
   `ContributionDecl.form_submit` wording; no such field is added.
4. **`InvokeOutcome::NoOp { message: String }`.** The request was valid but there was
   nothing to do.
5. **`InvokeCall`.** Add `item: Option<String>` (the bound element) and
   `request_id: Option<String>` (the form instance id, or the click request id of a
   plain button (B); the module's de-duplication key).
   The Design sentence about idempotency via `FormSpec.hidden` / `_idem_*` is replaced by
   `request_id`.
6. **`PropertyList.item: Option<String>`.** Binds the panel's `PanelTool` actions to one
   element. `QueueItem.id` already binds its `actions`.
7. **Field values.**

   ```text
   FieldValue = Text(String) | Int(i64) | Float(f64) | Bool(bool)
              | Multi(Vec<String>) | Money { amount: String, currency: String }
   pub fn decode_field(kind: &FieldKind, raw: &str) -> Result<FieldValue, FieldError>
   pub fn encode_field(kind: &FieldKind, value: &FieldValue) -> String
   ```

   Decode rules per kind:

   | Kind | Value | Accepted raw input |
   | --- | --- | --- |
   | String, Text | `Text` | any |
   | Ref(_) | `Text` | non-empty |
   | Int | `Int` | parses as `i64` |
   | Float | `Float` | parses as a finite `f64` |
   | Bool | `Bool` | `"true"` or `"false"` |
   | Enum(opts) | `Text` | a member of `opts` |
   | Multi(opts) | `Multi` | a JSON array of distinct members of `opts` |
   | Json | `Text` | parses as JSON |
   | Duration | `Text` | `^[0-9]+(s\|m\|h\|d)$` |
   | Money(currencies) | `Money` | a JSON object whose `amount` matches `^-?[0-9]+(\.[0-9]+)?$` and whose `currency` is in `currencies` |

   `FieldError { reason: String }`. `decode_field(k, &encode_field(k, v)) == Ok(v)` for
   every valid `v` (value → raw → value; raw → value → raw may normalise, e.g. `"01"` →
   `"1"`). `FieldValue` derives `PartialEq` only, because of the `f64`.

#### B. Form instances (`admin.forms`, in `core/admin/src/lib.rs` beside `REVEAL_DDL`)

```text
admin.forms (id text PRIMARY KEY, login text NOT NULL, module text NOT NULL,
             action text NOT NULL, contribution text, entity_kind text, entity_id text,
             item text, spec jsonb NOT NULL, generation bigint NOT NULL DEFAULT 0,
             incarnation text NOT NULL,
             claimed_at timestamptz, consumed_at timestamptz,
             expires_at timestamptz NOT NULL)
CREATE INDEX IF NOT EXISTS forms_expires_at ON admin.forms (expires_at)
```

- **Rows.** A row is a form instance (below) or a click claim (token-less path).
- **`spec` is tagged.** It holds `StoredSpec`, a portal-private serde enum in
  `core/admin/src/engine/actions.rs`: `Form(FormSpec) | Confirm(ConfirmSpec) | Click`,
  `#[serde(tag = "kind")]`. A row is never classified by its id prefix (a contribution may
  be named `click`).
- **Clock.** Every time comparison uses Postgres `now()`; the portal never compares its
  own clock with a stored timestamp.
- **Prune.** `DELETE FROM admin.forms WHERE expires_at < now() AND (claimed_at IS NULL OR
  claimed_at < now() - interval '30 seconds')`. Best-effort: a failure is logged. A row
  claimed by a request in flight is never pruned, so its settle always finds it.
- **Which forms get one.** A `Then::Form` or `Then::Confirm` modal, and an `ok`
  `Primitive::Form` panel with a `PanelForm` action. A readonly, stale, denied or error
  panel mints nothing, and its Save is disabled. So does an `ok` Form panel whose
  contribution has no `PanelForm` action: it renders readonly. admincheck cannot see this
  case, because the primitive arrives at runtime. A panel whose `PanelForm` action fails
  `admit` for the viewer (role, `implemented`, `writes`) also renders readonly and mints
  nothing.
- **Minting.**
  - The id is `<action or contribution id>:<new_token()>` (`lib.rs` ~:776);
    `incarnation` stores the same token. admincheck caps action and contribution ids at
    64 bytes, so a form `request_id` stays within the 128-byte idempotency-key cap of
    wallet and mail (`api/wallet/api/src/lib.rs` ~:79, `api/mail/events/src/lib.rs`
    ~:40).
  - `spec` is what was rendered: `StoredSpec::Form` for a `Then::Form` or a panel form,
    `StoredSpec::Confirm` for a `Then::Confirm`.
  - TTL is 1 hour.
  - Each mint, and each click upsert, first runs the prune.
  - A modal row is minted by the modal GET (`routes.rs` ~:630-660), which may carry
    `?entity=` and `?item=`. `item` passes `validate_item` first; failing that, the GET
    returns 400 and no row is minted.
  - A failed INSERT on the modal GET returns 503 with the error toast and no modal. On a
    panel, it renders the panel `error` state "Form unavailable".
  - A modal GET of a `Then::Link` action keeps today's `HX-Redirect`
    (`routes.rs` ~:647-649) and mints nothing; queue-item icon buttons rely on it
    (`primitives.html.tmpl` ~:240).
  - A modal GET of a `PanelForm` action or of a `Then::Invoke` action returns 400 and
    mints nothing: only `Then::Form` and `Then::Confirm` have modals. The now
    unreachable "direct invoke" modal kind is deleted in the core commit:
    `ModalView.kind = "invoke"` (`render.rs` ~:1346), its doc comment (~:1313), and the
    "direct invoke" branch of `modal.html.tmpl` (~:1).
- **POST `/admin/_action/{module}/{action}`, in this exact order:**
  1. **Parse the body** as `Vec<(String, String)>` (`routes.rs` ~:670). Parse only:
     repeated keys are kept; nothing is validated yet. `check_csrf` (`lib.rs` ~:558) and
     `ok_target` (`actions.rs` ~:293) read this list through one adapter (first value of
     a key); neither keeps a `HashMap` signature. `check_csrf`'s other caller, `logout`
     (`lib.rs` ~:950), uses the same adapter.
  2. **CSRF** → 403, zero calls. Then `actions::find` → 404, and a `Then::Link` action →
     400, both with zero calls and no row touched.
  3. **Path choice.** With `_form`, take the form path (steps 4–9). Without it, take the
     token-less path (below).
  4. **What the core commit deletes** (the allowed-key check itself is the first check of
     step 6):
     - The 8a core commit deletes the "every other field is input" rule (`actions.rs`
       ~:25, ~:125-129), `FIELD_REVISION` with its parse branch (~:29, ~:105-111), and
       the Required-reason check in `build_request` (~:117-123).
     - The modal stops posting `_entity`, hidden-map inputs and `_expected_revision`
       (`modal.html.tmpl` ~:10-11; template commit). Their Rust producers go in the core
       commit: `ModalView.entity` and its `entity_param` fill (`render.rs` ~:1312,
       ~:1336, ~:1343), `FormView.hidden` (~:956, fill ~:1011) and `FormView.revision`
       (~:959, fill ~:1014). The stored `FormSpec` in `admin.forms` is the only source of
       the hidden map and the revision. So that the core commit renders correctly on its
       own, it also deletes the template lines that read the deleted fields
       (`modal.html.tmpl` ~:10-11) and adds the hidden `_form`, `_csrf` and
       `name="_confirm"` inputs (C.1a, C.2); these are mechanical carriers, not layout.
       The template commit owns every visual change.
  5. **Claim** with one statement:

     ```sql
     UPDATE admin.forms SET claimed_at = now()
     WHERE id = $1 AND login = $2 AND module = $3 AND action = $4
       AND spec->>'kind' <> 'Click'
       AND expires_at > now() AND consumed_at IS NULL
       AND (claimed_at IS NULL OR claimed_at < now() - interval '30 seconds')
     RETURNING *
     ```

     The 30 s exceeds `INVOKE_DEADLINE` (10 s). When it returns no row, a `SELECT` by id
     classifies the reason:

     Rows are checked in this order; the first that applies wins:

     | # | Reason | Answer |
     | --- | --- | --- |
     | 1 | missing (no row) | 400, zero calls |
     | 2 | `spec` is `StoredSpec::Click`, or another login, other module or action | 400, zero calls |
     | 3 | `consumed_at` set, or claimed less than 30 s ago | repeated click: "Already submitted — this action is being processed or has completed", zero calls, never an error |
     | 4 | expired | 400, zero calls |
     | 5 | free (unexpired, unclaimed, unconsumed, matching): the holder released it between the UPDATE and the SELECT | run the claim once more; if it fails again, the repeated-click answer |

     Row 3 comes before row 4, so a double click on a form about to expire is never an
     error. The classification is a pure function,
     `classify_form(Option<&FormRow>, &Claimant, now) -> Claim` in `engine/actions.rs`,
     with `Claimant { login, module, action }` (the session login and the URL's module and
     action) and `Claim { Reject, Repeated, Retry }`. The classifying `SELECT` returns the
     row (if any) together with Postgres `now()`, so the function never reads a clock and
     Step 25 unit-tests every row and boundary without a database.

     The 400 does not say which reason applied. The repeated-click answer writes no flash
     and no `admin.reveals` row (the token-less path still runs its upsert first). An htmx request gets the neutral toast fragment; a non-htmx request
     gets a bare 303 (no flash) to `_return` when it passes the existing `_return` check,
     else the row's entity page, else `/admin`.

     **Every toast answer** (repeated click, 400, 403, 503, timeout, `Failed`) carries
     `HX-Retarget: #modal-root` and `HX-Reswap: innerHTML`, whatever the request's target,
     so a toast never replaces a panel form (the shell swaps 4xx/5xx,
     `shell.html.tmpl` ~:12).

  6. **Pre-invoke checks.** Each failure first releases the claim (`claimed_at = NULL`),
     then answers:
     - allowed keys → 400. The allowed set is `_csrf`, `_form`, `_reason`, `_confirm`,
       `_return`, every field key of the stored spec, and `<money key>__currency`. Only a
       Multi field's key may repeat;
     - `admit` (role, `implemented`, `writes`) → 403;
     - build each field's raw value, then decode it with `decode_field` → 422 with the
       field errors:
       - Multi: the JSON array of the field's repeated values (`[]` when none is posted);
       - Money: `{"amount": <key>, "currency": <key>__currency}`;
       - every other kind: the single posted value;
       - a stored `default` for Multi or Money is already in this JSON form;
       - a `readonly` field takes its stored `default`, and the posted value is ignored;
       - **blank** means: for String/Text, absent, or `""` when the field is required;
         for Multi, `[]`; for Money, an absent or empty amount; for every other kind,
         absent or `""` (browsers post empty inputs). An optional String/Text posted as
         `""` is sent as `Text("")`;
       - the rules apply in this order: a `readonly` field takes its stored default, or
         is left out of `input` when it has none, and is never "required"; then a blank
         optional field is left out of `input` (the only Multi rule: an optional Multi
         with no box ticked is left out, not sent as `[]`); then a blank required field
         is a `FieldError` "required";
     - Required `ConfirmSpec.reason` with an empty `_reason` → 422;
     - `_confirm` ≠ stored `type_to_confirm` → 422 (the input's name: C.1a). When the stored `type_to_confirm` is `None`, `_confirm` is not
       checked and not forwarded.
  7. **Invoke** with an `InvokeCall` built only from the row:
     - module, action, entity and `item` from the row;
     - `input` = `encode_field` per decoded field, plus the stored `hidden` map;
     - `expected_revision` = the stored `FormSpec.revision`;
     - `reason` from `_reason`;
     - `request_id` = the form id.
  8. **Settle the row** immediately, before the audit and the flash:

     | Outcome | Row |
     | --- | --- |
     | `Ok`, `NoOp`, `Conflict` / `Status::Conflict` | `consumed_at = now()`; kept as a tombstone until `expires_at` |
     | `FieldErrors`, timeout, `Failed` | `claimed_at = NULL` |

     Settle and every release match `id` AND the `claimed_at` the claim returned, so a
     late release never clears a later takeover. A failed settle is logged and does not
     change the answer. If the settle fails after an `Ok`, the row stays claimed and
     becomes re-claimable after 30 s, so a retry could reach the module again. That case
     relies on module de-duplication by `request_id`.
  9. **Audit and answer.**
     - `audit_detail` writes `noop: <message>` for `NoOp`; the other formats are unchanged.
     - After `Ok`, an audit failure keeps the existing 500 "action applied but audit
       append failed". After `NoOp` and every other outcome it logs a warning and keeps
       the outcome's status. The `matches!(.., Ok)` at `routes.rs` ~:712 names only `Ok`.
     - `Ok`: 303 with a `Success` flash.
     - `NoOp`: 303 to `ok_target(None, body, entity)` with an `Info` flash.
     - A 422 re-renders the same form from the stored spec with the SAME `_form`; today's
       `reshow` (`routes.rs` ~:696) changes to read the row.
     - A 409 re-renders it with the reload banner and Save disabled (the row is a
       tombstone).
- **Token-less path** (no `_form`), owner 2026-11-10 option 1a:
  - These checks run before any row is touched, each with zero calls: allowed keys
    `_csrf`, `_entity`, `_return`, `_item` (any other key → 400); `_entity` parses as
    `kind:id` or is absent (else 400); the action is `Then::Invoke` and its point is not
    `PanelForm` (else 400: a `Then::Form` or `Then::Confirm` must come with a form);
    `_item` passes `validate_item` (else 400).
  - **Click claim.** The portal derives a deterministic row id
    `click:<hex sha256(login \0 module \0 action \0 entity \0 item)>` (absent entity or
    item hash as the empty string) and upserts it with `spec` = `StoredSpec::Click`:

    ```sql
    INSERT INTO admin.forms (id, login, module, action, entity_kind, entity_id, item,
                             spec, incarnation, expires_at)
    VALUES (…, $incarnation, now() + interval '1 hour')
    ON CONFLICT (id) DO UPDATE SET expires_at = now() + interval '1 hour'
    ```

    `$incarnation` is a fresh `new_token()`. The conflict branch touches only
    `expires_at`, so a click row never classifies as expired, the upsert never disturbs a
    claim, and the incarnation changes only when the row is inserted again (after a prune
    or an admin-schema wipe). It then claims with:

    ```sql
    UPDATE admin.forms
    SET claimed_at = now(), consumed_at = NULL,
        generation = generation + CASE WHEN consumed_at IS NULL THEN 0 ELSE 1 END
    WHERE id = $1 AND login = $2 AND module = $3 AND action = $4
      AND (claimed_at IS NULL OR claimed_at < now() - interval '30 seconds'
           OR consumed_at IS NOT NULL)
      AND (consumed_at IS NULL OR consumed_at < now() - interval '5 seconds')
    RETURNING *
    ```

    A no-row result is classified by the inverse of this WHERE, not by the step 5 table,
    in a second pure function `classify_click(Option<&FormRow>, &Claimant, now) -> Claim`
    fed the same way, first match wins:
    - missing row (an admin-schema wipe between the upsert and the UPDATE) → `Retry`,
      where the retry repeats the upsert too;
    - login, module or action mismatch → 400;
    - claimed less than 30 s ago and not consumed, or consumed less than 5 s ago →
      the repeated-click answer;
    - anything else (the row became claimable between the UPDATE and the SELECT) → run
      the claim once more; if it fails again, the repeated-click answer.

    **Retry bound (both paths).** A `Retry` runs exactly once: for a form the claim, for a
    click the upsert plus the claim. If that attempt returns no row, the answer is the
    repeated-click answer without classifying again.
  - **`request_id` = `click:` + hex sha256(row id \0 incarnation \0 generation)**, 70
    bytes. The generation grows only when a claim takes over a consumed row, i.e. a
    deliberate repeat after the 5 s window; the incarnation changes when a pruned or
    wiped row is inserted again. Either way the module sees a key it has never seen and
    runs the action. A retry after a release (timeout, `Failed`, `FieldErrors`) keeps
    both, so the module recognises the same logical click (option 2a).
  - `admit` failures release the claim, as in step 6.
  - `input` is empty.
  - Then invoke, settle the row (Ok/NoOp/Conflict mark it consumed; the rest release it),
    and audit, as in steps 7–9.
- **`Flash.tone: FlashTone { Success, Info }`** (`actions.rs` ~:235-240). It has no
  `#[serde(default)]`. A pre-8a `admin.reveals` row fails to decode in `take_flash`
  (~:268-273), shows nothing, and expires within `REVEAL_TTL` (300 s). No DROP.
- **`validate_item(&str)`** in `core/admin/src/engine/actions.rs`: non-empty, at most 256
  bytes, no control characters. A forged item can reach a module; modules validate it
  against their own data, as `skip(expected)` does.

#### C. Rendering wiring

Both halves are in the core commit (`routes.rs`, `render.rs`) unless marked "template".
1. The panel handler (`routes.rs` ~:520-560) selects the module's actions with
   `ActionPoint::PanelTool(this contribution)`, binds them to `PropertyList.item` or
   `QueueItem.id`, and builds action items carrying the item:
   - a modal URL with `?item=`;
   - or `_item` in the token-less button's `hx-vals`.

   Template: render them in the panel-tools slot (`primitives.html.tmpl` ~:344).
1a. Modal form id. `ModalView` (`render.rs` ~:1307-1328) gains `form_id: String`, filled
   from the row the modal GET minted; the 422 and 409 re-renders fill it from the same
   row. Core commit (a carrier, B step 4): `modal.html.tmpl` carries
   `<input type="hidden" name="_form">` with it, and the confirm input gets
   `name="_confirm"` (~:20).
2. An `ok` Form panel with a `PanelForm` action mints a row (B) and renders a form posting
   to that action with `_form`, with the panel slot as the htmx target. Rust carrier
   (core commit): a `PanelFormView { form: FormView, post_url, csrf, form_id }` in
   `render.rs`, built by `render::panel` for `Primitive::Form` (~:643) from `Ctx.csrf`
   threaded through `PanelInput` (`routes.rs` ~:575-591), and filled again by
   `panel_form_result` on 422/409 from the same row. The core commit renders its
   `_csrf` and `_form` hidden inputs and the `hx-post` target. Template: the Save wiring
   and layout (`primitives.html.tmpl` ~:224-226).

   The POST answers for a panel form:
   - `Ok` / `NoOp`: `HX-Redirect`;
   - 422 / 409: the panel fragment, from a new `render::panel_form_result`, with the same
     `_form`;
   - every other answer is a toast retargeted to `#modal-root` (B, step 5), so the form
     stays on screen.

   Modal forms keep `render_result` (`routes.rs` ~:754-756).
3. `PanelQuery` is built from the typed fields: `routes.rs` ~:536 and `render.rs` ~:570,
   ~:770-776, ~:927, ~:1107 stop writing `q`/`cursor`/`before`/`state` into `params`.
   `engine/panels.rs` `cache_key` (~:83-96) includes the four fields, distinguishing
   `None` from `Some("")`.
4. `render.rs` `currency_key` (~:962) is deleted. The Money currency input name comes
   from one portal function, `engine::actions::money_currency_key(key) -> String`
   (`<key>__currency`; portal-private, modules never see the suffix),
   called by the POST decoder and by the renderer, which fills a new
   `FieldView.currency_name`. Template: the Money select and its readonly hidden input
   (`primitives.html.tmpl` ~:214) render `name="{{ f.currency_name }}"`; no template
   spells the suffix.
   Field keys must not contain `__`, must not start with `_`, and must not equal a
   `hidden` key, and every field `default` must decode with `decode_field`; the portal
   refuses a panel spec that breaks any of this with an error panel and no row.
   The read side follows the same codec: `render::field` (~:927-1000) decodes a stored
   `default` and a re-posted value with `decode_field`. The `value.split(',')` at ~:994
   and the `posted.get(&currency_key(..))` at ~:995 are deleted. `reshow`
   (`routes.rs` ~:693) takes the parsed `Vec<(String, String)>`, not a
   `BTreeMap<String, String>`, so a 422 re-render keeps every ticked Multi box.
5. Rust side (core commit): `ActionItem.primary`, a tone on `ActionResultView` and on
   the shell flash view, filled from `Flash.tone`, so no template hardcodes `err`
   (`modal.html.tmpl` ~:30).
   Template: every POSTing bare button carries `hx-disabled-elt="this"`, a row-menu
   item (`primitives.html.tmpl` ~:101, a `<div>`) is rendered as a `<button>` so the
   same attribute works, and every
   POSTing form carries `hx-disabled-elt="find button[type=submit]"` (a disabled `<form>`
   disables nothing). Both disable the control until the response arrives, so a normal
   browser sends one request per click burst. The server-side claim stays the authority. Each Multi checkbox gets
   `name="<key>"` and the hidden aggregate input (`primitives.html.tmpl` ~:212) is
   deleted. `admin.js` stops comma-joining
   Multi (`admin.js:118`) and posts the checkboxes as repeated keys. Primary comes from `ActionDecl.primary`
   (`shell.html.tmpl:47`). Info flashes render as a neutral toast.

#### D. Gates

- `tools/admincheck/src/checks.rs`:
  - every `PanelTool(c)` and `PanelForm(c)` names a contribution of the same module;
  - a `PanelForm` action is `Then::Invoke`;
  - at most one `PanelForm` action per contribution;
  - every action id and contribution id is at most 64 bytes;
  - at most one `primary` among the actions listed in each `PageDecl.head_actions`;
  - no `Then::Form` field key contains `__` or starts with `_`, and none equals a
    `hidden` key of the same spec;
  - every `Then::Form` field `default` decodes with `decode_field`.
- `tools/conformance/src/policy.rs`, all Opaque:
  - `cp.panel`: `req.search`, `req.cursor`, `req.before`, `req.queue_state` with
    `CONTROL_PLANE_READ_RATIONALE`;
  - `cp.invoke`: `req.item` with `CONTROL_PLANE_INVOKE_RATIONALE`, plus one sentence
    naming `validate_item` as the portal bound;
  - `cp.invoke`: `req.request_id` with `CONTROL_PLANE_INVOKE_RATIONALE`.

  (Opaque, because `cp.*` callers other than the portal are trusted mTLS peers, §8.)
- Re-bless the input golden, contract-golden and public-api, keeping only the lines this
  step causes.

(b) Why now: before the first provider (Step 9) codes against the strings.

(c) Add an Errata line: the Step 8 provisional conventions are superseded by Step 8a.

(d) Two commits, each reviewed:
- `[opus]` core-implementer: A, B and C (every Rust part, including C.1a's `form_id`
  and the deleted Rust producers in B step 4), D; adds `sha2 = { workspace = true }` to
  `core/admin/Cargo.toml`;
- `[opus]` mockup-implementer: every part marked "template" in B and C, and nothing
  else. Hidden carrier inputs (`_csrf`, `_form`, `_confirm` name) and the deleted reads
  of deleted fields are already in the core commit (B step 4).

### Step 9 — Portal-owned providers: backend

(a) `core/admin/src/{portal_provider.rs,events_provider.rs,fakes.rs}`.

(b) Why now: the dashboard and Event plane hosts exist before any module tile plugs in.

(c) How:
- `portal`: dashboard (`pages-core.js:45-69`) contributions, the attention host and fakes
  (values from `data.js`, each `// FAKE`).
- `core.events` (`pages-platform.js:130-157`): real pause / `resume_retry` / retry / skip
  via `asyncevents::admin` over `ctx.db()`; rewind is `implemented: false`; FAKE per the
  data-sources table.
- Skip what the operator saw (owner, 2026-11-10). The poison-event preview is a
  `PropertyList` with `item = Some(event_id)`. Its Skip action is a `Then::Confirm`
  `PanelTool` action, so its form row (Step 8a, B) stores that id, and the module
  reads it from `InvokeCall.item`. With `item: None` (a modal GET without `?item=`), the
  provider returns `opsapi::Status::Invalid` "Skip needs the previewed event" and does not
  call `skip`. The portal never skips an event the operator did not see.
  - `asyncevents::admin::skip` (`core/asyncevents/src/admin.rs` ~:240-320) gains a
    parameter `expected: Option<&str>`. It keeps ONE transaction body with the
    subscription row locked `FOR UPDATE`; there is no copied second implementation.
    `eventctl skip` passes `None` and keeps its current behaviour and errors.
  - The return type becomes `pub enum SkipResult { Skipped(SkipOutcome), AlreadyResolved { healthy: bool } }`.
    `SkipOutcome` is the existing struct, `admin.rs` ~:58.
  - With `Some(id)`, under the lock, each of these returns `AlreadyResolved` and moves
    nothing:
    - the healthy guard (~:255-261) would refuse;
    - no event is eligible (~:284-286);
    - the next eligible event id ≠ `id`.

    `AlreadyResolved { healthy: bool }` is not an error. With `expected: None`, the
    errors those first two cases raise today stay unchanged.
  - The `core.events` provider maps `AlreadyResolved` to `InvokeOutcome::NoOp` (Step 8a),
    with one message per case:
    - `healthy: true` → "Nothing to skip — the subscription is healthy";
    - otherwise → "Already resolved — the event was delivered or another event blocks
      now; refresh to see the current state".

    The audit `detail` is `noop: <message>`. Whichever of the retry and the click comes
    first wins.
- Writers: `core.events` has `writes: true`. Add its probe in `core/admin/src/conformance.rs` (`cp.invoke` input caps for
  `core.events`) with the CapCase rows in the `admin()` entry of `policy.rs`. The
  `cp.invoke` `req.input.<value>` policy row becomes Validated with the byte cap, as the
  old `admin.adminSubmit` row (`policy.rs:86` before Step 1) was.

(d) `[opus]` core-implementer.

### Step 10 — Portal-owned screens: view

(a) The templates and macros used by the dashboard and Event plane pages.

(b) After Step 9.

(c) 1:1 with `index.html` and `events.html` per R1–R3.

(d) `[opus]` mockup-implementer.

### Steps 11–24 — Provider ports (each step has a backend part `a` and a view part `b`)

**Port template.**
- **a** (`[opus]` core-implementer):
  - new `modules/<m>/src/admin.rs` implementing `controlplane::Provider`, with panels,
    actions, search and data per the data-sources table and R1–R2;
  - `modules/<m>/src/lib.rs` contributes it to `controlplane::PROVIDER_SLOT` in `init`;
  - `modules/<m>/Cargo.toml` adds `controlplane`;
  - a writer (`writes: true`) ships its probe in `modules/<m>/src/conformance.rs` plus
    its `policy.rs` CapCase rows;
  - `request_id` (owner, 2026-11-10, option 2a). An action with a non-idempotent effect
    uses `InvokeCall.request_id` as its idempotency key wherever the module already has
    one (e.g. wallet movements). Where it has none, the step's report names the action
    under **Leftovers for owner** as a documented double-apply risk, which arises only
    after a timeout, a `Failed` transport error after the module already applied the
    action, or a failed settle;
  - no test files (Step 25).
- **b** (`[opus]` mockup-implementer): template or macro adjustments the module's page needs,
  checked 1:1 against its mockup page per R3. When the generic macros already render the
  page exactly, b is a fidelity check with no commit.
- **Order:** after Steps 6–10.

The ports:

- **Step 11 — mail (pilot).** Outbox page (`pages-platform.js:53-71`), tile, attention.
  - REAL: counts, rows, cancel (pending), resume parked, requeue all, test send.
  - Disabled: pause sending, retry-now, park, cancel parked.
  - Writer: restores the mail probe deleted in Step 1.
- **Step 12 — pilot gate in the split (c).** No files.
  - Wipe schema `admin` first (`DROP SCHEMA admin CASCADE` via `psql`, per the wipe rule),
    because `CREATE TABLE IF NOT EXISTS` does not add the Step 5 `role` column to an
    existing table. `devctl up` then migrates it fresh and seeds `admin/admin`.
  - Run the `safe-verification` pre-flight, then `devctl up split`, then log in.
  - Fetch the mail page, each of its panel fragments and the row menu.
  - Make one real invoke (cancel a pending test message), then stop the fleet.
  - On any failure: stop, diagnose, and report to the owner with options. No fix loop
    inside this step (live-acceptance rule).
  - `[inline]` orchestration.
- **Step 13 — accounts.**
  - Users & players list (`pages-core.js:72-112`); kind `player` with `Point::EntityHeader`.
  - Profile & identities tab; search provider `player`; tile active players (DERIV).
  - Engine work (owner, 2026-11-10): `EntityListColumn` fan-out in `core/admin`.
    - For a DataTable whose `list_columns_for` names a kind, the portal calls every
      `Point::EntityListColumn { kind, .. }` contribution once per rendered page, with the
      page's entity ids.
    - The ids go in a typed `PanelQuery` field `refs: Vec<String>`, added to
      `core/controlplane` here per the Step 8a rule. Add it to `cache_key`, and add an
      Opaque `req.refs` policy row with `CONTROL_PLANE_READ_RATIONALE`.
    - Each column honours its `min_role`: a column whose role is denied makes no call and
      is not rendered. Each column call has its own `deadline_ms`, and the column calls for
      one page run concurrently after the table call.
    - The returned `Primitive::ColumnCells` is merged as extra columns.
    - A failing column renders an error icon in its cells, not a failed table
      (`catalog.js:15`).
- **Step 14 — characters.** Characters tab, summary fact, tab count.
- **Step 15 — wallet.**
  - Economy page (`pages-platform.js:164-185`; the global ledger is FAKE per R1).
  - Wallet tab; grant/revoke (REAL).
  - Embers list column (DERIV); tile embers spent (DERIV).
  - Writer.
- **Step 16 — inventory.** Inventory tab; grant item disabled.
- **Step 17 — friends.** Friends tab.
- **Step 18 — groups.**
  - Groups tab (DERIV via `page_mine`); promote member to admin (REAL, Q1).
  - Writer: restores the groups probe deleted in Step 1.
- **Step 19 — storage.** Cloud saves tab (`writes: false`); tile cloud saves used (DERIV).
- **Step 20 — notifications.** Inbox tab, Notify header action, Send notification row action. Writer.
- **Step 21 — apikeys.**
  - API keys page (`pages-platform.js:31-44`); search provider `apikey`; reveal.
  - Rotate/expiry disabled.
  - The reveal hint uses the gateway's real API-key header (read `modules/gateway`), not the
    mockup's `Authorization: Bearer` text.
  - Writer.
- **Step 22 — scheduler.** Scheduler page (`pages-platform.js:74-85`); all writes disabled.
- **Step 23 — config.**
  - Config & flags page (`pages-platform.js:88-109`).
  - Edit via `cp.invoke` with `expected_revision` = the global revision. The edit form is
    a `Primitive::Form` panel whose edit action has point
    `ActionPoint::PanelForm(<that contribution>)` (Step 8a).
  - The Revisions timeline is FAKE except for the current value and revision.
  - Writer.
- **Step 24 — audit.** Audit log page (`pages-platform.js:112-126`), and the dashboard
  "Recent operator activity" panel. Once every port has landed, admincheck also reports
  any peer admin-svc dials whose `<peer>-svc` hosts no provider (owner, 2026-11-10; held
  back until now because it would fire for every not-yet-ported peer). Step 24 also
  deletes every store/service item that Step 1's raze left unused and no port reused
  (`cargo clippy --workspace --all-targets -- -D warnings` must show no `dead_code`);
  until then the blocking clippy stage is red on that dead code by owner decision
  (2026-11-10).

The Q1 extras (groups promote-admin, wallet create-currency, "Purges pending" KPIs) are kept.

### Step 25 — Tests

(a) Unit tests:
- `core/controlplane/src/tests.rs`:
  - `LocalFace` routes by module;
  - an unknown module → NotFound;
  - a duplicate module panics;
  - section merge (fixed wins, smallest module name wins, unknown section falls back to `platform`).
- `core/controlplane-rpc/src/tests.rs`:
  - round trip of all four methods over a loopback edge;
  - `invoke` is not replayed after a connection-fatal error.
- `core/app` test: an edge process with one contributed provider serves `cp.manifests`
  with that manifest. With an empty slot it answers `cp.manifests` with an empty `Vec`.
- `core/asyncevents/src/admin_tests.rs`:
  - `resume_retry` resets failures and activates;
  - `poison_event` returns the first event past the cursor;
  - `resume` still leaves failures;
  - `skip(.., Some(id))` skips only the previewed event. It returns `AlreadyResolved`
    without moving the cursor in three cases: a retry delivered the event first (healthy
    subscription), no event is eligible, or another event blocks. `skip(.., None)` keeps
    the old errors. The `core.events` Skip with `item: None` returns Invalid without
    calling `skip`.
- `core/admin` tests for Step 8a:
  - method for every time window: never sleep and never race a real clock. A window
    test back-dates `claimed_at` or `consumed_at` with an `UPDATE` before the next POST.
    A concurrency test holds the fake provider on a `Notify` until every competing
    request has answered, so the overlap is structural;
  - claim: two concurrent POSTs with one `_form` invoke once. The other gets the
    "Already submitted" toast (htmx) or a bare 303 with no flash (non-htmx), and no
    `admin.reveals` row. A POST after completion (tombstone) gets the same answer. A
    claim back-dated past 30 s can be re-taken;
  - a late release after a takeover leaves the new claim in place;
  - `classify_form` and `classify_click` unit tests on built rows, a `Claimant` and a
    given `now`: each table row in order, each boundary (29.99/30.01 s claimed,
    4.99/5.01 s consumed), an expired row claimed < 30 s ago → Repeated, another login's
    free row → Reject, a missing row (form → Reject, click → Retry), and a free row →
    `Retry`; this is where the "released between UPDATE and SELECT" case is
    proven, since the two statements have no seam to interleave in an HTTP test;
  - a Money form rendered by the portal, with its input names extracted from the HTML
    and POSTed back, reaches the module as one Money object;
  - a panel spec with an undecodable readonly default renders an error panel and mints
    no row;
  - a computed `click:` id posted as `_form` gives 400 with zero calls, whether the
    click row is free, claimed or consumed;
  - a modal rendered by the portal carries `_form`, and posting its own inputs back
    takes the form path and invokes;
  - a panel form rendered by the portal carries `_csrf` and `_form`, and posting its own
    inputs back takes the form path and invokes; its 422 re-render carries the same
    `_csrf` and `_form`;
  - a panel whose `PanelForm` action the viewer cannot `admit` renders readonly and
    mints no row;
  - a modal GET of a `Then::Link` action answers `HX-Redirect` and mints no row; of a
    `Then::Invoke` action, 400;
  - a `Retry` that fails again answers repeated-click without a third attempt;
  - every toast answer to a panel-form POST carries `HX-Retarget: #modal-root`;
  - classify: a missing, expired, other-login or mismatched-action `_form` gives 400 with
    zero calls;
  - pre-invoke rejections release the claim, and an immediate corrected resubmit
    invokes. The rejections: `admit` 403, a field decode 422, an empty Required reason
    422, a wrong `_confirm` 422;
  - settle: `Ok`, `NoOp` and `Conflict` mark the row consumed; `FieldErrors`, timeout and
    `Failed` release it; an `Ok` whose audit append fails is still consumed;
  - the 422 re-render carries the same `_form`; the 409 re-render disables Save;
  - `InvokeCall.item`, `expected_revision` and `request_id` come from the row; a readonly
    field takes its stored default;
  - token-less: `input` is empty; a field key, a `Then::Form`/`Then::Confirm` action and a
    `PanelForm` action each give 400 with zero calls;
  - `validate_item` gives 400 at 257 bytes on `?item=` (no row) and on `_item` (zero
    calls);
  - a panel spec with a `__` key renders an error panel and mints no row; a failed mint
    gives the 503 toast on a modal and "Form unavailable" on a panel;
  - a cached `search=a` copy is never served for `search=b`;
  - `NoOp` gives a 303 with an `Info` flash and audit `noop: …`;
  - an unknown or wrongly repeated key gives 400 and releases the claim, and a corrected
    resubmit invokes;
  - POST decoding: repeated Multi checkboxes reach the module as one JSON array, which
    survives a value with a comma; `<key>` + `<key>__currency` reach it as one Money
    object;
  - blank optional fields: an empty Int, Float, Json, Duration and Ref, a Money with an
    empty amount, and a Multi with no box ticked are left out of `input`; the same blanks
    on a required field are a 422 "required";
  - a 422 re-render keeps every ticked Multi box, and a Multi default `["a,b"]` renders
    exactly one ticked box;
  - an `ok` Form panel without a `PanelForm` action renders readonly and mints no row;
  - token-less click claim: a burst of concurrent POSTs on one button invokes once (the
    provider held on a `Notify`). The rest get the toast with zero calls and no
    `admin.reveals` row. With `consumed_at` back-dated past 5 s, the next POST invokes
    again: the row's `generation` is + 1 and the `request_id` differs from the previous
    one. After a released claim (timeout), the retry
    keeps the same `request_id`. An upsert onto a row whose `expires_at` has passed
    invokes rather than giving 400. After the click row is deleted (prune), the next
    click's `request_id` differs from every earlier one;
  - blank-rule order: a required readonly field without a default is left out, not a
    422; a required String posted as `""` is a 422 "required", an optional one reaches
    the module as `Text("")`;
- `core/controlplane` tests for the Step 8a contract:
  - an `encode_field`/`decode_field` round trip for every `FieldKind`, including a Multi
    value containing a comma and a Money value;
  - every rejected input in the decode table is a `FieldError`: non-member Enum/Multi,
    non-finite Float, bad Duration, invalid Json, bad Money amount or currency.
- `tools/admincheck` fixtures for the Step 8a rules, one each:
  - a `PanelTool` or `PanelForm` naming an unknown contribution;
  - a non-Invoke `PanelForm`;
  - two `PanelForm` actions on one contribution;
  - two primary head actions;
  - a `__` field key, a `_`-prefixed field key, a field key equal to a `hidden` key;
  - a 65-byte action id and a 65-byte contribution id;
  - an undecodable `Then::Form` default.
- `core/admin` engine test for the Step 13 `EntityListColumn` fan-out: one call per page,
  and a failing column does not fail the table.
- `core/admin/src/engine/*_tests.rs`:
  - deadline → error; deadline with last-good → stale;
  - role below `min_role` → denied with zero provider calls;
  - `NotFound` → empty; `writes: false` → readonly;
  - `implemented: false` POST → 403 with zero calls;
  - `Conflict` → 409;
  - manifest refresh keeps last-good when a source fails;
  - `ADMIN_OPEN` invoke carries `Local Admin`.
- `core/admin/src/tests.rs` auth suite: the user-insert helper (old `:1118`) gains a role.
  The login, lockout, rate limit, CSRF and reveal assertions are kept.
- `tools/adminctl/src/lib_tests.rs`: `--role` on insert; a conflict keeps the role; `set-role`.
- `weles/master/src/fleet_toml_tests.rs:883-886`: expected args include `--role admin`.
- `tools/admincheck` tests: every finding kind, each with a failing fixture.
- One `admin_tests.rs` per ported module (mail, accounts, characters, wallet, inventory,
  friends, groups, storage, notifications, apikeys, scheduler, config, audit), covering
  its panels' data mapping and each REAL action's success and failure path.
- `tools/conformance` tests for the manifest-derived writer check: a `writes: true`
  manifest without a matching CapCase fails; a CapCase whose cap differs from the
  Validated row fails; `core.events` maps to the `admin` entry.
- `core/controlplane` test: `LocalFace::invoke` on a `writes: false` provider returns
  Forbidden and never calls the provider.

Splitproof:
- **Files:** `tools/splitproof/src/main.rs` and `tools/splitproof/src/party_checks.rs`.
- **Rewrite** these groups and helpers:

  | Group | Current lines |
  | --- | --- |
  | AD0–AD6g | `:4922-5450` |
  | ADX1–ADX3b | `:5058-5126` |
  | M3/M3b | `:303-384` |
  | WL6m | `:419-467` |
  | NT4m | `:514-538` |
  | ML3/ML4/ML4b | `:1486-1603` |
  | FR8 | `:2096-2111` |
  | GR5 | `:2538-2556` |
  | PH3 | `:3561` |
  | PH6 | `:3817` |
  | PH11 | `:4026` |
  | AU3 | `:5467-5472` |
  | WL3–WL6 | `:5563-5665` |
  | NT2/NT2b/NT2c | `:5733-5841+` |
  | NT4 | `:5965` |
  | PY7 | `party_checks.rs:291-293` |
  | helpers `wallet_form`, `inbox_form`, `mail_form`, `admin_csrf` | `:1086, :1110, :1298, :5226` |

  `wallet_form` returns an error on a missing form instead of empty strings.
- **Keep** the login, lockout, rate limit, CSRF, reveal and audit-trail assertions.
- **Add:**
  - a killed provider's panel renders the error fragment while another provider's panel on
    the same page returns 200;
  - a viewer gets `denied`, and 403 on invoke;
  - a config CAS edit through admin-svc → config-svc succeeds, and a stale revision returns 409;
  - one real invoke per writing provider (mail, wallet, groups, notifications, apikeys,
    config, `core.events` `resume_retry`).

(b) Why now: after Steps 1–24. (c) Split is the at-risk topology for every engine assertion.
(d) Unit tests: `[test-author]` (`model: "sonnet"`). Splitproof: `[test-author]` with `model: "opus"`.

### Step 26 — Docs

(a) What is touched:
- `.agents/shared/gamebackend.md`:
  - constraint 1 core list `:74-83` (+ controlplane, controlplane-rpc, admin);
  - constraint 10 `:158-164` (admin is core, not a module);
  - the module recipe `:62, :175-177`;
  - the module list and admin description `:261-287`;
  - the Commands admincheck line `:528`;
  - the Layout;
  - every other `admin` line the docs-writer finds in that file;
- `.agents/skills/add-game-module/SKILL.md` and `.agents/skills/mockup-implementation/SKILL.md`;
- `.claude/agents/mockup-implementer.md:3, :41-42, :51-54` and
  `.grok/agents/mockup-implementer.md:3, :49-50`;
- `docs/reference/event-plane-ops.md:123-134` and `docs/reference/module-reference.md`;
- `docs/roadmap/feature-tracker.md:230-232` (redesign, RBAC, event-plane surface);
- `docs/todo/admin.md`: ADMIN-01, 04, 05 and 06 are made obsolete by the redesign, so move
  them to `admin-done.md` with this plan's slug;
- dead-crate prose: `api/wallet/api/Cargo.toml:18-22`, `api/notifications/api/Cargo.toml:16-18`,
  `api/party/rpc/Cargo.toml:17`, `api/groups/events/src/lib.rs:40`, `core/opsapi/src/lib.rs:433`,
  `core/remote/src/lib.rs:1649-1650`, `Cargo.toml:58`, the admin prose in
  `cmd/{scheduler,audit,wallet,notifications}-svc` (Cargo.toml and lib.rs),
  `modules/apikeys/src/{store.rs,conformance.rs}` and `tools/opscatalog/src/lib.rs:20`;
- the hook path `.claude/hooks/ref_reminder.py:29` and its test `.claude/hooks/tests/test_hooks.py:693`
  (`modules/admin/templates/` → `core/admin/src/templates/`). Without this the
  mockup-skill reminder never fires on `core/admin`;
- errata in `docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md` (D1, and §8
  superseded by revision 2);
- memory: `memory/admin-extension-points-shipped.md`, `memory/generic-remote-admin-write-seam.md`
  and the `MEMORY.md` index, then `scripts/memory-sync.sh push`.

(b) Last, against landed code. (d) `[docs]` docs-writer (`model: "sonnet"`).

### Step 27 — Verification and live render comparison

(a) No files. (b) Terminal gate.

(c) Per `safe-verification`:
1. Run one `verifyctl` manifest (`--all --strict`).
2. Run `devctl up monolith`, log in, fetch every screen and fragment, and compare element by
   element against the mockup page.
3. Do the same through the split fleet.

The report lists the declared deviations.

(d) `[inline]` orchestration; report per `mockup-implementation` step 6.

## Owner answers

- **Q1 (answered: keep).** Groups "promote member to admin" (Step 18), wallet "create
  currency" (Step 15) and the "Purges pending" KPI (`docs/reference/event-plane-ops.md:125-130`,
  in each of the 7 modules' port steps) are kept as extra elements built from existing
  primitives in the mockup's style (D6).
- **Revision 2 (2026-11-10).**
  - Admin is core, with no admin module.
  - admin-svc is a composition root.
  - `admin.action` moves to core.
  - The fixed section list can be extended by module sections merged by id.
  - Raze first.
  - One registration mechanism.
  - Port steps are split backend/view.

## Declared deviations (known before implementation)

- **D1.** No admin-only `AuthReq` variant, although §8 mentions one. The `cp.*` ops are
  wire-only and the operator label is a request field. Errata in Step 26.
- **D2.** `user:` refs render from the `player` kind; the "staff" kind is FAKE.
- **D3.** No annotation layer, state picker, catalog page or states page. No Design-system nav section.
- **D4.** Real vocabulary in the mockup's layout where real data exists: `ak_` keys,
  interval seconds, `gold/gems`, `namespace`+`key`, `admin|member`, and mail `parked` for "failed".
- **D5.** Disabled actions and FAKE data per R1/R2 and the data-sources table.
- **D6.** Extra elements not in the mockup (Q1).
- **D7.** No `when` predicates on actions. The only mockup predicates gate Lore fragments,
  which are fakes.
- **D8.** A nav entry whose source has never answered since admin-svc started is absent,
  not greyed. Greying with the red dot applies once a last-good manifest exists.

## Open items for implementers (do not change scope)

- HTTP/2 and SSE are unverified; nothing here needs them.
- The manifest refresh dials all 13 peers when admin-svc starts. A down peer is retried
  every 5 s and logged at debug, as the gateway does.
- `include_str!` into `UILayout/design-system/` couples the admin build to that directory.
  No packaging context excludes it today.
- The routecheck SERVE-PARITY treatment of `cp.*` was read only at
  `tools/routecheck/src/main.rs:215-240, 460-490`. Step 4's implementer confirms it.

## Errata (during implementation)

- **Step 2 (04a9ece4).**
  - `ControlPlane` arguments are spelled `crate::PanelRequest` / `crate::SearchRequest` /
    `crate::InvokeRequest`. rpc-macro generates same-named wire structs in a
    `use super::*` module, so the bare names would refer to themselves.
  - `SECTIONS` is `&[(&str, &str, i32)]` (id, label, order).
  - The defaults are `DEFAULT_PANEL_DEADLINE_MS` / `DEFAULT_SEARCH_DEADLINE_MS`.
  - Types, fields and wire names are unchanged.
- **Step 3 (94d39ba7).** `PoisonEvent` follows the real schema: `event_id: String` (a text
  uuid), and `appended_at: String` read from `created_at::text`.
- **2026-11-10 (owner).** `Manifest.writes` is enforced by `LocalFace::invoke` (Step 4), and
  the conformance writer check derives writers from monolith manifests instead of a text
  scan and a hand list (Step 7).
- **Step 7 (0f187959).**
  - The monolith build and `PROVIDER_SLOT` collection is `checkmodules::monolith_manifests`,
    shared by admincheck and conformancecheck.
  - admincheck also reports a duplicate `Manifest.module` (`LocalFace::new` panics on it)
    and a `PageDecl.head_actions` id that names no action of the module (the portal drops
    it silently). Unique ids per module include `entity_kinds`.
  - Entity-kind checks cover `Point::{EntityHeader, EntityTab, EntityTabCount,
    EntitySummary, EntityListColumn}` and `ActionPoint::{RowAction, HeaderAction}`.
    `PageHead` and `PanelTool` carry no entity kind.
  - The conformance check is `control_plane_cap_findings`. Every `cp.invoke` CapCase in a
    writer's entry must equal the `req.input.<value>` row's cap, and a writer with that
    row not Validated is a finding.
- Step 6 review fix: admin.action is emitted for every invoke attempt; `detail` states the outcome (`ok` / `unknown: timeout…` / `failed: …` / `rejected: …` / `conflict: …`) plus the operator reason (owner, 2026-11-10); section merge lives in `controlplane`.
- Step 7 review fix (owner, 2026-11-10): admincheck is a BLOCKING verifyctl stage; it also checks admin-svc's dialled peers against every svc that hosts a provider (and monolith/split provider parity); conformance requires a cp.invoke CapCase per writing module. The reverse peer check (a dialled peer whose svc hosts no provider) is not enforced: all 13 dialled peers host none until Steps 11–24, so it awaits an owner decision; only a dialled peer with no `<peer>-svc` process is reported.
- Step 8: `/admin/assets/` also serves `htmx.min.js` and a small `portal.css` for styles the design system lacks (`.modal-icon.info`, work-queue state tones), plus the portal's `[hidden]`, `.slot` and theme-icon rules.
- Step 8 review fix (provisional, pending owner confirmation): the portal→module conventions Step 8 introduced. `PanelRequest.params` carries every panel query key but `entity` (`core/admin/src/routes.rs` `panel`), and the portal writes these keys: `q`, the table search (`core/admin/src/render.rs:770-771`); `cursor`, the next table page (`render.rs:776`); `before`, older timeline entries (`render.rs:927`); `state`, the work-queue tab (`render.rs:1107`). A Multi field posts its values comma-joined (`core/admin/src/admin.js:118`; read back at `render.rs:994`). A Money field posts `<key>` and `<key>_currency` (`render.rs:962-963`). A Bool field posts `"true"` or `"false"` (`admin.js:126`; `templates/primitives.html.tmpl:210`). A page-head action with `icon == "plus"` (and not `danger`) renders primary (`templates/shell.html.tmpl:47`).
- Step 2 rename (owner, 2026-11-10): `PanelRequest` / `SearchRequest` / `InvokeRequest` are `PanelQuery` / `SearchQuery` / `InvokeCall`, written bare in `ControlPlane`. This replaces the Step 2 `crate::` spelling: the trait tokens are copied into the exported `cp_control_plane_meta!`, where `crate::` names the invoking crate (clippy `crate_in_macro_def`). The new names cannot collide with rpc-macro's generated `{Method}Request` wire structs. Method names, field names and wire shapes are unchanged.

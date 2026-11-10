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
  operator: String }`. `operator` is only an audit label. Idempotency stays a module concern
  and uses hidden form fields (`FormSpec.hidden`), as wallet's `_idem_*` does today.
- `InvokeOutcome = Ok { message: String, reveal: Option<SecretReveal>, redirect: Option<Link> }
  | FieldErrors(BTreeMap<String, String>) | Conflict { current_revision: u64 }`.
- Portal HTTP mapping:

  | Outcome | HTTP |
  | --- | --- |
  | `Ok` | 303 PRG |
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
  `EntityListColumn` is fetched once per table page with `params["refs"]` = comma-joined ids
  and returns `Primitive::ColumnCells`.
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

### Step 9 — Portal-owned providers: backend

(a) `core/admin/src/{portal_provider.rs,events_provider.rs,fakes.rs}`.

(b) Why now: the dashboard and Event plane hosts exist before any module tile plugs in.

(c) How:
- `portal`: dashboard (`pages-core.js:45-69`) contributions, the attention host and fakes
  (values from `data.js`, each `// FAKE`).
- `core.events` (`pages-platform.js:130-157`): real pause / `resume_retry` / retry / skip
  via `asyncevents::admin` over `ctx.db()`; rewind is `implemented: false`; FAKE per the
  data-sources table.
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
  - Edit via `cp.invoke` with `expected_revision` = the global revision.
  - The Revisions timeline is FAKE except for the current value and revision.
  - Writer.
- **Step 24 — audit.** Audit log page (`pages-platform.js:112-126`), and the dashboard
  "Recent operator activity" panel.

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
  - `resume` still leaves failures.
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

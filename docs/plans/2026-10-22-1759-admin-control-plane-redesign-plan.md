# Admin control plane — the admin portal moves into core (plan)

Status: waiting for approval

Revision 2 (2026-11-10). Revision 1 (bb8c721f) kept the portal as the `modules/admin`
fortress and moved only the contract into core. The owner corrected that on 2026-11-10:
the whole admin becomes part of core and there is no admin module.

Revision 2026-11-18: Steps 1–13 have landed (Step 13 closed 2026-11-17, post-mortem
`2ed237a1`). Steps 28, 13c and 13d are written and not dispatched: the owner holds
implementation until the owner says go (2026-11-17). This revision moves the steps that are not
landed to the current agent workflow (`.agents/shared/planning-dispatch.md`, the
`implementation-dispatch` and `research-plan-present` skills). Landed steps keep their text.
- Tests are written by the step's implementer, to the bar "revert the change and the test
  must fail". `[test-author]` stays only for verify gates (goldens, `admindiff`, admincheck
  and conformance findings), splitproof assertions and new harnesses. The port template's
  "no test files (Step 25)" goes: each port's part `a` writes its `admin_tests.rs`. This
  supersedes the per-module `[sonnet]` test-author tier of Step 10b item 4 **Test tier**.
- Step 13c is cut into Steps 13c.1a, 13c.1b, 13c.2a, 13c.2b (code with its tests),
  13c.1g and 13c.3 (gate tests) and 13c.4 (docs), to fit the step size.
- From Step 28 on, every step runs `cargo test` on its touched crates (intermediate gate).
- Step 25 is cut into ordered Steps 25a–25h: fixes, backfill tests for the landed steps on
  `core-implementer`, then the gate tests and the splitproof rewrite on `test-author`.
- Step 24a writes its own tests. The splitproof assertions of Steps 13c and 15 are a
  `[opus]` test-author step after the code, not part `a`.
- Review: one `core-reviewer` round per step; a second only for `core/`, split wiring or a
  verify gate. Prose-only steps (`[docs]`, plan edits) get no review.
- Step 26's anchors are re-read against the tree after the agent-workflow simplification.

Revision 2026-11-18 (step size): unlanded steps re-cut to one mechanism of about 4–5 files
each (~30 tool calls per file measured on 13c.1a/1b); source
`docs/research/2026-11-18-2205-admin-plan-step-size-audit.md`. Each old step's decisions,
invariants, owner answers, X-rules and K-items moved into the sub-step that implements them.
An audit point the plan had not decided is an **Owner decision before dispatch** line in its
step.

Execution order once the owner says go: 28 → 13c.1a → 13c.1r → 13c.1b → 13c.1g → 13c.1h →
13c.2a1 → 13c.2a2a → 13c.2a2b → 13c.2a3 → 13c.2b1 → 13c.2b2 → 13c.3a → 13c.3b → 13c.4 →
13d1 … 13d6 → 14 … 24 (per port: Na0 when needed, Na1, Na2, Na3, Nt, Nb; port 24 as
24.a1 … 24.a3, 24.t, 24.a4, 24.b) → 24a → 24b0 … 24b4 → 25a … 25h4 → 26a … 26d → 27.
Open for the owner: X7, the order against
`docs/plans/2026-11-17-1900-purge-race-locks-plan.md` (Steps 6–7 of it remain); Steps 28,
13c and 13d touch none of the purging modules, so only Step 14 waits on that answer. Its
Step 7 also edits `tools/splitproof/src/main.rs`, `docs/reference/event-plane-ops.md` and
`.claude/rules/domain-modules.md`, which Steps 13c.3b, 15t, 25h1–25h4 (`main.rs`), 13c.4
and 26b (`event-plane-ops.md`) and 26a (`domain-modules.md`) edit: those never run at the
same time as it.

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
- From Step 28 on (revision 2026-11-18), each step also runs `cargo test -p <touched
  crates>` under `safe-verification`, green except the red-by-design items below.
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
- **Red by design from Step 1 until Step 25h4 (splitproof):**
  - splitproof's admin groups;
  - every non-admin group driven through the admin wallet grant: PH3, PH6, PH11, NT4, NT4m
    (`tools/splitproof/src/main.rs`) and PY7 (`tools/splitproof/src/party_checks.rs:282-336`);
  - the M3/M3b monolith-parity admin rows;
  - from Step 13c.1a until Step 13c.1h: Step 28's variant-coverage test, on the new
    variants only; from Step 13c.1b until Step 13c.1h also the two `Primitive` case-count
    checks `golden_cases_cover_every_macro_variant_and_state` and
    `variant_coverage_fails_on_a_new_enum_variant` (Step 13c.1b errata);
  - from Step 13c.2a2a until Step 13c.2a2b: Step 28's variant-coverage test, on
    `Point::EntityLabel` (Step 13c.2a2a errata; owner, 2026-11-19);
  - from Step 13c.2b2 until Step 13c.3a: Step 28's variant-coverage test, on
    `FilterKind::Select` (and on any other `FilterKind` variant 13c.2b2's owner decision
    brings into the gate: `Range`, `Bool` and `Ref`, which no golden draws; Step 13c.2b2
    errata, owner 2026-11-19).

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
    mints nothing: only `Then::Form` and `Then::Confirm` have modals. With the
    token-less 422/409 answers below being toasts, `render::modal` is never called for
    `Then::Invoke` or `Then::Link`; it takes the stored `FormSpec` or `ConfirmSpec`
    instead of the decl. The core commit deletes `ModalView.kind` (no template reads
    it; `render.rs` ~:1313, ~:1346, ~:1390) and the words "direct invoke" from the
    `modal.html.tmpl` header comment (~:1); the shared non-form branch (~:16-21) stays,
    it is Confirm's body.
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
       (`modal.html.tmpl` ~:10-11; core commit, see below). Their Rust producers go in the core
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
    and audit, as in steps 7–9. A click has no form to re-render, so its 422 and 409 are
    toasts (retargeted as every toast, step 5): `FieldErrors` → 422 with the joined
    `key: reason` errors; `Conflict` / `Status::Conflict` → 409 "This changed since you
    opened it — reload".
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
   `render.rs`, built by `render::panel` for `Primitive::Form` (~:643) from the routes
   `ctx.csrf` (`routes.rs` ~:541) threaded through `PanelInput` (declared `render.rs`
   ~:517-536, built `routes.rs` ~:554-570), and filled again by `panel_form_result` on
   422/409 from the same row; `panel_form_result` takes the panel frame (title, icon,
   mode) from the row's `contribution` in the manifest snapshot. `form` is
   `#[serde(flatten)]`, so the existing `panel_form` reads (`primitives.html.tmpl`
   ~:226, ~:280) keep working in the core commit. The core commit renders its
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

### Step 9a — Contract gaps found in Step 9: backend

(a) `core/controlplane/src/lib.rs`, `core/controlplane-rpc`, `core/admin/src/{engine/*,render.rs,routes.rs,events_provider.rs,portal_provider.rs}`,
`tools/admincheck`, `tools/conformance/src/policy.rs`, `tools/conformance/input-fields.golden.tsv`,
`docs/reference/contract-golden/contracts.txt`, `docs/reference/public-api-baseline/controlplane.txt`.

(b) Why now: Step 9 found mockup elements the contract cannot carry (owner, 2026-11-17:
"dodaj brakujące kontrakty"). Step 10 must be 1:1 with the mockup, and Steps 11–24 port
pages that use the same elements: panel meta appears on about 50 mockup panels, tiles
above panels on five pages.

(c) Decisions (one per gap) and the invariants they must keep:
1. **Panel envelope.** `Provider::panel` and `ControlPlane::panel` return
   `PanelBody { primitive, meta, heading, note, banner }` instead of a bare `Primitive`.
   - `meta: Option<String>`: the muted text right of the title ("since 14:02").
   - `heading: Option<String>`: rendered mono after the declared title ("Paused:
     `<id>`"). The declared `ContributionDecl.title` stays the static part, so a failed
     or loading panel still has a title.
   - `note: Option<String>`: the muted text in the tools row ("last 4 h").
   - `banner: Option<Banner>`, with `Banner { tone: Tone, text: String }` using the
     existing `Tone`.
   - All four are plain text, never HTML.
   - A module banner shows in `ok` and `readonly`. It never shows in empty, stale,
     error, denied or loading; those states keep the portal's own banners.
   - The wire shape changes in place; both sides are one build (wipe rule).
2. **Code block.** `PropertyList` gains `code: Option<String>`, shown below the items
   in a code block. The provider bounds it (the poison payload already caps at 4096). The
   view (Step 10) puts the tools of a `PropertyList` panel with `code` in the body footer,
   as the mockup's poison resolver does.
3. **Tiles above a layout.** `PageDecl` gains `tiles: Vec<String>`, a tile row rendered
   above `layout`. `Layout::Tiles` is deleted in this step, and with it the `"tiles"`
   layout name and its engine, template (`shell.html.tmpl`) and admincheck branches, so
   there is one way to write a tile row. The Design section's contract sketch is
   superseded here. The tile render mode comes from membership in `tiles`. admincheck applies
   the existing `layouts()` rule to `tiles` (every id is a `Point::Page(<this page>)`
   contribution of the same module). An id in both `tiles` and `layout` is a finding.
4. **Prepared confirms and forms.** A `Then::Confirm` or `Then::Form` action whose text
   depends on the bound item sets `ActionDecl.prepare: true`.
   - On the modal GET, after `admit` (role, `implemented`, `writes`), the portal calls a
     new method `prepare(PrepareCall { module, action, entity, item, operator }) -> Result<Then, opsapi::Error>`.
     It is `#[retry_safe]` and is the fifth `ControlPlane` method, in `LocalFace` and
     controlplane-rpc. A refused action makes no `prepare` call.
   - `Provider` gains `prepare` with a default that returns NotFound, so providers
     without prepared actions do not change.
   - The call is bounded by the invoke deadline. Every failure toasts and mints no row:
     - a timeout is a 504 toast;
     - a provider `Err` is a toast with its mapped status;
     - a returned variant other than the declared one, or a prepared `Form` that fails
       `form_spec_errors`, is a 503 toast.
   - On success the portal stores the returned spec in the `admin.forms` row exactly as
     Step 8a stores the declared one. There is no fallback to the declared text: the
     declared spec only fixes the variant and is what admincheck validates.
   - The provider builds the text from `PrepareCall.item`, never from a fresh read of
     "the current" element. So the operator confirms the element they saw, and the
     stored `type_to_confirm` is what Step 8a enforces.
   - admincheck: `prepare: true` is valid only on a `Then::Confirm` or `Then::Form`
     action whose point is not `PanelForm`.
   - Conformance: `cp.prepare` gets policy rows for `req.module`, `req.action`,
     `req.entity.*`, `req.item` and `req.operator`, matching the `cp.invoke` rows for
     the same fields. `CONTROL_PLANE_READ_RATIONALE` names `cp.prepare` as read-only.
     The input-fields golden and `contracts.txt` gain the new method and fields.
5. **Dashboard full-width row.** New point `Point::DashboardWide`, rendered full width
   below the attention and activity grid. The portal's Service health moves there and is
   no longer a `DashboardPanel`.
6. **Dashboard time range.** `PanelQuery` gains `range: Option<TimeRange>`, where
   `TimeRange` is `H1 | H24 | D7`.
   - The dashboard has a 1 h / 24 h / 7 d switch with query key `range`.
   - Every dashboard-point request carries `Some(range)`. A missing key means `H24`, and
     an unknown value is a 400. `range` is never left in `params`.
   - Non-dashboard requests carry `None`.
   - `range` is part of `cache_key`, and it has an Opaque policy row with
     `CONTROL_PLANE_READ_RATIONALE`.
   - A contribution that ignores it is correct.
   - Refresh re-fetches every panel on the page; it is view-only (Step 10).
7. **Dashboard nav entry.** No contract change. `/admin` is the portal's own route, so the
   shell renders a fixed "Dashboard" entry at the top of the nav, as it renders the brand.
   A `NavEntry` keeps linking only to module pages.
8. **Row highlight.** `Row` gains `highlight: bool` (the mockup marks the paused
   subscription's row).
9. **Attention host meta** ("3 items from 3 modules") is an aggregate over separately
   fetched slots, so it is view-only: `admin.js` counts the swapped attention rows and
   their modules (Step 10). No contract change.
10. **Step 9 providers use all of the above.**
    - Event plane page:
      - its KPI tiles go in `tiles`, the rest is `Layout::Stack`;
      - paused subscription rows set `highlight`.
    - The poison panel:
      - `heading` is the subscription id, `meta` is "since …";
      - the error `banner` names failures and the waiting count;
      - `code` is the payload, and the hint is `note`;
      - its `PropertyList` keeps only Event, Consumer and Error;
      - the Subscription, Since, Payload, Failures, Waiting and hint rows are deleted.
    - Skip is `prepare: true`. The mockup's event number (`12905`) is a sequence the plane
      does not expose, so the event is named by the first 8 hex characters of its
      `event_id`, taken from `item`. This is a declared deviation.
      - Title: "Skip event `<short id>`?".
      - Impact: subscription, event (topic + short id). The mockup's "Goes to:
        dead-letter list" row is left out (declared deviation): the plane keeps no
        dead-letter list, a skipped event is only stepped past.
      - Type-to-confirm: the short id.
    - Dashboard: Service health becomes `DashboardWide` with `meta` and the "last 4 h"
      `note`.
    - The Step 9 workarounds are deleted.

Tests (Step 25 adds them; listed here so the design is checkable):
- a `PanelBody` round-trips over the loopback edge;
- a module banner shows in ok and readonly and not in stale or error;
- `prepare` round-trips, and an action without `prepare` makes no call;
- a denied, not-implemented or readonly action makes no `prepare` call;
- a prepared Confirm is stored, and its `type_to_confirm` is the one enforced;
- each prepare failure (timeout, `Err`, wrong variant, a Form failing
  `form_spec_errors`) toasts and mints no row;
- admincheck findings for: `prepare` on an invalid variant or point; a `tiles` id that is
  not `Page(this page)`; an id that is in both `tiles` and `layout`;
- `range` reaches dashboard providers, an absent `range` equals `H24` with the same cache
  key, and an unknown value is a 400.

(d) `[opus]` core-implementer. The view parts go to Step 10.

### Step 9b — Remaining view carriers: backend

(a) `core/controlplane/src/lib.rs`, `core/admin/src/{engine/*,render.rs,routes.rs,events_provider.rs,portal_provider.rs,fakes.rs}`,
`core/admin/src/templates/*` (carrier reads only, see below), `tools/admincheck`, and the
public-api baseline.

(b) Why: the Step 10 spec extraction (2026-11-17) found six mockup elements the contract
still cannot carry. The owner chose to add carriers for all six rather than declare
deviations.

(c) Decisions and invariants. Where an old field is replaced, it dies in the same step
(one rail).
1. **Bar cell.** `Cell` gains `bar: Option<u8>`, a fill percentage. The renderer clamps
   it to 100. It renders as the mockup's lag bar (`pages-platform.js:129`) before the
   value.
   - When `bar` is set, `tone` colours the fill only and the value stays plain mono:
     - `None` gives the default fill (`--ok`);
     - `Warn` gives `.w`;
     - `Err` gives `.e`;
     - `Ok` and `Info` give the default fill.
   - The `core.events` Lag column sets `bar` with the mockup's scale
     (`min(100, max(3, lag / 3))`) and the tone it already has.
   - The column is no longer right-aligned.
2. **Reason hint.** `ConfirmSpec` gains `reason_hint: Option<String>`, the placeholder of
   the reason input. Skip sets one in the mockup's style.
3. **Typed impact values.** `ConfirmSpec.impact` becomes `Vec<(String, Cell)>`, so a
   value can be mono or toned, as in the mockup. Every existing impact row is converted.
4. **Spark colour.** `KpiTile` gains `spark: Palette`, where `Palette` is `Chart1 | Chart2 |
   Warn`. Those are the colours the mockup uses (`registry.js:48-50`): the design-system
   `--chart-1`/`--chart-2` tokens and `--warn`. Every provider sets it explicitly:
   `Chart1` unless the mockup gives another colour. A port that needs another colour adds
   a variant.
5. **Last value on a failed tile** (engine rule, no contract change):
   - A failed fetch with a cached copy younger than `cache_ttl_s` stays `stale`, as today.
     The value is shown in the value slot with the stale badge, as in the mockup.
   - A failed fetch whose only copy has expired, but is at most 1 h old, is `error`. For a
     `KpiTile` only, the error tile then shows "last value <age> ago: <value>"
     (`bundle.js:180`) from that copy.
   - The panel cache keeps an expired copy (bounded by its existing cap) and exposes it
     for this purpose only. In `error`, a copy older than 1 h is never shown. A tile
     whose contribution declares no `cache_ttl_s` has no copy and so no last value.
   - In `error` the last value appears only in that muted text, never in the value slot.
6. **Action emphasis.** `ActionDecl.primary: bool` is replaced by
   `emphasis: Emphasis { Primary, Default, Ghost }`. The Step 8a rule "at most one primary
   per `head_actions`" moves to `Emphasis::Primary`, and admincheck keeps enforcing it.
   Copy payload is `Ghost`.
   - `ActionDecl` also gains `trailing_icon: bool`, which draws the icon after the label
     (the mockup's "Audit log →", `pages-core.js:55`). admincheck flags it on any action
     that is not `Then::Link`. Its first user is the audit port (Step 24), with `Ghost`.
   - `ActionItem` (the view model) exposes the emphasis instead of `primary`.
7. **Carrier reads in the templates.** Every template read of a field this step removes is
   moved to its replacement in this step, so the build renders correctly on its own:
   - `a.primary` becomes the emphasis;
   - an impact value renders through the existing `cell` macro.

   New markup (bar, hint, spark colour, last value, ghost, trailing icon) is Step 10.

Tests for Step 25:
- a bar cell and a typed impact cell round-trip, and `bar` is clamped to 100;
- admincheck still flags a second Primary, and flags `trailing_icon` on a non-Link action;
- a failed KPI tile:
  - with a copy younger than its ttl is stale;
  - with an expired copy at most 1 h old is error with the last value;
  - with an older copy shows none.

(d) `[opus]` core-implementer. The markup goes to Step 10.

### Step 10 — Portal-owned screens: view

(a) The templates and macros used by the dashboard and Event plane pages.

(b) After Step 9b.

(c) 1:1 with `index.html` and `events.html` per R1–R3. Also two generic macro changes
(owner, 2026-11-17):
- Cancel. In a modal, Cancel closes the modal. In a panel form, Cancel restores the
  values the form was rendered with; today it is disabled.
- Panel tools. The `panel-tools` slot also renders in state `empty`, so an action such
  as "Add" can create the first row. A provider leaves out the actions that make no
  sense on an empty panel. Tools stay hidden in `error`, `loading`, `denied` and
  `unavailable`.
- Step 9a view parts:
  - panel `meta`, `heading`, `note` and module `banner`;
  - `PropertyList` `code`, with footer tools;
  - `Row.highlight`;
  - the attention host's aggregate meta, counted in `admin.js`;
  - the `PageDecl.tiles` row;
  - the `DashboardWide` row;
  - the dashboard range switch and Refresh;
  - the fixed Dashboard nav entry;
  - KPI delta colours from `Delta.good`, not from an arrow prefix.
- Step 9b view parts:
  - the bar cell (fill colour from tone, value plain);
  - the reason placeholder;
  - typed impact values;
  - the spark colour;
  - the error tile's last value;
  - the ghost emphasis and the trailing icon.

(d) `[opus]` mockup-implementer.

### Step 10a — Fixes found rendering Step 10's real states

(a) `core/controlplane/src/lib.rs`, `core/admin/src/{engine/*,render.rs,routes.rs,events_provider.rs,portal_provider.rs,fakes.rs,templates/*}`,
`tools/admincheck`, and the public-api baseline.

(b) Why: the Step 10 render of real states and its review found four gaps. Owner decisions, 2026-11-17.
This corrects the earlier assumption, which was wrong, that a provider can leave tools out
per render: tools come from the static manifest.

(c) Decisions and invariants:
1. **Tools on an empty panel (L2, option a).** `ActionDecl` gains `when_empty: bool`.
   - A `PanelTool` action renders on an empty panel only when `when_empty` is set. This
     replaces Step 10's "tools also render in state empty" for every other action.
   - admincheck flags `when_empty` on any action that is not a `PanelTool`.
   - The `core.events` poison actions keep `false`.
2. **Read-only chrome (L1).** A `writes: false` module's panel gets the read-only chip and
   banner only when the contribution has a write affordance. That means one of:
   - a `PanelTool` action whose `then` is not `Then::Link` (a link is not a write, and
     `admit` lets it through);
   - a `PanelForm` action;
   - a `Form` body.

   Any other panel of such a module renders `ok`.
   - The rule lives in one place in the engine. Step 8a's separate route rule (a `Form`
     with no usable `PanelForm` is readonly, in `routes.rs`) stays as it is.
   - The portal's Service health then has no read-only chrome, as in the mockup.
   - This supersedes the unconditional "`writes: false` → readonly" in Step 25's engine
     tests.
3. **Delta colour (L3).** `Delta.good: Trend` is replaced by `Delta.tone: Option<Tone>`,
   which states the colour directly, as the mockup classes do (`bundle.css:251-252`):
   - `Ok` gives `up`, `Err` gives `down`, `Warn` gives `bad-up`, and `Info`/`None` give
     `flat`.
   - `Delta.good` and `Trend` die in this step: `Delta.good` is `Trend`'s only user.
     Their chain goes too: the imports, the `delta_class` arm and its doc, the
     `fakes::delta` helper's `Trend` parameter, and the baseline lines.
   - `Delta.tone` gets a doc comment.
   - The providers set the mockup colour per tile:
     - dashboard Paused is `Err`;
     - Max lag and Lore store are `Warn`.
4. **Service list layout.** `ServiceStatus` gains `columns: u8` (1 or 2) and
   `value_label: Option<String>`, the right-hand column header.
   - Today every Services body renders as the dashboard's two columns with "p99". The
     mockup's other ServiceStatus panels are one column with a "lag N" value
     (`pages-lore.js:228-229,261-262`).
   - The view splits into columns only when `columns` is 2, and prints the label only
     when it is set.
   - Service health sets 2 and "p99".
   - `ServiceItem.p99_ms: Option<u32>` is replaced by `value: Option<String>`, which the
     provider formats: Service health gives "33 ms", a lag panel gives "lag 4 s".
     `p99_ms` dies here with its chain (the fakes, the renderer, the baseline). The
     hard-coded "p99" header in the panel chrome is deleted, and `value_label` replaces
     it.

Tests for Step 25:
- a `when_empty` tool shows on an empty panel and an ordinary tool does not;
- admincheck flags `when_empty` off a `PanelTool`;
- a `writes: false` panel:
  - without a write affordance is `ok`;
  - whose only tool is a `Then::Link` is `ok`;
  - with a non-link tool, a `PanelForm` or a `Form` is `readonly`;
- a one-column `ServiceStatus` without a label renders a single list without "p99", and
  shows each item's `value` exactly as the provider gave it;
- each `Delta.tone` maps to its class.

(d) `[opus]` core-implementer; it also makes the template reads follow.

### Step 10b — Mockup translation tooling, skill and agent

(a) Files:
- `core/admin/src/testdata/`, the golden test;
- new crate `tools/admindiff` and its accept files `tools/admindiff/accept/<page>.txt`;
- `.agents/skills/mockup-implementation/SKILL.md` and its pointer
  `.claude/skills/mockup-implementation/SKILL.md` (frontmatter included);
- `.claude/agents/mockup-implementer.md` and `.grok/agents/mockup-implementer.md`;
- the UI-lane rule in:
  - `.agents/shared/planning-dispatch.md` (~:67, ~:96);
  - `docs/reference/implementation-mode.md` (~:14);
  - `.agents/adapters/{claude,codex,cursor,grok}.md`;
- the node prerequisite in `docs/reference/testing.md`;
- this plan's port template, Step 12 and Step 27 (edited in this revision, below);
- memories `follow-uilayout-mockup-faithfully` and `specialized-core-agents`, and the
  `MEMORY.md` index line (the repo copy `memory/MEMORY.md` and the live copy).

(b) Why: owner, 2026-11-17, after the Step 10 post-mortem. Ports in Steps 11–24 should be
cheap and mostly mechanical, and repeated checks should be automated so they do not spend
model tokens. Step 10's cost came from:
- contract gaps found only during translation;
- states found only by rendering;
- a throwaway render harness;
- hand-run live checks with curl.

(c) Decisions and invariants.

1. **Golden render test** (`[opus]` test-author).
   - It renders every primitive macro and the panel chrome, from view models built in
     code, to `core/admin/src/testdata/<name>.html`.
   - It covers each `PanelState`: ok, empty, readonly, stale, error, denied. It also
     covers the slot placeholder (`loading_block`) and the emphasis, bar, code,
     heading/meta/note/banner, `when_empty` and two-column variants.
   - Deterministic: fixed csrf, fixed `age_s`, no clock, no random ids.
   - Without `ADMIN_BLESS=1`, these all fail:
     - any diff;
     - a missing golden file;
     - a golden file the test no longer produces.
   - `ADMIN_BLESS=1 cargo test -p admin golden` rewrites the set, deleting orphans.
2. **`tools/admindiff`** (`[opus]` core-implementer). It is a dev tool under the Dev
   tooling scope rule: simple, not hardened, trusted local operator. Typed CLI flags, no
   config file. `--base-url` defaults to the monolith `http://localhost:8080`; the split
   uses `:8085`. Login `admin/admin` is the default.
   - **`crawl`**
     - Login must land on the dashboard shell. Anything else, such as the login page
       served with 200, fails.
     - It follows two discovery hooks, which are its contract with the templates:
       - nav links → pages;
       - `.slot[hx-get]` → fragments, fetched with `HX-Request`.
     - `data-menu-url` row menus are fetched for status only. The mockup builds menus in
       the DOM, so they are never compared.
     - It writes each response to `target/admindiff/<run>/`. It prints one line per
       fragment: status, panel state, title.
     - It fails on any non-2xx, any error-state fragment (unless `--allow-errors`), zero
       nav pages, or zero fragments.
   - **`mock`**
     - It renders `P.<page>.render()` from `UILayout/redesign/assets/pages-*.js` (with
       `data.js`, `registry.js` and the design-system `bundle.js`) through `node`. Node
       is a dev prerequisite, documented in `testing.md`; a missing node is an error,
       not a skip.
     - An embedded harness stubs the globals the builders read: `window`; `document`
       (listeners, empty queries, `createElement`, `body`, `documentElement.dataset`);
       `localStorage`; `matchMedia`; `location.search`. `after` hooks are not run.
   - **Page map.** One named list in the tool maps each mockup page key
     (`P.<key>` in `pages-*.js`) to a portal route. Entity pages get a fixed entity id
     for the crawl and a `location.search` for the harness. A second named list holds
     the excluded keys, with a reason each: `states`, which Declared deviation D3
     covers, and Lore pages until a Lore module exists. Both lists are validated
     against the mockup (every key exists) and against each other (no key in both).
     `crawl` fetches every mapped route, entity pages included, besides what the nav
     hooks discover.
   - **Assembly.** Before a skeleton is built, each fetched fragment replaces its
     `.slot[hx-get]` element (outerHTML) in document order. So the portal side is the
     loaded page, never loading placeholders. Unit-tested.
   - **`diff --page <mockup page key> --portal <crawl dir>`**
     - Both sides reduce to a class skeleton: element names, `class` lists, nesting.
       Text is dropped unless `--text`.
     - Repeated siblings (`tbody tr`, list items, `.svc`, `.pop-row`) collapse to their
       distinct skeletons, so N fake rows against M real rows is not a difference.
     - Two named lists, each validated against its own side so a dead entry is an error,
       say which wrappers are unwrapped and which are removed:
       - mockup: `frag` is unwrapped; `.frag-label` and the devbar are removed;
       - portal: `.slot` and its `hx-*` attributes are unwrapped.
     - Panels match by `panel-title` text with markup stripped. Tiles match by
       `kpi-label`. When a title holds data or is absent, the key is the panel's
       position on the page.
     - Every panel and tile on either side is either matched or reported. A page with
       zero matched panels fails.
     - `tools/admindiff/accept/<page>.txt` holds one entry per accepted difference.
       - An entry that matches no current difference is an error (a stale accept).
       - This file is the page's declared-deviations source. A step report cites it
         instead of re-listing deviations in prose.
     - Exit non-zero on any difference not accepted.
   - **Errata (as built, 2026-11-17).**
     - A third template hook: every `panel_fragment` root carries `data-state`, because
       every panel state answers 200 and a nav counter's error state renders no markup;
       `crawl` reads the state there and fails a fragment without it.
     - A page is named by its mockup file stem (`index`, `events`), which is also the
       accept file name; its `P.<key>` is read from the file's `PAGES.boot` call. The
       page map lists every mockup file, as mapped, `Pending` (printed by every `crawl`,
       refused by `diff`) or excluded. `catalog` is excluded under D3 too.
     - Both sides compare from `.app`. The mockup also strips `.annot-legend`. The
       drawing inside an `svg` is compared only with `--text`.
     - A panel or tile key is the title's or label's own text: a nested element's text
       (a heading, a badge) is left out and a trailing colon trimmed.
     - Only the containers in the named `REPEAT` list collapse to their distinct shapes:
       `tbody > tr`, `ul > li`, `.svc`, `.pop-row`, and `.uptime > i`, whose bars count
       by tag alone because a bar's class is data. The list is validated against every
       mapped and pending mockup page; the portal side is not checked, as most of its
       pages are not ported yet. Every other sibling list compares by position, with
       `order` and `moved` lines. A unit's placeholder in the frame carries its key
       (`[panel "Paused"]`), and a line naming an element with children carries a short
       hash of its skeleton (`div.uptime#3f0d1e`).
     - `crawl` writes a `CLEAN` marker only when clean, and `diff` refuses a run without it.
     - Accept entries are `<difference> | <reason>`. The password comes from
       `ADMINDIFF_PASSWORD` (default `admin`), so it never sits in argv.
3. **Skill `mockup-implementation`, rewritten for the control-plane portal.** It drops the
   stale `modules/admin` / `adminapi` / `theme.css` cheat-sheet. Per port:
   1. Extract the checklist (element → mockup line → carrier → status) BEFORE the port's
      backend step, so a contract gap becomes a decision in part `a`. The checklist
      has a states column (ok, empty, readonly, stale, error) per panel, plus the
      module's `writes` and tools.
   2. Every mockup class maps to `CellKind`, `Tone`, `Emphasis` or `Palette`. Anything
      that does not map is an accept entry with a reason.
   3. The panel chrome slot order is documented once, beside `bundle.js:130-149`: icon,
      title + heading, meta, ro-chip, tools (legend, note, value label, actions),
      banners (stale | readonly, module, form), body, footer tools.
   4. Failure handling in `admin.js` uses delegated htmx events, never `htmx.ajax`
      promises.
   5. Closing the loop takes three checks: the golden diff; `admindiff diff` clean
      against the page's accept file; a clean `crawl`. The orchestrator, not the view
      lane, runs the `safe-verification` pre-flight and boots `devctl up` after the
      view commit, because templates are compiled into the binary. The view lane never
      starts a fleet.
   - **Errata (as written, 2026-11-17).** The view lane runs and blesses the golden test
     before its commit (`ADMIN_BLESS=1` regenerates `testdata/`), and the orchestrator
     re-runs `cargo test -p admin golden` as the gate, first in its live sequence and
     before any tool build, as no Cargo command may run once the fleet is up.
4. **The UI lane rule changes once, at its source** (owner, 2026-11-17).
   - `planning-dispatch.md` gets the carve-out in shared terms. It amends the
     `[mechanical]` row and its prose so that a port's view part (`b`) with a complete
     checklist and no open contract gap may run at the cheap tier on the
     `mockup-implementer` persona. Each adapter maps that tier for its runtime: Claude
     `subagent_type: "mockup-implementer", model: "sonnet"`, and the others their own
     cheap tier.
   - Opus stays on checklist extraction, contract decisions and review.
   - Every other place that states "never sonnet / never a mechanical lane" for UI points
     to that rule instead of repeating it: implementation-mode, the four adapters, the
     agent frontmatter, the two memories.
   - The agent prompt carries the item 3 rules.
   - **Pilot:** Step 11 (mail) runs its `b` on sonnet. It passes when all of these hold:
     - `admindiff diff` is clean against its accept file;
     - `crawl` is clean;
     - the review's first round returns PASS or comment-only findings.

     Otherwise a post-mortem finds what the skill, the checklist, `admindiff` or the
     golden tests missed, and those are fixed before the next port.
   - **Owner, 2026-11-17: no fallback to a stronger model.** The goal of the plan is a
     workflow cheap enough that the view part always runs at the cheap tier. When a port
     goes badly, the answer is to fix the skill, tools and tests.
   - **Test tier** (owner, 2026-11-17). Tests that ARE a gate stay `[opus]` test-author:
     golden coverage, `admindiff`, admincheck and conformance findings, splitproof, and
     any test whose job is to prove that a guard fails closed. Per-module provider tests
     (`modules/<m>` `admin_tests.rs`) and small fixture tests (item 5) are repeated
     patterns and run `[sonnet]` test-author. The mail pilot's `admin_tests.rs` is
     written at `[opus]` and becomes the reference file the later ports copy. proof-auditor
     audits the first sonnet test file, then every port whose review finds a test that
     does not reach its once-wrong branch. The no-fallback rule above applies here too.
   - A post-mortem also runs after Step 13 (accounts, the first port with new engine
     work), and after any port whose review needs more than one fix round. Its fixes
     land before the next port.
   - **Pilot result (Step 11, 2026-11-17).** Part `b` ran on sonnet (b84775cf, 6753541e,
     8d025b32). `admindiff diff` and `crawl` were clean; the first core-reviewer round was
     a REJECT, so the pilot failed its third criterion.
     - Findings: (1) the queue search box disappears when a search narrows the queue to
       zero items (template defect); (2) the mock's "failed" row (`tr#d4bd69`) was
       labelled a data difference, not declared deviation D4, although the checklist
       named D4.
     - Causes:
       - P1. The checklist went stale after the owner's G1–G13 decisions. Its §5 still
         proposed `mailctl seed-mockup`, which G13 overruled, and `b` went looking for it.
       - P2. `admindiff diff` labels a collapsed mock-only row only by its skeleton hash,
         so `b` could not tell the failed row from the retrying rows and gave all three
         one reason.
       - P3. Accept kinds are free text; nothing ties an entry's kind to the kind the
         checklist predicted.
       - P4. The states column has no "narrowed to zero by search or filter" state,
         although `table_empty_search` was a known table case. `crawl` never searches, so
         `admindiff` cannot see that state either.
       - P5. The checklist did not predict two entries: the toolbar span/form pair and the
         frame `[panel "Outbox"]` placeholder. SKILL §6 allowed `b` no kind the checklist
         did not name, but `b` copied both from precedent, so rule and practice disagreed.
       - Outside the view part: the review reported that `with_param` keeps `cursor` on
         the tab and search URLs. Checked against the code (b5eb40d5) it does not:
         `render::panel` builds `Ctx.query` without `cursor` and `before`, and every URL
         builder starts from it (goldens `queue_later_page` and
         `table_highlight_row_menu` pin this). b5eb40d5 adds the comment that states the
         rule. Lesson: a review finding is a lead; check it against the code before
         recording it as a cause.
     - Fixes:
       - Skill §1: the checklist is updated in place once the owner decides its gaps,
         before part `a` (P1); the states row gains "narrowed to zero" with its golden
         case (P4); the accept plan predicts every entry with its kind and reason (P3,
         P5); every URL the renderer builds for a panel is audited for the query keys it
         keeps and drops.
       - Skill §6: `b` copies the predicted kind verbatim and returns any unpredicted
         line, except a word-for-word repeat of another accept file's pair, which it
         copies and flags (P3, P5).
       - The Step 11 checklist gets the decisions, the overruled §5 proposal, D4 on
         `tr#d4bd69` and the two unpredicted entries.
       - Code, dispatched separately: a readable label for a collapsed row in
         `admindiff diff` (P2).
    - **Post-mortem (Step 13, 2026-11-17).** Part `a`: 1bea67f8, 6f2de49f, review fix
      183fedc6 (two rounds). Tests: 0dd57f9b, ed41431f, 9ce5eb84, 97df9b4c. Part `b` changed
      no template; accept files 8b5f4775, 8cd11e8c. Monolith and split `crawl` clean and
      `diff` clean for every page; the part `b` review had one prose finding.
      - Findings:
        - Q1. The golden enum guard restated the templates' drawing rules in test code, and
          four audit rounds each found a new drift. Each round patched the copy instead of
          naming the cause.
        - Q2. The §7 `diff` runs without `--text` and `admindiff` never compares attributes,
          so a difference in an attribute or text passes: the toolbar search width
          (ADMIN2-35), the sort arrow's column (D9), the crumb and the foot count. D9 can have no accept entry.
        - Q3. The mock harness never runs `P.players.meta.after`, so the header shape of
          a column a later port contributes (`th > span`) is never compared.
        - Q4. The skill's §7 split line named only `crawl`. `demoseed` at its default
          `:8080` hits characters-svc in the split and gets a 404 on `/accounts/register`;
          the split crawl only had data because the monolith run had seeded the shared
          database.
        - Q5. The orchestrator read `starting (6/6 healthy)` as ready and sent `devctl down`
          during the split's boot, which aborted it. In the session before, a fleet found up
          was waited on for about 55 minutes and then stopped without asking whose it was.
      - Fixes:
        - Q1: plan Step 28 (coverage observed from the render) replaces the copy; review
          findings now state their origin and a structural one goes to the owner as a fix
          for the cause (`.agents/shared/planning-dispatch.md` **Adversarial Diff Review**,
          `proof-auditor` attack 6; c68277e4, 0160f0ed).
        - Q2, Q3: skill §1 gains a hand-check section per page, and the step report cites it
          beside the accept file (§6). The port that contributes a list column names its
          `meta.after` shape in its checklist.
        - Q4: skill §7 crawls the split with `demoseed --base-url http://127.0.0.1:8082`.
        - Q5: owner question. The devctl `starting` / `running` fact and asking whose a
          fleet is belong in `.agents/shared/gamebackend.md` **One test rollout at a time**,
          which today says to wait for or stop the owning rollout; the skill does not
          restate them.

5. **Mockup-state seed** (owner, 2026-11-17). A fresh dev fleet has no paused and no
   lagging subscription, so about a third of the Event plane's and the dashboard's accept
   entries were data differences, and the designed Paused panel was never compared.
   - **Where.** A new verb `eventctl seed-mockup` in `tools/eventctl`, with its SQL in
     that tool (`src/seed.rs`). `admindiff` and `demoseed` stay without database access;
     demo fixtures do not go into `asyncevents::admin`. It is a dev tool under the Dev
     tooling scope rule.
   - **What.** Two subscription rows with no registered consumer, so no worker ever
     claims them. The implementer cites the worker code that guarantees this, for both
     topologies.
     - `seed.mockup-paused.v1`: paused, with failures and `last_error` as the mockup's
       paused row and Paused panel show (`pages-platform.js:128-161`). Its cursor sits
       before the first event of the topic, so `poison_event` returns a real event.
     - `seed.mockup-lagging.v1`: active, with the mockup's lagging row (warn badge, lag
       bar), cursor at the same place.
     - The topic is the one with the most events in the log. No events at all is an
       error that says to run `demoseed` first. Whether the lag reaches warn is not
       checked by the seed; `admindiff diff` checks it, because the re-cut accept files
       no longer accept the lagging row's absence.
   - **Idempotent.** Each run resets both rows to the designed state, whatever state they
     are in, including retired. Running the verbs on them (resume, skip, retire) is
     harmless. The rows hold the retention floor until retired or wiped; the usage text
     says so.
   - **Run order** before every comparison: `devctl up`, `demoseed`,
     `eventctl seed-mockup`, `admindiff crawl`, `admindiff diff`. The item 3 skill
     states it.
   - **Accept files.** After the seed lands, the orchestrator runs that sequence. The
     data-difference entries the seed resolves are removed from
     `tools/admindiff/accept/{index,events}.txt`, which `diff` enforces, as such an
     entry is now stale.
   - **Ports.** ~~A port's part `a` adds the data its page's designed state needs:
     through `demoseed` when the module's public API can produce it, otherwise through
     the module's own operator tool.~~ Superseded (owner, 2026-11-17): no per-module seed
     tools. `demoseed` becomes the one seeder and seeds every kind of data each module
     can create (Step 24b). Until then a port adds no seed: a designed state a fresh
     fleet lacks is a `data difference` accept entry, and Step 24b removes those
     entries. `eventctl seed-mockup` stays as the Event plane's dev fixture (Step 24b).
   - **Errata (as built, 2026-11-17).**
     - archcheck bans `asyncevents.<table>` SQL outside the plane, splitproof and tests,
       so `tools/eventctl/src/seed.rs` (that one file) joins its skip list.
     - The rows sit on the `(topic, contract_version)` stream with the most events, since
       lag and `poison_event` match on the version. The genesis position is the lagging
       row's cursor. Both rows keep `start_kind` `genesis` and `spec_hash`
       `<topic>|v<n>|genesis`; the paused row's cursor is described below.
     - The lagging row carries 3 failures and a `last_error`, because only an active row
       with failures gets the warn badge.
     - The paused row's cursor is not genesis. It sits on the event just before the
       stream's middle event, so `poison_event` returns that middle event and the paused
       row's lag is strictly below the lagging row's, as in the mockup (118 against 273).
       With equal lag, the Max lag tile named the paused row and showed an Err delta
       where the mockup shows the lagging row's warn delta. The seed therefore needs at
       least two events on the stream and refuses a one-event stream as it refuses an
       empty log.

This revision also changes the port template (b → `[sonnet]` mockup-implementer under
item 4; checklist first), Step 12 (use `admindiff crawl` against the split) and Step 27
(use `admindiff diff` per page).

Tests: item 1 is itself a test. `admindiff` ships its own unit tests in item 2, covering
the skeleton, collapse, strip lists, assembly, page map, matching and stale-accept rules.
Item 5 gets `[sonnet]` test-author tests (item 4, **Test tier**) against local Postgres: both rows created, a
re-run resets a resumed or skipped row, `poison_event` returns the topic's first event,
and an empty log is refused.

(d) Order and lanes:
- 1 `[opus]` test-author, then 2 `[opus]` core-implementer; one at a time.
- 5 `[opus]` core-implementer after item 2's tests land, then its tests, then the
  accept-file re-cut (core-implementer, from the orchestrator's crawl output).
- 3 and 4 `[opus]` docs-writer, after 1, 2 and 5 have landed, so the skill cites real
  commands.
- core-reviewer reviews each item. proof-auditor also audits items 1 and 2, because they
  are gates.

### Steps 11–24 — Provider ports (each step has a backend part `a` and a view part `b`)

**Port template.** Steps 11 and 13 landed with one part `a`. From Step 14 on (revision
2026-11-18, step size) part `a` is cut into the parts Na0–Na3 below; port-specific cuts are
in the port bullets.
- **Before a:** `[opus]` checklist extraction per the `mockup-implementation` skill
  (Step 10b item 3). Every contract gap it finds becomes a decision in Na0; each data
  decision goes to the a-part that implements it.
- **Every a-part** (`[opus]` core-implementer; one `[review]` core-reviewer round on that
  part's cumulative diff):
  - every `// FAKE:` marker and `implemented: false` action it adds gets an item in
    `docs/todo/admin-second-pass.md`, in the same commit;
  - tests in the part, written by its implementer to the bar "revert the change and
    the test must fail" (revision 2026-11-18): `modules/<m>/src/admin_tests.rs` covers
    what the part adds (the first a-part creates the file, later parts extend it). A golden
    case or splitproof assertion the port needs is a `[opus]` test-author step (Nt) right
    after the a-part that lands what it covers, never the a-part's own;
  - no seed data (Step 10b item 5, **Ports**, as superseded): missing data is a
    `data difference` accept entry until Step 24b;
  - `order` values follow the mockup's order, around the ones Step 9 took:
    - dashboard tiles: accounts 10, storage 40, mail 50, wallet 70 (portal holds 20,
      30, 80; core.events holds 60);
    - attention: mail 40 (core.events 20; portal 10, 30, 50);
    - platform nav entries: 10–50 (Event plane is 60);
    - audit's "Recent operator activity" is a `DashboardPanel`.
    - (owner, 2026-11-17, research doc X2) entity tabs: characters 20, wallet 30,
      inventory 40, friends 50, groups 60, storage 70, notifications 80 (accounts' Profile
      tab 10); facts: characters 30, Lore 40 reserved, wallet 50 (accounts 10, 20); list
      columns on players: characters 10, Lore 20 reserved, wallet 30; header actions:
      notifications Notify 20 (accounts Suspend 30); row-menu groups: wallet 40,
      inventory 50, notifications 60, storage 80 (accounts 20, 30); Purges-pending attention
      items: characters 61, wallet 62, inventory 63, friends 64, groups 65, storage 66,
      notifications 67.
  - Cross-cutting rules (owner, 2026-11-17, research doc X); X1 is Na3's, X4 Na1's, X8
    Na2's, X9 Step 24.a4's:
    - X1: each purging module (characters, wallet, inventory, friends, groups, storage,
      notifications) contributes "Purges pending" as a `DashboardAttention` item shown
      only while its sweep is behind (at the order above); it replaces the Q1 KPI wording.
    - X4: each port reuses or deletes its own dead admin readers.
    - X5: an operator is a user cell linked to the audit page filtered by actor;
      `system`, `scheduler` and `apikey:<name>` are plain tags.
    - X7: Steps 14–20 touch the seven purging modules that the untracked
      `docs/plans/2026-11-17-1900-purge-race-locks-plan.md` also edits; the owner orders the
      two rollouts before the first of them starts.
    - X8: writer action ids read like the mockup (`key.create`, `flag.update`); a writer
      maps `request_id` to its existing idempotency key (wallet `idempotency_key`).
    - X9: the "nav entries … arrive with their ports" accept line shared by five accept
      files keeps one wording and is swept once, at Step 24 (24.a4).
- **Na0** (only when Before a finds a contract gap): the core contract and its render
  (`core/controlplane/src/lib.rs`, `core/admin/src/render/*.rs`, templates, the public-api
  baseline), with its tests; then **Nt0** (`[opus]` test-author) for its golden. Never folded
  into Na1. Depends on Before a.
- **Na1 — read surface.** Depends on Na0 when there is one.
  - New `modules/<m>/src/admin.rs` implementing `controlplane::Provider` with
    `writes: false`: pages, tabs, panels, search and kinds at the X2 orders, with data per
    the data-sources table and R1–R2; store readers in `modules/<m>/src/store.rs` (X4).
  - `modules/<m>/src/lib.rs` contributes it to `controlplane::PROVIDER_SLOT` in `init`;
    `modules/<m>/Cargo.toml` adds `controlplane`.
  - The page's entry in `tools/admindiff/src/pages.rs` (errata 2026-11-17):
    `Route::Pending` becomes `Route::Mapped(<route>)`, as `diff` refuses a pending page;
    its accept file `tools/admindiff/accept/<page>.txt`.
  - Tests: `admin_tests.rs` covers the panels' data mapping. Target: at most 7 files,
    about 60 tool calls.
- **Na2 — writes** (writers only). Depends on Na1.
  - Actions and `invoke`, `writes: true`; `implemented: false` actions with their todo
    items.
  - `request_id` (owner, 2026-11-10, option 2a). An action with a non-idempotent effect
    uses `InvokeCall.request_id` as its idempotency key wherever the module already has
    one (e.g. wallet movements). Where it has none, the step's report names the action
    under **Leftovers for owner** as a documented double-apply risk, which arises only
    after a timeout, a `Failed` transport error after the module already applied the
    action, or a failed settle.
  - The probe in `modules/<m>/src/conformance.rs` plus its CapCase rows in
    `tools/conformance/src/policy.rs`.
  - Tests: each REAL action's success and failure path.
- **Na3 — contributions outside the module's own page or tab.** Depends on Na2 (Na1 for a
  non-writer). X1 Purges pending, dashboard tile, players list column, tab count,
  `EntityLabel`, nav counter: each is one reader and one contribution. Tests: each reader's
  data mapping, and Purges pending shown only while the sweep is behind. Folded into Na1 for
  a port estimated under 60 tool calls (Steps 16, 17, 19, 22).
- **Nt** (`[opus]` test-author): the golden cases and splitproof assertions the port needs;
  proof-auditor audits a splitproof assertion.
- **Nb**, the view part `b` (`[sonnet]` mockup-implementer under Step 10b item 4; an open
  gap in the checklist is closed in part `a` before b starts, never by a stronger b): the template or macro
  adjustments the module's page needs. Checked by
  the golden diff, `admindiff diff` against `tools/admindiff/accept/<page>.txt`, and a
  clean `crawl`, run by the orchestrator. When the generic macros already render the page
  exactly, b is a fidelity check with no commit. Runs after the port's last a-part and Nt.
- **Order:** after Steps 6–10b. Within a port the parts run in order and are not
  file-disjoint (they share `admin.rs` and `admin_tests.rs`). Ports run in sequence,
  never in parallel: they share `tools/admindiff/src/pages.rs`,
  `tools/conformance/src/policy.rs`, `docs/todo/admin-second-pass.md` and
  `core/admin/src/golden_tests/cases.rs`, and only one test rollout runs at a time.
- **Owner decision before dispatch (X7, Steps 14–20):** the order of the ports against
  `docs/plans/2026-11-17-1900-purge-race-locks-plan.md` (Steps 6–7 of it remain). The
  audit gives no recommendation.

The ports:

- **Step 11 — mail (pilot).** Outbox page (`pages-platform.js:53-71`), tile, attention.
  - REAL: counts, rows, cancel (pending), resume parked, test send (requeue all dropped, G11).
  - Disabled: pause sending, retry-now, park, cancel parked.
  - Writer: restores the mail probe deleted in Step 1.
  - **Decisions (owner, 2026-11-17, from the checklist
    `docs/plans/2026-11-17-step11-mail-checklist.md`, gaps G1–G13): "do the visual part and add the missing
    contract".**
    - Contract, in part `a`:
      - G1: new `ActionPoint::QueueRow` for per-item actions. A `PanelTool` on a
        WorkQueue panel draws as a panel tool ("Pause sending", disabled).
      - G2, G3, G5: WorkQueue gains what the mockup's outbox table has. That covers the
        search box, a filter, the pager foot, a selectable checkbox column, and declared
        extra columns with cells per item (Template, To as a plain email).
      - G6(b): the attempt bar draws a fixed 5 squares, as the mockup does.
    - Data and module:
      - G4: 5 state tabs (attention, pending, retrying, parked, sent). The mockup's
        separate Failed tab is a D4 accept entry.
      - G6(a): the test-send idempotency key is `admin-send-test-` + `request_id`, and
        `is_test_key` widens to match.
      - G7: the message id is shown in full, and "next attempt" is relative time from SQL.
      - G8: panel meta comes from `MAIL_SMTP_HOST`. The throttle banner and the
        attention item show only while a row is retrying.
      - G9: the page's `div.stack` wrapper is a template-choice accept entry.
      - G10: the Retrying count runs without a new index (R1).
      - G11: "requeue all" has no mockup element, so it is not built. The plan's REAL
        list loses it.
      - G12: the probe is restored from `b8435c9a^` with its `cp.invoke` CapCase.
      - G13: no seed tool (item 5 **Ports**, as superseded).
- **Step 12 — pilot gate in the split (c).** No files.
  - Wipe schema `admin` first (`DROP SCHEMA admin CASCADE` via `psql`, per the wipe rule),
    because `CREATE TABLE IF NOT EXISTS` does not add the Step 5 `role` column to an
    existing table. `devctl up` then migrates it fresh and seeds `admin/admin`.
  - Run the `safe-verification` pre-flight, then `devctl up split`, then log in.
  - Run `admindiff crawl --base-url http://localhost:8085`, which fetches the mail page,
    its fragments and the row menu statuses.
  - Make one real invoke (cancel a pending test message), then stop the fleet.
  - On any failure: stop, diagnose, and report to the owner with options. No fix loop
    inside this step (live-acceptance rule).
  - `[inline]` orchestration.
  - **Result (2026-11-17): passed.**
    - Schema `admin` was wiped, then `devctl up split` started all services healthy.
    - `admindiff crawl --base-url http://localhost:8085` was clean: 3 pages and 24
      fragments, with mail's panels served through admin-svc from mail-svc. `diff` was
      clean on all three pages: index 20/20, events 13/13, mail 16/16.
    - The real invoke was a **test send** through admin-svc, not a cancel. It answered
      `HX-Redirect` with a show-once reveal ("Test message queued for
      pilot@example.com"), and the row appeared under Sent. A pending cancel could not be
      exercised, because the dev `log` provider sends at once, so no row stays pending.
    - `devctl down` printed the known "os error 32", but the fleet stopped (status
      inactive, no `*-svc` left).
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
    - The returned `Primitive::ColumnCells` is merged as extra columns. This replaces how
      the point renders at HEAD: as a standalone `inline` slot (`core/admin/src/render.rs:594`)
      whose `ColumnCells` draw as a PropertyList (`render.rs:780-784`).
    - A failing column renders an error icon in its cells, not a failed table
      (`catalog.js:15`).
  - **Decisions (owner, 2026-11-17, from the checklist
    `docs/plans/2026-11-17-step13-accounts-checklist.md`, gaps G1–G15)**: the checklist's
    recommendations, except G9.
    - Contract, in part `a`:
      - G1: `DataTable.list_columns_at` (insert index) and `align` on
        `Point::EntityListColumn`; header `th > span`. A failing column draws a muted alert
        icon with a title in each cell, plus one warn banner with Retry.
      - G2: an empty contribution title draws no `.panel-head`.
      - G3: `CellKind::Muted`, `CellKind::Label` (plain badge), `Cell.more` (several
        elements in one cell); a User cell in a list draws its link inside.
      - G4: `Column.sorted`, set on Created (the real sort order).
      - G6: `DataTable.item_noun`.
      - G7: new `ActionPoint::TableRow`; an action with no icon draws no svg.
      - G8: an `implemented: false` action is not a write affordance
        (`has_write_affordance` ignores it).
      - G10: `EntityHeader.badges`, the decl icon on facts, the crumb shows the handle
        instead of the uuid.
      - G11: `Point::EntityTab` gains `columns`, so the profile tab draws `div.grid.g-2`.
      - G14: admindiff's `player` page gets a route that follows the list's first row.
    - Data and module:
      - G6: the total is an exact `count(*)` (no new index, R1), with `(created_at, id)`
        keyset paging; a code comment names the missing-index cost.
      - G8: accounts keeps `writes: false`.
      - G9: no `Forbidden` → denied mapping. Part `a` contributes no Personal data panel;
        the mockup's panel is mock-only, a `contract gap` accept entry (no PII store
        exists, so it is not a data difference).
      - G12: kind `"player"` and status `"active"` are truthful constants; staff rows are
        data differences. This supersedes D2's "the 'staff' kind is FAKE" clause. Last seen is
        derived from `refresh_tokens.issued_at`. Active sessions are one row per refresh
        family, with Client/IP a FAKE hash of `family_id` (never the token). Subject is
        masked.
      - G13: exact handle, uuid and email are REAL; the substring search is derived with a
        limit; the Kind and Status filters are inert.
      - G15: no "Open detail" row-menu entry.
      - Accept entries: G2's `players-table` class (template choice), G4's arrow on Created
        instead of Last seen (declared deviation D9), G5's "Columns: 9 of 12" control
        (contract gap), G12's mockup differences (data difference).
      - Part `a` adds `docs/todo/admin-second-pass.md` items: G5 under Accepted contract
        gaps, a `reltuples` estimate at scale (G6), the Personal data panel under Accepted
        contract gaps (G9), and one per
        `// FAKE:` marker (G12).
    - Tests: a dedicated `[opus]` test-author step right after part `a` writes the
      `EntityListColumn` fan-out engine test (the gate test listed under Step 25), instead
      of waiting for Step 25.
- **Step 14 — characters.** Characters tab, summary fact, tab count. Starts after Steps 13c
  and 13d (13c.4, 13d6); its checklist
  (`docs/plans/2026-11-17-step14-characters-checklist.md`) is re-cut against them,
  leaving only its own data decisions (G2, G3, G7, G8, G10, G11); G6's accept line is
  14a3's (moved back from 13d1, 2026-11-19).
  Contributes `EntityLabel{character}` (Step 13c Refs seam). Not a writer: no 14a2.
  - **14a1** (Na1): the Characters tab (order 20), the summary fact (30) and the tab count;
    data decisions G2, G3, G7, G8, G10, G11.
  - **14a3** (Na3): the players list column (order 10), `EntityLabel{character}` (Refs seam
    **Producers**), Purges pending at 61. Depends on 14a1. It also adds the G6 `template
    choice` accept line to `tools/admindiff/accept/players.txt` (checklist line 220), copied
    from the difference its column first produces.
  - **Owner decision before dispatch:** X7 (port template).
- **Step 15 — wallet.**
  - Economy page (`pages-platform.js:164-185`; the global ledger is FAKE per R1).
  - Wallet tab; grant/revoke (REAL). Writes the Grant/Revoke seg `contract gap` accept
    entry of ADMIN2-40 (`docs/todo/admin-second-pass.md`).
  - Embers list column (DERIV); tile embers spent (DERIV).
  - Writer.
  - Ledger player column draws accounts' `EntityLabel{player}`; a `[opus]` test-author
    step after part `a` adds the splitproof assertion of the Step 13c Refs seam (label
    answered by accounts-svc), audited by proof-auditor.
  - Parts, in order:
    - **15a1** (Na1): the Wallet tab (order 30) and its fact (50).
    - **15a2** (Na2): grant/revoke (REAL) in the row-menu group (40), `request_id` →
      `idempotency_key` (X8), the probe and CapCase rows, and the ADMIN2-40 Grant/Revoke seg
      accept entry (an Enum field, K16).
    - **15a3**: the Economy page reads, the FAKE global ledger whose player column
      draws accounts' `EntityLabel{player}` (Refs seam **Consumers**), and
      `pages.rs` mapping the economy page.
    - **15a4**: create-currency (Q1; an extra element, D6), with Na2's tests.
    - **15a5** (Na3): the Embers players list column (30), the embers-spent tile (70),
      Purges pending at 62.
    - **15t** (`[opus]` test-author, after 15a3): the splitproof assertion that the ledger
      draws a player label answered by accounts-svc (`tools/splitproof/src/main.rs`);
      proof-auditor audits.
  - **Owner decision before dispatch (15a5):** which currency is "Embers". D4 names
    `gold/gems` as the real vocabulary; the audit points at research P's list and gives no
    recommendation.
- **Step 16 — inventory.** Inventory tab; grant item disabled. One a (Na1 with Na3): the tab
  (order 40), the disabled grant item in the row-menu group (50) with its todo item, Purges
  pending at 63.
- **Step 17 — friends.** Friends tab. One a (Na1 with Na3): the tab (order 50), Purges
  pending at 64.
- **Step 18 — groups.**
  - Groups tab (DERIV via `page_mine`); promote member to admin (REAL, Q1).
  - Writer: restores the groups probe deleted in Step 1.
  - **18a1** (Na1 with Na3): the Groups tab (order 60), Purges pending at 65.
  - **18a2** (Na2): promote member to admin and the probe, restored from
    `b8435c9a^:modules/groups/src/conformance.rs` (as mail's G12), with its `cp.invoke`
    CapCase.
- **Step 19 — storage.** Cloud saves tab (`writes: false`); tile cloud saves used (DERIV).
  One a (Na1 with Na3): the tab (order 70), the row-menu group (80), the tile (40), Purges
  pending at 66.
- **Step 20 — notifications.** Inbox tab, Notify header action, Send notification row action. Writer.
  - **20a1** (Na1 with Na3): the Inbox tab (order 80), Purges pending at 67.
  - **20a2** (Na2): the Notify header action (20), the Send notification row action (row-menu
    group 60), the probe.
  - **Owner decision before dispatch (20a2):** the Notify form's fields; the audit calls it
    undesigned and gives no recommendation.
- **Step 21 — apikeys.**
  - API keys page (`pages-platform.js:31-44`); search provider `apikey`; reveal.
  - Rotate/expiry disabled.
  - The reveal hint uses the gateway's real API-key header (read `modules/gateway`), not the
    mockup's `Authorization: Bearer` text.
  - Writer.
  - **21a1** (Na1): the page reads, search provider `apikey` and its kind; rotate/expiry as
    `implemented: false` with todo items.
  - **21a2** (Na2): the key writes with the show-once reveal and its gateway-header hint, the
    probe; a create without an idempotency key is a `request_id` Leftover (template Na2).
- **Step 22 — scheduler.** Scheduler page (`pages-platform.js:74-85`); all writes disabled.
  The portal's `storage.gc` attention fake moves into the scheduler provider at the
  portal's order 50, with its todo item (X6, owner 2026-11-17). One a (Na1 with Na3); its
  files include `core/admin/src/fakes.rs` and `core/admin/src/portal_provider.rs` (the
  move).
- **Step 23 — config.**
  - Config & flags page (`pages-platform.js:88-109`).
  - Edit via `cp.invoke` with `expected_revision` = the global revision. The edit form is
    a `Primitive::Form` panel whose edit action has point
    `ActionPoint::PanelForm(<that contribution>)` (Step 8a).
  - The Revisions timeline is FAKE except for the current value and revision. Writes the
    restore-link `contract gap` accept entry of ADMIN2-39 (`docs/todo/admin-second-pass.md`).
  - Writer.
  - **23a0**: T6, moved from Step 13d: an admindiff page map entry can run a JS prelude
    before mount, paired with a portal route parameter, for a client-side default
    selection. Files: `tools/admindiff/src/{pages.rs,mock.rs}`, `core/admin/src/routes.rs`.
  - **23t** (`[opus]` test-author, after 23a0): the prelude option reaches the harness
    (`tools/admindiff/src/mock_tests.rs`, `pages_tests.rs`); proof-auditor audits.
  - **23a1** (Na1), after 23a0: the page reads, the FAKE Revisions timeline and the
    ADMIN2-39 accept entry, and the key selection through `PageDecl.params` (K20).
  - **23a2** (Na2): the PanelForm edit with `expected_revision`, the probe.
  - **Owner decision before dispatch (23a0):** research P says "per-key revisions NEW"; the
    plan's decision stays `expected_revision` = the global revision. Whether research P
    means new storage is open; if it does, that storage is its own step before 23a0.
- **Step 24 — audit.** Audit log page (`pages-platform.js:112-126`), and the dashboard
  "Recent operator activity" panel. Once every port has landed, admincheck also reports
  any peer admin-svc dials whose `<peer>-svc` hosts no provider (owner, 2026-11-10; held
  back until now because it would fire for every not-yet-ported peer). Step 24 also
  deletes every store/service item that Step 1's raze left unused and no port reused
  (`cargo clippy --workspace --all-targets -- -D warnings` must show no `dead_code`);
  until then the blocking clippy stage is red on that dead code by owner decision
  (2026-11-10). Parts (named `24.a1`… so they do not collide with Steps 24a/24b):
  - **24.a1** (Na1): the audit log page, its operator cells per X5 and the actor filter X5
    links to.
  - **24.a2** (Na3): the dashboard "Recent operator activity" `DashboardPanel`.
  - **24.a3**: the admincheck peer finding (`tools/admincheck/src/checks.rs`), after every
    port (14–23, 24.a1, 24.a2) has landed. **24.t** (`[opus]` test-author): its fixture in
    `tools/admincheck/src/checks_tests.rs`; proof-auditor audits.
  - **24.a4**: the dead-code sweep to clippy-clean, and the X9 accept-line sweep; after
    24.t, before 24.b.
  - **24.b**: the view part.
  - **Owner decision before dispatch (24.a4):** the lane. The template's is `[opus]`
    core-implementer; the audit recommends `[mechanical]` core-implementer `sonnet` after a
    code-scout inventory of the store/service items no port reused.

The Q1 extras (groups promote-admin, wallet create-currency, "Purges pending") are kept;
"Purges pending" is the X1 attention item of the port template (owner, 2026-11-17).

### Step 13c — The contract the remaining ports need (owner, 2026-11-17)

Order: after Step 28 (O1), before Step 13d and Step 14. Source:
`docs/research/2026-11-17-ports-contract-gaps.md` (items K, R, X3; anchors there). Owner:
"dopisz to wszystko do planu" — the research doc's proposals are adopted; where it lists an
alternative, the first proposal stands. Why: every remaining port hit the same contract
gaps one at a time (Step 14 checklist G1–G11, then the sweep of Steps 15–24); deciding them
per port repeated the same question eleven times. The cross-cutting rules (X1, X2, X4–X9)
live in the port template and the port bullets, not here.

Owner decisions from the plan review (2026-11-17):
- **O1 (decided: yes).** Step 28 (variant coverage observed from the render) runs before
  this step, out of numeric order, so the new variants below land on the observed gate
  instead of growing the `SEEN_ENUMS` / `NOT_SEEN` / `UNREAD_ENUMS` hand lists Step 28
  removes.
- **O2 (decided: yes).** The refs cap and the label batch are enforced once, in the
  `LocalFace` impl (`core/controlplane/src/lib.rs`, beside the `Manifest.writes`
  check), before any provider is called; providers carry no check of their own. Labels
  are covered because they ride `cp.panel` `refs`.

(a) What is touched: `core/controlplane` (contract, the `LocalFace` cap check of O2),
`core/admin` (`render/*.rs`, templates, `engine/columns.rs`, `fakes.rs`, `routes.rs`), the
public-api baseline, `tools/conformance` policy rows for new request fields,
`modules/mail/src/admin.rs` and `core/admin/src/events_provider.rs` (K18's `item_noun`
users), `modules/accounts/src/admin.rs` (moves to the shared FAKE helper; contributes
`EntityLabel{player}`, Refs seam), the mockup skill
(standing data rules), `docs/reference/event-plane-ops.md:117-122`.
What dies, in this step:
- `TimelineEntry.actor: Option<EntityRef>` and its always-a-link render path (K10);
- `DataTable.item_noun: String` as a single plural (K18);
- the fixed readonly banner text in `primitives.html.tmpl` (K12);
- `SearchHit.target: EntityRef` (K17: one destination field replaces it);
- accounts' local FAKE hash and the portal's FAKE hash `core/admin/src/fakes.rs` `hash`
  (X3; its callers move to the helper; `render.rs`'s avatar-colour hash is not FAKE data
  and is kept);
- the `events.txt` contract-gap lines and ADMIN2-18 (K1); `events.txt` entries a K4 row
  change makes stale (a kind with no `EntityHeader` no longer draws `tr.clickable`);
- the stale Purges-pending paragraph of `event-plane-ops.md`.

(b) Decisions and invariants:
- Cells and columns: K1 `CellKind::MonoMuted`; K2 `Tone::Accent`; K3 `Column.wrap`; K5
  `Cell.row`; K6 `CellKind::Toggle`; K7 `CellKind::Strong`; K8 a `Mono` cell with a link
  draws `a.mono` (template rule).
- Rows and menus: K4 a row is `clickable` only when its kind has an `EntityHeader`
  contribution, so a row menu no longer forces a link; K14 a row-menu group of a module
  whose source is down draws the "unavailable" note and disabled items (portal only); a
  per-row disabled reason stays a D7 deviation.
- Chrome and tabs: K9 `PanelBody.foot` and `PanelBody.tag`; K12
  `ContributionDecl.readonly: Option<String>` forces the readonly state with that text and
  the tab lock icon; K13 a tab whose module is down draws the error dot (portal only); K11
  `Point::EntityTabTile` drawn as a `g-tiles` row above the tab panels, and
  `Primitive::KpiTiles` for a data-driven number of tiles; K15 `Layout::Rows`.
- Timeline: K10 `TimelineEntry.title`, `.badges`, `.actor: Option<Cell>`; `Timeline.days`
  (opt-in day labels) and a date-only `when` format. A per-entry action stays a
  contract-gap accept entry with a todo item.
- Forms and reveals: K16 `Field.mono`, `Field.placeholder`, `FormSpec.title`,
  `SecretReveal.extra`; the Grant/Revoke seg is an Enum field plus a contract-gap accept
  entry.
- Search and page state: K17 a search hit has exactly one destination, an entity or a page
  (`Link`); K20 `PageDecl.params` names the page URL keys forwarded to
  `PanelQuery.params` (covered by the existing `cp.panel req.params` policy rows unless
  conformance proves otherwise), and `FilterKind::Select`.
- Cross-module: K19 `Point::EntityLabel{kind}` and K21 `controlplane::MAX_PANEL_REFS`
  (O2), specified once in the **Refs seam** block below.
- Text: K18 the noun has a singular and a plural; the portal picks the singular at 1.
- Shared FAKE helper (X3): one deterministic helper in `controlplane` (explicit-width hash
  of an entity id, no clock, no rand); accounts and the portal's `fakes.rs` move to it.
- Variant coverage of every new enum variant is Step 28's observed gate (O1): each new
  variant (`MonoMuted`, `Toggle`, `Strong`, `Tone::Accent`, `FilterKind::Select`,
  `Primitive::KpiTiles`) needs a golden that renders it, or that gate fails. `Point` and
  `Layout` are placement enums Step 28 does not observe (`mode_of`, `layout_name`), so
  `Point::EntityTabTile`, `Point::EntityLabel` and `Layout::Rows` are covered by named
  tests in (c). Field coverage is the named test list in (c).
- Standing data rules added to the skill (R-a … R-j of the research doc): identifiers in
  full; times computed in SQL, keeping the mock's per-column format; real stable order, a
  sort arrow only on the real sort column; singular at 1; a count is always a `Cell`, `0`
  included; a demo-only mockup element takes its nearest designed sibling's ok shape,
  declared in the hand-check; portal empty states, and a filter narrowed to zero keeps the
  toolbar; truthful constants beat FAKE, and an exact `count(*)` carries a comment naming a
  missing index; "—" for a never-happened value; D4 vocabulary.

**Refs seam** (K19, K21, O2; review unit (ii)). The one specification of how ids cross
modules on a rendered page; plan review 2026-11-18 found three gaps in the earlier
scattered sentences (F1, F3, F4), so every rule of this seam lives here and nowhere else.
- Unit: one slot response (one table, one panel body), not the page. Each list column
  collects the distinct `row.entity` refs of its kind from that table (one per row, as
  `columns::refs` does today); each label pass collects every ref of its kind the body
  draws, where one row may carry several (a timeline actor and target).
- Batching: the distinct refs are split into calls of at most `MAX_PANEL_REFS` (the plan's
  mechanism for K21's "the portal never exceeds it", from plan review P1, 2026-11-17). All
  column × chunk calls run concurrently; the chunks of one column share that column's one
  deadline (the decl's, as `fan_out` uses today), not one deadline each.
- Failure: any chunk that fails, times out or answers a primitive other than the expected
  one makes the whole column `ColumnOutcome::Failed`: the marker in every cell and one
  warn banner, as today. No partial column.
- Cap backstop (O2): `LocalFace` refuses `refs` longer than `MAX_PANEL_REFS` before the
  provider runs, with `opsapi::Error::invalid`: an over-cap request is malformed, not a missing entity,
  so `not_found` would name the wrong cause. The `cp.panel req.refs` conformance
  rationale names the bound.
- Labels: one label pass per slot response, after its body (and its columns) are
  built; it reads refs from the body only, never from another slot. A pass per kind calls
  the `EntityLabel{kind}` contribution of the one module whose manifest declares that
  `EntityKindDecl` (the kind's owner; a kind is declared by exactly one manifest, which
  `tools/admincheck` enforces across manifests, beside its per-manifest duplicate check
  `unique_ids` at `checks.rs:45`); an `EntityLabel{kind}` declared by any other
  module is never called. That contribution is called under its own decl's deadline, batched and
  chunked as above. A label that fails, times out, is denied (role below `min_role`, zero
  calls) or is missing from the answer draws the plain id, as `ref_cell` does today; a
  failed label pass adds no banner (the id is still a correct, linked value).
- Producers: accounts contributes `EntityLabel{player}` (the handle) in this step,
  unit (ii); characters contributes `EntityLabel{character}` in Step 14. Any later kind's
  owner adds its own with its port.
- Consumers and the split assertion: the first consumer of a cross-module label is the
  Step 15 wallet ledger (player). The `[opus]` test-author step after its part `a` adds
  the committed splitproof assertion that the ledger draws a player label answered by
  accounts-svc.

(c) Steps and their tests (revision 2026-11-18; re-cut for step size the same day). The steps
edit the same files (`core/controlplane/src/lib.rs`, `core/admin/src/render/*.rs`,
templates, the public-api baseline), so they run in this order unless a step names itself
file-disjoint. Each code step re-blesses the existing goldens its change alters and leaves
them green; the only red by design is listed in the intermediate gate: Step 28's
variant-coverage test naming the new variants from 13c.1a until 13c.1h (with the two
`Primitive` case-count checks from 13c.1b), and `FilterKind::Select` from 13c.2b2 until
13c.3a. Every code step runs the intermediate gate (`cargo test -p <touched crates>`).
- **Step 13c.1a — cells, rows, menus, text and the FAKE helper** (K1–K8, K14, K18, X3).
  `[opus]` core-implementer. Tests in the step: row clickability by kind (K4); the
  down-module row-menu group (K14); the singular noun at 1 (K18); the FAKE helper gives a
  pinned output for a fixed id, checked from the accounts and the portal call sites (X3).
- **Step 13c.1r — split `render.rs` and `golden_tests.rs` by responsibility** (owner,
  2026-11-18). Why: `core/admin/src/render.rs` (2057 lines) and `golden_tests.rs` (3194
  lines) are read whole by every later 13c step, 13c.1g and each port; an agent working on
  one area (cells, forms, queues) should read only that file. Runs after 13c.1a, before
  13c.1b. Pure move: no behaviour, template, contract or test change. Line ranges are at
  `0c380299`; a range boundary inside a cluster that the implementer finds wrong moves to
  the nearest item boundary, named in the hand-off.
  - (a) `core/admin/src/render.rs` becomes `core/admin/src/render/`:
    - `mod.rs`: the `seen` pair (17-95, both cfg variants and its unit test, unchanged and
      inline, so its `super::` calls resolve through imports of `tone`, `mode_of` and
      `emphasis_name` in `mod.rs`); templates and entry points (97-139); the `mod` lines
      and the re-exports.
    - `url.rs` 141-210; `style.rs` 211-267 plus `thousands` and `fixed1`; `cell.rs`
      268-372; `action.rs` 373-467; `shell.rs` 468-727 (view models, `ranges`,
      `layout_columns`, `layout_name`); `panel.rs` 728-986 (`mode_of`, `panel`, `prim`,
      `Ctx` as `pub(super)` with its fields `pub(super)`); `table.rs` 987-1224 (the
      helpers `queue` reaches become `pub(super)`); `tiles.rs` 1225-1398 and 1724-1920
      (kpi/spark, timeline, services, chart, attention/header); `form.rs` 1399-1583;
      `queue.rs` 1584-1723; `modal.rs` 1921-2057.
  - `core/admin/src/golden_tests.rs` becomes `core/admin/src/golden_tests/`:
    - `mod.rs`: header doc, imports, consts (1-73) and case types (2277-2310); `mod` lines.
    - `harness.rs` 75-278; `view_tests.rs` 280-531 (`table_grid` re-exported as
      `pub(crate)` for `tests.rs:921`); `build.rs` 533-1326; `cases.rs`: `cases()`
      1328-2275 moved whole, with `push!`/`panel!` inside it unchanged (its locals `in_mode`,
      `no_errors`, `ban`, `grant` cross sections, so it is not cut);
      `variant_coverage.rs` 2312-2424 (not `coverage.rs`: `.gitignore` ignores `coverage.*`);
      `macros.rs` 2426-2760; `declared.rs` 2762-2961; `selftests.rs` 2962-3194.
  - (b) Invariants:
    - Every external path stays valid: `crate::render::X` for every item that routes.rs,
      lib.rs, engine/actions.rs, events_provider.rs, tests.rs and the goldens use
      (`render::seen::{record, collect, Record, Converted}` included), and
      `crate::golden_tests::table_grid`, through re-exports in the two `mod.rs` files. A
      `pub(super)` item is never re-exported as `pub(crate)`: an item used outside
      `render` keeps `pub(crate)`.
    - Moved lines are unchanged apart from: `use` lines, a visibility prefix widened to the
      minimum the split needs, and the `include_str!` paths (`render.rs:97-100` →
      `../templates/…`; `golden_tests.rs:2348` → `../engine/panels.rs`).
    - Every case still goes through `scoped` (Step 28 per-case collect); case names and
      order are unchanged; every testdata file stays byte-identical and no bless runs.
    - No item is copied; each lives in one file.
    - Outside the two moved trees only `docs/todo` line pointers into the moved files
      change (`apikeys.md`, `admin-second-pass.md`); `docs/plans` and `docs/reviews` are
      archives and stay. `cargo fmt` is not run (hand-format).
  - (c) Tests: none new. Proof:
    - a line-multiset check (a throwaway script, its output in the hand-off): the sorted
      lines of each old file against the concatenated new files, with `use` lines removed,
      the visibility prefix and `include_str!` paths normalised, and leading whitespace
      stripped; any remaining difference is listed and justified;
    - `cargo test -p admin`: the same test count and the same leaf names with the same
      outcomes as at `0c380299` (the Step 28 gate red on exactly the four 13c.1a variants);
    - `cargo clippy -p admin --all-targets -- -D warnings` (catches a bad or unused
      re-export in both builds);
    - the reviewer's second view: `git diff <c>~1 <c> --color-moved=zebra
      --color-moved-ws=allow-indentation-change`.
  - (d) Lanes: `[mechanical]` core-implementer `sonnet` (fully specified by the map
    above), two commits in order (render, then golden_tests). `[review]` core-reviewer
    `opus`, one round on both commits; no proof-auditor (no gate behaviour changes).
- **Step 13c.1b — chrome, tabs, timeline and forms** (K9–K13, K15, K16). `[opus]`
  core-implementer. Tests in the step: the down-module tab dot (K13); a `readonly` decl
  forces the readonly state with its text (K12).
- **Step 13c.1g — goldens for the 13c.1a surface.** `[opus]` test-author, after 13c.1b.
  - Scope: a golden case for `Column.wrap`, `Cell.row`, `a.mono` and the singular noun, and
    one per new 13c.1a variant (`MonoMuted`, `Toggle`, `Strong`, `Tone::Accent`). The
    singular noun reuses the `table_noun_*` cases (`golden_tests/cases.rs:377-383`) where one
    already draws it.
  - Files: `core/admin/src/golden_tests/cases.rs`, `build.rs`, the new cases' testdata.
  - Bar: reverting the field's draw changes the golden.
  - Ends the red window for the four 13c.1a variants.
- **Step 13c.1h — goldens for the 13c.1b surface.** `[opus]` test-author, after 13c.1g
  (same files, so not parallel).
  - Scope: a golden case for `PanelBody.foot`, `PanelBody.tag`, the
    `ContributionDecl.readonly` text, the tab tiles row, `Layout::Rows`,
    `TimelineEntry.title/badges/actor`, `Timeline.days` and the date format, `Field.mono`
    and `placeholder`, `FormSpec.title`, `SecretReveal.extra`; and the `Primitive::KpiTiles`
    variant (the `kpi_tiles` case). `timeline_two_days_older` (`cases.rs:406`) already sets
    `days: true`, so add only a `days: false` case and a `date_only` case.
  - Files and bar as 13c.1g.
  - Ends the red window above, including the two `Primitive` case-count checks.
- The Refs seam block above is the one specification of K19, K21 and O2; Steps 13c.2a1–2a3
  each implement the rules they name and restate none.
- **Step 13c.2a1 — chunked column fan-out and the cap** (K21, O2; Refs seam rules **Unit**
  for columns, **Batching**, **Failure**, **Cap backstop**). `[opus]` core-implementer, after
  13c.1h.
  - Scope: `controlplane::MAX_PANEL_REFS`; `fan_out` (`core/admin/src/engine/columns.rs:36`)
    chunks each column's distinct refs; the cap check in `LocalFace::panel`
    (`core/controlplane/src/lib.rs:261`, beside the `write_refusal` check at :281).
  - Files: `core/controlplane/src/lib.rs`, `core/admin/src/engine/columns.rs`,
    `core/admin/src/tests.rs` (beside `fan_out_guarded`, :1388), the `cp.panel req.refs`
    row of `tools/conformance/src/policy.rs` (:192), the public-api baseline
    (`--bless-public-api`).
  - Tests: distinct refs per kind; more distinct refs than `MAX_PANEL_REFS` split into
    chunks that all run and still fill every cell; the chunks of one column share one
    deadline and run concurrently (paused clock: two chunks each answering at 0.6× the
    deadline fill the column and it resolves before 1.0× of the deadline has elapsed, which
    a sequential run cannot; one chunk at 1.1× fails it); one failing chunk fails the whole
    column; the cap refused in `LocalFace` with `invalid` before the provider runs (O2), on
    the monolith face.
  - Rationale: a new rationale constant naming `MAX_PANEL_REFS` replaces
    `CONTROL_PLANE_READ_RATIONALE` on the `req.refs` row (`policy.rs:192`).
- **Step 13c.2a2a — the label pass** (K19; Refs seam rules **Unit** for labels, **Labels**).
  `[opus]` core-implementer, after 13c.2a1 (reuses its chunking).
  - Scope: `Point::EntityLabel{kind}`; one label pass per slot response after its body and
    columns, reading refs from the body only; only the kind's owning manifest's contribution
    is called, under its decl's deadline, chunked as columns; a failed, timed-out, denied or
    missing label leaves the plain id with no banner.
  - Files: `core/controlplane/src/lib.rs` (`Point`), new `core/admin/src/engine/labels.rs`,
    `engine/mod.rs`, `engine/panels.rs` (the non-slot `Point` arm at :106),
    `tools/admincheck/src/checks.rs` (the `point_kind` arm, :62-75),
    `core/admin/src/tests.rs`, the public-api baseline.
  - Tests: only the kind's owner answers a label (a second module's `EntityLabel` for the
    kind gets zero calls); more distinct refs than `MAX_PANEL_REFS` still fill every label;
    a row carrying two refs of one kind; a label that fails, times out, is denied or is
    missing yields the id with no banner; labels read from the body only; the named
    placement test for `Point::EntityLabel` (Step 28 does observe `Point`, through
    `mode_of`; Step 13c.2a2a errata).
  - **Owner decision (2026-11-19; its carriers are superseded by the next one):**
    - a label answers `Primitive::ColumnCells` with one text cell per requested ref, which
      adds no `Primitive` variant; a ref missing from the answer draws its plain id;
    - the carriers that get labels are `Row.entity` (`lib.rs:1005`), the timeline
      `target` (`lib.rs:1131`) and the K10 actor `Cell` (`TimelineEntry.actor`,
      `lib.rs:1127`) where it names an entity.
  - **Owner decision (2026-11-19, after the 13c.2a2a reviews; supersedes the carriers
    above):** a label is fetched for every place that draws an entity: every `Cell` whose
    link is `Link::Entity` and whose displayed text is the raw id (tables, facts, the
    timeline actor), the timeline `target`, and the WorkQueue `QueueItem.entity` (drawn by
    `render/cell.rs` `ref_cell` via `render/queue.rs:81`). `Row.entity` is not a carrier: it
    is only the row link and the list-column key. Why: the first consumer, the Step 15
    wallet ledger, draws the player as a table cell with an entity link.
- **Step 13c.2a2b — the labelled draw** (Refs seam **Unit** and **Labels** at the draw
  sites; owner, 2026-11-19). `[opus]` core-implementer, after 13c.2a2a.
  - Scope:
    - `labels::refs` collects the carrier set of the 2026-11-19 decision above (this step
      owns that change to 13c.2a2a's `labels::refs`);
    - the slot route runs `labels::pass` once per slot response, after its body and
      columns, and passes the `Labels` into rendering;
    - the draw sites draw the label, or the plain id without one: `render/cell.rs`
      (`one_cell`, :50, for an entity-linked cell; `ref_cell`, :88, for the queue),
      `render/tiles.rs` (the timeline actor :147 and target :152) and `render/queue.rs`
      (`QueueItem.entity`, :81);
    - Step 28 placement: `Point::EntityLabel` and `Point::EntityListColumn` are recorded
      with `render::seen::record` where production draws them (the labelled draw and the
      list-column cell draw), not through `render::mode_of`; both leave the
      `slot_every_point` golden case (`golden_tests/cases.rs:1001`). Ends the
      `Point::EntityLabel` red window.
  - Files: `core/admin/src/routes.rs`, `engine/labels.rs`,
    `render/{cell.rs,tiles.rs,queue.rs,panel.rs}`, `core/admin/src/tests.rs`,
    `core/admin/src/golden_tests/` (`cases.rs` and the new draw's golden).
  - Tests: a labelled entity cell draws its label; a failed or missing label draws the id;
    a cell whose text is not the raw id gets no label call; route level: on a page with two
    slots where only one draws ref X, X's label is requested from that slot's body only; a
    failed label leaves no banner or alert in the slot HTML; the golden of the labelled
    draw.
- **Step 13c.2a3 — kind ownership and the first producer** (the admincheck cross-manifest
  kind rule; Refs seam **Producers**). `[opus]` core-implementer, after 13c.2a2b.
  - Scope: admincheck reports a kind declared by two manifests, beside `unique_ids`
    (`checks.rs:45`; the per-manifest entity-kind line is :59), an `EntityLabel{kind}`
    declared by a module that does not own the kind, and more than one `EntityLabel{kind}`
    in the owner's manifest (owner, 2026-11-19); accounts contributes `EntityLabel{player}`.
  - Files: `tools/admincheck/src/checks.rs`,
    `modules/accounts/src/{admin.rs,store.rs,lib.rs}`, new
    `modules/accounts/src/admin_tests.rs` (this step creates it; Step 25f2 extends it).
  - Tests: accounts' `EntityLabel{player}` answers the handle for each requested id and
    leaves out a missing player, so the portal draws its plain id. The admincheck fixtures
    are 13c.3a's.
- **Step 13c.2b1 — one search destination** (K17). `[opus]` core-implementer, after
  13c.2a3 (shares `lib.rs`, the baseline and accounts' `admin.rs`).
  - Scope: a `SearchHit` (`lib.rs:417`) has exactly one destination, an entity or a page
    (`Link`); `SearchHit.target: EntityRef` dies.
  - Files: `core/controlplane/src/lib.rs`, `core/admin/src/engine/search.rs`,
    `render/shell.rs` (`SearchHitView`, :240), `routes.rs`, `modules/mail/src/admin.rs`
    (:567), `modules/accounts/src/admin.rs` (:706-721), `core/admin/src/events_provider.rs`
    (:752), `core/admin/src/tests.rs` (the fakes at :973, :1368, :1631), the public-api
    baseline.
  - Tests: a search hit reaches its one destination, the entity and the page variant each.
- **Step 13c.2b2 — page params and `FilterKind::Select`** (K20). `[opus]` core-implementer,
  after 13c.2b1.
  - Scope: `PageDecl.params` (`lib.rs:541`) names the page URL keys forwarded to
    `PanelQuery.params`; the page route (`routes.rs:685`, today `params: query` forwards
    every key) forwards only those, covered by the existing `cp.panel req.params` policy
    rows unless conformance proves otherwise. `FilterKind::Select` draws in the table
    toolbar (`render/table.rs` `filter_views`, :76; `templates/primitives.html.tmpl:198`).
  - Files: `core/controlplane/src/lib.rs`, `core/admin/src/routes.rs`, `render/table.rs`,
    `core/admin/src/templates/primitives.html.tmpl`, `core/admin/src/tests.rs`, the
    public-api baseline.
  - Tests: only the `PageDecl.params` keys reach `PanelQuery.params`; an undeclared key is
    dropped.
  - Declared red window (audit recommendation; owner confirms at plan approval):
    `FilterKind::Select` is a drawn variant, so Step 28's gate is red from this step until
    13c.3a (in the intermediate gate's list). `FilterKind` is declared `undrawn` today
    (`lib.rs:1022-1024`) and the gate fails on a converted undrawn enum, so drawing it by
    kind makes it a drawn enum whose every variant needs a golden.
- **Step 13c.3a — gate tests for 13c.2a3 and 13c.2b2.** `[opus]` test-author, after
  13c.2b2:
  - the `FilterKind::Select` golden (`golden_tests/cases.rs`, `build.rs`, testdata); ends
    the Select red window; with it the `FilterKind::Range`, `Bool` and `Ref` goldens, which
    entered the same red window when 13c.2b2 made `FilterKind` drawn (Step 13c.2b2 errata,
    owner 2026-11-19);
  - admincheck fixtures (`tools/admincheck/src/checks_tests.rs`): two manifests declaring
    the same `EntityKindDecl` kind; an `EntityLabel{kind}` from a module that does not own
    the kind; two `EntityLabel{kind}` in the owner's manifest.
    Also a non-owner with two labels of one kind yields exactly one non-owner finding
    (13c.2a3 proof audit).
  - The labelled-draw golden is 13c.2a2b's (owner, 2026-11-19).
- **Step 13c.3b — the over-cap refusal in the split.** `[opus]` test-author, after 13c.2a1;
  file-disjoint from 13c.3a (`tools/splitproof/src/main.rs` only), but its fleet run never
  overlaps another test rollout.
  - Split, the at-risk topology: a committed splitproof assertion that an over-cap
    `cp.panel` is refused over the wire by the remote `LocalFace`. The cross-svc label
    assertion is Step 15's (15t; Refs seam block).
  - **Owner decision before dispatch:** how splitproof sends a raw `cp.panel` over the cap,
    which the portal never does: a direct dial of the remote svc's internal edge (splitproof
    already dials internal mTLS edges with `edge::Client::dial`, `main.rs:2508`, :2989) or an
    HTTP route able to send more refs than the cap. The audit gives no recommendation.
  - **Owner decision (2026-11-19):** the direct dial: splitproof dials the remote svc's
    internal edge with `edge::Client::dial` and sends a `cp.panel` with `MAX_PANEL_REFS + 1`
    refs; no production route is added for the test.
- **Step 13c.4 — docs.** `[docs]` docs-writer: the standing data rules in
  `.agents/skills/mockup-implementation/SKILL.md` and the Purges-pending text of
  `docs/reference/event-plane-ops.md`. Lands before Step 14, whose checklist uses the rules.

(d) Review: `[review]` core-reviewer after each code and test step, one round each; a
second round is allowed (`core/`, split wiring, goldens). proof-auditor on every step that
changes the public-api baseline, goldens, conformance rows, admincheck or splitproof:
13c.1a, 13c.1b, 13c.1g, 13c.1h, 13c.2a1, 13c.2a2a, 13c.2a2b, 13c.2a3, 13c.2b1, 13c.2b2,
13c.3a and 13c.3b. 13c.4 is prose and gets none.

### Step 13d — admindiff sees tabs, modals, menus and ok states (owner, 2026-11-17)

Order: after Step 13c, before Step 14. Source: the research doc's items T1–T8 and the
Step 14 checklist's G6.

(a) What is touched: `tools/admindiff/src` (`pages.rs`, `crawl.rs`, `mock.rs`,
`skeleton.rs`, `lib.rs`), `tools/admindiff/accept/`, the skill §1, §6 and §7. What dies:
the crawl's status-only row-menu check (T3 replaces it with a diffed menu body).

(b) Invariants:
- T1: a page map entry can name a tab of a `FirstRow` page; the crawl follows the first row
  with `?tab=<id>`, the harness renders the mockup with the same tab, and the pair has its
  own accept file. Each of Steps 14–20 adds its tab entry.
- T2: a page map entry can name a modal: the harness runs the mockup's JS opener and
  captures the modal; the crawl GETs the action's modal URL; the pair has its own accept
  section.
- T3: the crawl writes row-menu fragments, and the harness renders the matching mockup menu;
  menus are diffed.
- T4: a page map entry can force the mockup's ok state (`UI.state.force`), so a module the
  mockup draws only down or stale (wallet) is compared in its ok shape; hard-coded demo
  markup the force cannot reach is a declared deviation, and golden cases pin each ok shape.
- T5: `select > option` collapses like `tbody > tr`.
- T6 (moved to Step 23a0, revision 2026-11-18 step size, audit recommendation, owner to
  confirm: config is its only user and it
  touches `core/admin`): a page map entry can run a JS prelude before mount, paired with a
  portal route parameter, for a client-side default selection (config).
- G6 of the Step 14 checklist: the harness never runs `meta.after`, so a contributed
  list-column header diffs as `th.num` against the portal's `th.num > span`. That line is a
  `template choice` accept entry naming the harness, shared by every list-column port, and
  the skill §1 sentence says the diff reports it.
- A clean crawl still requires every declared target to be reached: an unreachable tab,
  modal or menu target fails the crawl, as an empty `FirstRow` does.
- T7 is Step 13c's golden work; T8 stays the hand-check section of each checklist.

(c) Steps (revision 2026-11-18, step size). 13d1–13d4 share `crawl.rs` and `mock.rs`, so
they run in order; each runs `cargo test -p admindiff`. Review: `[review]` core-reviewer plus
proof-auditor (admindiff is a gate) on each of 13d1–13d5, one round, a second allowed for the
gate.
- **Step 13d1 — tab targets and reachability** (T1, T5, the reachability invariant).
  `[opus]` core-implementer, after 13c.4. Files: `tools/admindiff/src/{pages.rs,crawl.rs,
  mock.rs,lib.rs,skeleton.rs}`.
  - **Erratum (2026-11-19, review of bbe6840b):** the G6 accept line moved to Step 14a3: an
    accept entry that matches no difference fails the diff as stale (`accept::judge`), and no
    module draws a list column before 14a3.
  - **Review fix (owner, 2026-11-19):** `crawl::run` takes the page map as a parameter, as
    `validate_lists` does, so 13d5 tests a tab target without editing `PAGE_MAP`; and
    every tab the mockup draws on a `FirstRow` page is a `PAGE_MAP` tab entry or an
    excluded tab with a reason, derived from the mockup render, as pages are against
    `EXCLUDED`. The not-ported tabs' prose in `accept/player.txt` moves to those reasons.
- **Step 13d2 — menus diffed** (T3). `[opus]` core-implementer, after 13d1. The diffed menu
  body replaces the status-only check at `crawl.rs:210-223`. Files: `crawl.rs`, `mock.rs`,
  `lib.rs`.
  - **Review fix (owner, 2026-11-19, review of 05489704):**
    - Portal side: every `/admin/_menu/<kind>/` a crawled untabbed host draws is a `PAGE_MAP`
      menu entry for that host or an `EXCLUDED_MENUS` entry with a reason; an excluded kind is
      still fetched once and a non-2xx answer fails the crawl; an exclusion no host draws is
      stale and fails. This replaces the coverage the deleted status-only check gave.
    - Mockup side: the menu inventory comes from the mockup's own registry (the harness records
      every `UI.defineMenu` id), not from matching `UI.openMenu(event,'…')` text; each id is a
      menu entry, drawn only on pending or excluded pages, or excluded with a reason.
    - Menus are collected on untabbed hosts only, so a tab entry needs no menu entry of its own.
    - With `--allow-empty`, a menu whose host list is empty is reported EMPTY like the page;
      the crawl test fixture draws the row-menu button inside the table rows, as the portal does.
- **Step 13d2b — menu state marker.** `[opus]` core-implementer, after the 13d2 review fix,
  before the first live crawl that writes a menu accept file. A menu group the portal draws
  for an unavailable module carries a machine-readable state marker in the `core/admin` menu
  template (as panels carry `data-state`), with a golden or route test; `crawl_menu` fails
  on it unless `--allow-errors`, as an error-state page fragment does. Files: the
  `core/admin` menu template and its test, `tools/admindiff/src/crawl.rs` and its test.
  Review: core-reviewer plus proof-auditor.
- **Step 13d2c — whole page and menu state.** `[opus]` core-implementer, after 13d2b, before
  the first live crawl that writes an accept file. Found in the 13d2b review (owner,
  2026-11-19): a module that never answered, or answered `Ok([])`, is absent from
  `snapshot.modules` and draws no menu group and no panel. The menu and the page then answer
  200 with only the healthy parts, and the crawl passes them, so an accept file can be written
  from an incomplete fleet.
  - `core/admin` marks the root of a page and of a row menu with a machine-readable state
    (`data-state`, as panels and menu groups carry): not ok when any module the snapshot
    expects is absent or unhealthy and has no last good manifest. The expected set comes from
    the one source the portal already uses for its module list, not a second list.
  - `crawl_page` and `crawl_menu` fail on a not-ok root unless `--allow-errors`, and fail
    on a missing root state always (as `fetch_menu` does for a group). The panel-fragment gate
    in `crawl.rs` takes the same closed set (`ok`, and `error` with `--allow-errors`); today any
    value other than missing or `error` passes it (13d2b review-fix leftover).
  - Tests: a golden or route test for an absent module on a page and in a menu; admindiff crawl
    tests read those goldens with `include_str!`.
  - Until this step lands, a live crawl that writes accept files runs only on a fully healthy
    fleet.
  - Files: the `core/admin` shell and menu templates and the snapshot code that feeds them,
    their tests, `tools/admindiff/src/crawl.rs` and its test. Review: core-reviewer plus
    proof-auditor.
- **Step 13d3 — modals** (T2). `[opus]` core-implementer, after 13d2. Files: `pages.rs`,
  `crawl.rs`, `mock.rs`, `accept.rs` (the modal accept sections).
  - **Owner decision before dispatch:** how the harness runs a mockup JS opener: the
    `innerHTML` setter throws a sentinel right after render, so `after` hooks and openers
    never run (`mock.rs:22-23`). The audit gives no recommendation.
- **Step 13d4 — forced ok state** (T4). `[opus]` core-implementer, after 13d3. Files:
  `pages.rs`, `mock.rs`.
- **Step 13d5 — admindiff tests.** `[opus]` test-author, after 13d4, on the fake-server crawl
  tests and the harness (`tools/admindiff/src/{crawl_tests.rs,mock_tests.rs,skeleton_tests.rs,
  pages_tests.rs}`): a tab target fetches `?tab=`, a modal and a menu target are written and
  diffed, an unreachable target fails the crawl, `option` collapse, and the force option
  reaches the harness (the prelude's test is Step 23t).
- **Step 13d6 — skill.** `[docs]` docs-writer: `.agents/skills/mockup-implementation/SKILL.md`
  §1 (with G6's sentence), §6 and §7; prose, no review. After 13d4; file-disjoint from 13d5.
  It settles the header-shape sentence of §1 (today: hand-check) against G6 (the diff reports
  it, from Step 14a3 on).

### Step 24a — Typed `asyncevents::admin` errors

(a) `core/asyncevents/src/admin.rs`, `core/admin/src/events_provider.rs`, `tools/eventctl/src/main.rs`.

(b) Why: owner, 2026-11-17, deferred from Step 9 to the end of this plan. The verbs return
untyped `anyhow`, so `core.events` pre-reads every subscription (`state_of`) only to
answer NotFound. Only `Retired` is typed today.

(c) Decisions and invariants:
- The verbs return a typed error with `NotFound`, `Retired`, `Healthy`, `NothingToSkip`
  and `Db`. `Retired` folds into it.
- `core.events` maps it one to one: NotFound → NotFound; Retired, Healthy and
  NothingToSkip → Invalid; Db → Internal.
- The `state_of` pre-read is deleted.
- `eventctl` prints the same messages as today.
- `SkipResult::AlreadyResolved` (Step 9) stays a result, not an error.

Tests, written in this step by its implementer: each variant reaches the provider as its
mapped status; an unknown id is
NotFound with no pre-read.

(d) `[opus]` core-implementer.

### Step 24b — `demoseed` seeds every module (owner, 2026-11-17)

`demoseed` becomes the one seeder. Through the gateway's public API only, as today, it
creates every kind of data each module's API can create; that is how real data arrives.
Per-module seed tools are not built. Afterwards the `data difference` accept entries the
seed resolves are removed.

A designed state that no public API can produce stays a `data difference` accept entry:
a mail that retries or parks, or a paused subscription. The portal does not have to
show what never happens on a dev fleet (owner: "panel admina nie musi pokazywać
dosłownie wszystkiego"). Remaining mockup gaps go to the next iteration, not into this
plan. `eventctl seed-mockup` stays as the Event plane's dev fixture.

Steps (revision 2026-11-18, step size). 24b1–24b3 share `tools/demoseed/src/lib.rs`, so they
run in order; each writes its tests in `tools/demoseed/src/tests.rs` and gets one review
round.
- **Step 24b0 — seed inventory.** `[opus]` code-scout, read-only: a table, per module, of the
  data its public API can create and the `data difference` accept entries each would
  resolve.
  - **Owner decision before dispatch of 24b1:** the owner approves the 24b0 table; it
    decides what each module seeds.
- **Step 24b1 — seed accounts, characters, inventory, wallet.** `[opus]` core-implementer.
- **Step 24b2 — seed friends, groups, party, notifications.** `[opus]` core-implementer,
  after 24b1.
- **Step 24b3 — seed storage, apikeys, config, mail.** `[opus]` core-implementer, after
  24b2.
- **Step 24b4 — resolve the accept entries.** `[inline]`, after 24b3: under
  `safe-verification`, a fleet `admindiff crawl` and `diff`, then delete the `data
  difference` entries in `tools/admindiff/accept/` the seed resolved.

### Step 25 — Tests

**Re-cut (revision 2026-11-18; step size the same day).** The lists below stay the
specification; they run as these ordered steps, each one dispatch with one review round (a
second only for `core/` or a gate). Steps 25a–25g depend on no port after Step 13 except the
admincheck finding Step 24 (24.a3) adds, so the owner may move them earlier. A backfill test
that exposes a bug is fixed at its authority in the same step, or listed under **Leftovers
for owner**. Each step runs its own crates' tests, one rollout at a time.
- **25a — Step 8a fixes.** `[opus]` core-implementer: the test migrate race and the
  undecodable claimed row (release before the 503), each with a test that fails before the
  fix. Files: `core/admin/src/tests.rs` and the claim path in `core/admin/src`.
- **25b — back/forward cache.** `[opus]` mockup-implementer: the `admin.js` `pageshow`
  re-enable. Checked by hand in Step 27 (c) 4 (no harness runs the portal's `admin.js`).
- **25c — contract and plane backfill.** `[opus]` core-implementer. 25c1–25c3 are
  file-disjoint and may be authored in parallel; their test runs are serialized.
  - **25c1:** `core/controlplane/src/tests.rs`: routing, sections, the `writes: false`
    Forbidden, and the Step 8a encode/decode table (the `core/controlplane` items below).
  - **25c2:** `core/controlplane-rpc/src/tests.rs` (loopback round trips, including Step 9a's
    `PanelBody` and `prepare`; no replay after a connection-fatal error) and the `core/app`
    `cp.manifests` test.
  - **25c3:** `core/asyncevents/src/admin_tests.rs`, run alone with `--test-threads=1`.
- **25d — portal form and claim suite.** `[opus]` core-implementer: the `core/admin` tests
  for Step 8a, all in `core/admin/src/tests.rs`, so 25d1–25d4 run in order. The list's
  time-window method applies to each.
  - **25d1:** the `classify_form` / `classify_click` unit tables.
  - **25d2:** claim and click-claim concurrency and windows: the two-POST claim, the late
    release, the token-less click claim (`generation`, `request_id`), a `Retry` that fails
    again, a computed `click:` id posted as `_form`, the classify 400s, and
    `InvokeCall.item` / `expected_revision` / `request_id` taken from the row.
  - **25d3:** settle, the 422/409 re-renders (including: a 422 re-render keeps every
    ticked Multi box; a Multi default `["a,b"]` renders one box), the toasts and
    `HX-Retarget`, the pre-invoke releases, and the modal and panel forms rendered by the
    portal posting their own inputs back.
  - **25d4:** POST decoding (Money, Multi, blanks, the blank-rule order, the unknown or
    repeated key), `validate_item`, the token-less 400s, the `__` key and mint failures, every
    "renders an error panel / renders readonly and mints no row" item, the `Then::Link` /
    `Then::Invoke` modal GET, the search cache key, and the `NoOp` flash.
- **25e — portal engine, auth and tools backfill.** `[opus]` core-implementer.
  - **Owner decision before dispatch of 25e2:** lane `[sonnet]` (audit recommendation) or
    `[opus]`.
  - **25e1:** the `core/admin/src/engine/*_tests.rs` list; the engine items of the "Tests"
    lists of Steps 9a, 9b and 10a (prepare, `range`, KPI tile stale/error, `when_empty`,
    `writes: false`, `ServiceStatus`, `Delta.tone`).
  - **25e2:** the auth suite's role (`core/admin/src/tests.rs`, so after 25d4);
    `tools/adminctl`; `weles/master/src/fleet_toml_tests.rs`.
- **25f — mail and accounts provider tests.** `[opus]` core-implementer. These ports landed
  before the revision; every later port writes its own in part `a`. 25f1 and 25f2 are
  file-disjoint.
  - **25f1:** extends `modules/mail/src/admin_tests.rs`.
  - **25f2:** extends `modules/accounts/src/admin_tests.rs` (created by Step 13c.2a3),
    including `players_page`, search, count and `live_families`.
- **25g — gate tests.** `[opus]` test-author; proof-auditor audits. 25g1 and 25g2 are
  file-disjoint.
  - **25g1:** `tools/conformance`: the input-inventory and writer-check tests.
  - **25g2:** every admincheck fixture listed here and in Steps 9a, 9b and 10a, plus one
    failing fixture per finding kind and the valid manifest with zero findings.
- **25h — splitproof.** `[opus]` test-author: the **Splitproof** list below, one fleet run
  per step, in order (`tools/splitproof/src/main.rs`; PY7 in `party_checks.rs`).
  proof-auditor audits; split is the at-risk topology for every engine assertion. 25h4 ends
  the red of the intermediate gate.
  - Before dispatch of 25h1, the plan names, per rewritten group, what it asserts after the
    redesign (research gap from the step-size audit; for example, ML4b drives
    `requeue-all-parked`, which Step 11's G11 dropped); the owner approves those lines.
  - **25h1 — portal core:** AD0–AD6g, ADX1–ADX3b, AU3, `admin_csrf`, the kept login,
    lockout, rate limit, CSRF, reveal and audit-trail assertions, and the killed-provider and
    viewer-denied additions.
  - **25h2 — monolith parity:** M3/M3b, WL6m, NT4m, ML3–ML4b, `mail_form`, `wallet_form`
    (its first users, `main.rs:427`, :442, :514; 25h3's groups reuse it).
  - **25h3 — cross-process money and inbox:** WL3–WL6, NT2/NT2b/NT2c, NT4, PH3, PH6, PH11,
    PY7 (`party_checks.rs`), `inbox_form`.
  - **25h4 — remaining groups and writers:** FR8, GR5, the config CAS edit, one real invoke
    per writing provider.

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
- `core/admin` fixes found while landing Step 8a (owner, 2026-11-10: fix here):
  - test migrate race: `core/admin/src/tests.rs` ~:643 and ~:656 call `admin.migrate`
    outside `ensure_schema`'s once-cell, so on a freshly wiped schema two tests can run
    `CREATE TABLE IF NOT EXISTS` for `admin.forms` at once and fail on a duplicate
    `pg_type` key (passes on rerun). Route them through the once-cell;
  - back/forward cache: after an `HX-Redirect`, htmx keeps `hx-disabled-elt` buttons
    disabled, and a page restored from the browser's back/forward cache shows them
    disabled until reload. `admin.js` re-enables `[data-disabled-by-htmx]` on a
    `pageshow` event with `persisted` (mockup-implementer lane);
  - a claimed row whose stored `spec` fails to decode answers 503 and stays held 30 s;
    release it before answering.
- Gate tests found by the Step 9a proof audit (2026-11-17); their firing branches have
  never run:
  - conformance input inventory:
    - a unit-only enum field (and an `Option` of one) is exactly one key;
    - the data-carrying rejection asserts the `data-carrying` message;
    - shapes covered: data variant last, struct variant, `#[cfg]` tuple variant;
    - a foreign `other::Kind` path that shares a local enum's name is not resolved
      through the local index;
  - admincheck (`tools/admincheck/src/checks_tests.rs` has no fixture for these branches):
    fixture manifests with one finding each:
    - `prepare` on `Then::Invoke`;
    - `prepare` on `Then::Link`;
    - `prepare` on a `PanelForm` action;
    - a `tiles` id whose contribution is a `DashboardTile`;
    - an id in both `tiles` and `layout`;

    plus one valid manifest that gives zero findings.
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
  - token-less `FieldErrors` gives a 422 toast and `Conflict` a 409 toast, with no
    modal; the click row is released or consumed as the settle table says;
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
- `core/admin` engine test for the Step 13 `EntityListColumn` fan-out: landed in Step 13
  (`0dd57f9b`); its per-chunk form is Step 13c.2a1's.
- `core/admin/src/engine/*_tests.rs`:
  - deadline → error; deadline with last-good → stale;
  - role below `min_role` → denied with zero provider calls;
  - `NotFound` → empty; `writes: false` with a write affordance → readonly, and without
    one → ok (Step 10a);
  - `implemented: false` POST → 403 with zero calls;
  - `Conflict` → 409;
  - manifest refresh keeps last-good when a source fails;
  - `ADMIN_OPEN` invoke carries `Local Admin`.
- `core/admin/src/tests.rs` auth suite: the user-insert helper (old `:1118`) gains a role.
  The login, lockout, rate limit, CSRF and reveal assertions are kept.
- `tools/adminctl/src/lib_tests.rs`: `--role` on insert; a conflict keeps the role; `set-role`.
- `weles/master/src/fleet_toml_tests.rs:883-886`: expected args include `--role admin`.
- `tools/admincheck` tests: every finding kind, each with a failing fixture.
- `admin_tests.rs` for mail and accounts (25f), covering their panels' data mapping and
  each REAL action's success and failure path. Every later port writes its own in part `a`.
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
  | PY7 | `party_checks.rs:282-336` |
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

(b) Why now: Step 25h after Steps 1–24; see the re-cut above for 25a–25g. (c) Split is
the at-risk topology for every engine assertion.
(d) Lanes: per the re-cut above. The per-module `[sonnet]` test-author tier of Step 10b
item 4 is superseded (revision 2026-11-18).

### Step 26 — Docs

(a) What is touched (anchors re-read 2026-11-18):
- `.agents/shared/gamebackend.md`:
  - `:62` (`adminapi::SLOT` becomes `controlplane::PROVIDER_SLOT`);
  - constraint 1 core list `:74-79` (+ controlplane, controlplane-rpc, admin);
  - constraint 10 `:158-165` (admin is core, not a module);
  - every other `admin` line the docs-writer finds in that file;
- `.agents/skills/add-game-module/SKILL.md` (`:31`, `:46`, `:64-65`: `adminapi::SLOT` and
  `adminrpc` become `controlplane::PROVIDER_SLOT`) and
  `.agents/skills/mockup-implementation/SKILL.md`;
- `.claude/rules/domain-modules.md` `:49-66` (admin as a module: GameOps portal,
  `admin.adminData`, `adminrpc::admin_remote_factory`) and `:94-96` (apikeys via
  `adminapi::AdminSubmit`);
- `docs/reference/event-plane-ops.md` (admin prose Step 13c.4 did not cover) and
  `docs/reference/module-reference.md`;
- `docs/roadmap/feature-tracker.md:227-233` (admin portal, RBAC, redesign, event-plane
  surface, and the mockup-driven rows);
- `docs/todo/admin.md`: ADMIN-01, 04, 05 and 06 are made obsolete by the redesign, so move
  them to `admin-done.md` with this plan's slug;
- dead-crate prose: `api/wallet/api/Cargo.toml:18-22`, `api/notifications/api/Cargo.toml:16-18`,
  `api/party/rpc/Cargo.toml:17`, `api/groups/events/src/lib.rs:40`, `core/opsapi/src/lib.rs:433`,
  `core/remote/src/lib.rs:1649-1650`, `Cargo.toml:58`, the admin prose in
  `cmd/{scheduler,audit,wallet,notifications}-svc` (Cargo.toml and lib.rs),
  `modules/apikeys/src/{store.rs,conformance.rs}` and `tools/opscatalog/src/lib.rs:20`;
  the line numbers are from 2026-11-10, so the docs-writer finds each by its text;
- errata in `docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md` (D1, and §8
  superseded by revision 2);
- memory: `memory/admin-extension-points-shipped.md`, `memory/generic-remote-admin-write-seam.md`
  and the `MEMORY.md` index, then the `memory-backup` skill's push.

Dropped (2026-11-18): the gamebackend.md recipe, Commands and Layout anchors (deleted by
the agent-workflow simplification; the module list moved to `.claude/rules/domain-modules.md`,
named above); the
`.grok` agent file (deleted); `.claude/agents/mockup-implementer.md` (already names
`core/admin`); the `ref_reminder.py` hook path (the mockup reminder is now
`.claude/rules/admin-ui.md`, whose `paths:` already name `core/admin`).

(b) Last, against landed code. (d) `[docs]` docs-writer (`model: "sonnet"`); prose, no
review.

(e) Steps (revision 2026-11-18, step size). 26a–26c are file-disjoint and may run in
parallel; 26d goes last. 26a and 26b never run at the same time as Step 7 of the
purge-race-locks plan.
- **Step 26a — agent rules:** `.agents/shared/gamebackend.md`, the two skills and
  `.claude/rules/domain-modules.md` items of (a).
- **Step 26b — references and trackers:** `docs/reference/event-plane-ops.md`,
  `docs/reference/module-reference.md`, `docs/roadmap/feature-tracker.md`, the
  `docs/todo/admin.md` moves and the research-doc errata of (a).
- **Step 26c — dead-crate prose:** the dead-crate prose list of (a), in `api/`, `core/`,
  `cmd/`, `modules/`, `tools/` and the root `Cargo.toml`.
- **Step 26d — memory:** the memory files and `MEMORY.md` index of (a), then the
  `memory-backup` skill's push.


### Step 27 — Verification and live render comparison

(a) No files. (b) Terminal gate.

(c) Per `safe-verification`:
1. Run one `verifyctl` manifest (`--all --strict`).
2. Run `devctl up monolith`, then `admindiff crawl`, then `admindiff diff` for every
   mockup page against its accept file.
3. Do the same through the split fleet (`--base-url http://localhost:8085`).
4. By hand in a browser (Step 25b): after an `HX-Redirect`, go back; the page restored
   from the back/forward cache shows no button left disabled.

The declared deviations are the accept files `tools/admindiff/accept/<page>.txt`; the
report cites them.

(d) `[inline]` orchestration; report per the `mockup-implementation` skill's closing step.
Once every item is green, `memory/admin-refactor-red-gates-expected.md` is retired (file,
`MEMORY.md` line, `memory-backup` push).

### Step 28 — Enum-variant coverage observed from the render, not mirrored (owner, 2026-11-17)

Order (owner, 2026-11-17, O1 of Step 13c): runs after Step 13 and before Step 13c, out of
numeric order.

Why: the golden gate's `SEEN_ENUMS` / `NOT_SEEN` partition and the `panel_variants`
walker in `core/admin/src/golden_tests.rs` (Step 13 commits 9ce5eb84, 97df9b4c) restate
the template's drawing rules in test code: when a head or footer draws tools, that a
disabled action draws no `then`, that `ColumnCells` renders `Empty`, that danger overrides
emphasis. That second copy drifts from the templates, and each of four audit rounds found
a new spot where it did (leftovers L1–L5 and the unguarded `Point` variants in the
Step 13 post-mortem). Patching the copy grows it. This step replaces it.

(a) What is touched: `core/controlplane` (each drawn enum derives its variant list),
`core/admin/src/render.rs` (a test-build-only record of each variant converted into a
view model), `core/admin/src/golden_tests.rs` (the walker, `SEEN_ENUMS`, `NOT_SEEN`,
`UNREAD_ENUMS`, `VIEW_SOURCES` and their tests are removed), the public-api baseline.

(b) Invariants:
- The set of variants to cover comes from the enum type itself, never from a hand list
  or a source parse.
- The set of variants seen comes from what the render path actually converted while
  rendering the golden cases, never from the case's model JSON or a restatement of
  template rules.
- A new variant with no golden that renders it fails the gate; a variant recorded only
  outside the golden run does not count.
- The record exists only in test builds; production rendering is unchanged.
- An enum the render path never converts (placement, role, outcome) needs no exception
  list: it is simply not in the observed set, and the gate covers only enums the render
  path converts.

(c) Tests: a new variant added to a covered enum in a scratch build fails the gate with
its name; deleting the golden case that is the only holder of a variant fails the gate;
the existing golden renders are unchanged.

(d) Lanes: `[opus]` core-implementer for the derive and the render record; `[opus]`
test-author for the gate and its tests, after the implementation lands, deleting the old
walker and hand lists in the same commit that adds the gate, so no window has no gate; `[review]`
core-reviewer plus proof-auditor (the gate is the risk surface). One review pass each,
no fix-on-fix: a finding that needs the mirror back is an owner question.

## Owner answers

- **Q1 (answered: keep).** Groups "promote member to admin" (Step 18), wallet "create
  currency" (Step 15) and the "Purges pending" KPI (`docs/reference/event-plane-ops.md:125-130`,
  in each of the 7 modules' port steps) are kept as extra elements built from existing
  primitives in the mockup's style (D6). **Amended (owner, 2026-11-17, X1):** "Purges
  pending" is a conditional `DashboardAttention` item per purging module, not a KPI tile.
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
- **D9.** The players list's sort indicator sits on the real sort column, Created, not on
  the mockup's Last seen (Step 13, G4).

## Open items for implementers (do not change scope)

- HTTP/2 and SSE are unverified; nothing here needs them.
- The manifest refresh dials all 13 peers when admin-svc starts. A down peer is retried
  every 5 s and logged at debug, as the gateway does.
- `include_str!` into `UILayout/design-system/` couples the admin build to that directory.
  No packaging context excludes it today.
- The routecheck SERVE-PARITY treatment of `cp.*` was read only at
  `tools/routecheck/src/main.rs:215-240, 460-490`. Step 4's implementer confirms it.

## Errata (during implementation)

- **Revision 2026-11-18 (workflow).** Supersedes for unlanded steps: Step 10b item 4
  **Test tier** (per-module `[sonnet]` test-author), the port template's "no test files",
  the Step 13c single test-author step, and Step 25's single dispatch. The scope,
  invariants and owner decisions are unchanged; see the revision note under the title.

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
- Step 8a: the Step 8 provisional conventions above (the `q`/`cursor`/`before`/`state` params, comma-joined Multi, `<key>_currency`, the `icon == "plus"` primary) are superseded by Step 8a. The form-spec rules admincheck and the portal share (field keys without `__` or a leading `_`, not equal to a `hidden` key, every `default` decodable) are one public function, `controlplane::form_spec_errors`, so the rule is not written twice (owner confirmed it stays public, 2026-11-17).
- Step 9:
  - `SkipResult::Skipped` holds `Box<SkipOutcome>` (clippy `large_enum_variant`).
  - The poison preview's `PropertyList.item` is `<subscription_id>#<event_id>`, not the bare event id: one event can block several subscriptions of its topic, and Resume needs the subscription. `core.events` splits it at the last `#`.
  - `asyncevents::admin::states` is the "new reader" of the data-sources table (paused tile, nav counter, Subscriptions KPI); the panels that show lag use `info`.
  - The Event plane page is one `Layout::Stack` with the KPI tiles first, because `Layout` has no tiles-then-panels variant.
  - The `cp.invoke` `req.input.<value>` row is Validated at `admin::conformance::CORE_EVENTS_INPUT_VALUE_BYTES` (4096). `core.events` also refuses a skip reason over 1024 bytes as a `FieldErrors` on `_reason`, because the reason is stored in `last_error`.
  - The `storage.gc` attention fake is in the `portal` provider; no port step owns it.
  - Retirement is permanent (owner, 2026-11-17): `asyncevents::admin` retry, pause, resume, resume_retry and skip refuse a retired subscription inside the write (`state <> 'retired'` in the UPDATE; skip on its locked row) with the typed `asyncevents::admin::Retired` error, which `core.events` maps to `Invalid` and `eventctl` reports as a refusal. The provider's separate retired pre-read is gone, and `eventctl resume` no longer un-retires; no un-retire path exists.
- Step 9a:
  - The conformance input inventory (`tools/conformance/src/input_inventory.rs`) rejected every enum in a request, so `PanelQuery.range: Option<TimeRange>` could not get its Opaque row. An enum whose variants are all unit is now one key (on the wire it is one variant name); an enum with a data-carrying variant is still rejected. The existing rejection test now uses a data-carrying enum.
  - The `range` query values (`1h`, `24h`, `7d`) and their parsing are portal-private (`core/admin/src/engine/panels.rs`); modules see only `TimeRange`. An unknown `range` on `GET /admin` is a 400 error page (a toast for htmx); on a dashboard panel it is a 400 toast. A non-dashboard panel request drops a `range` key and carries `None`.
  - The poison preview shows only a paused subscription; an active one that is retrying no longer appears there. Its declared title is the static `Paused:` the heading follows (the mockup's "Paused: `<id>`"), which would be false for a retrying one. Nothing paused is the empty state "No subscription is paused."
  - The poison banner is `Err` "Consumer failed N× on the same event and stopped to preserve ordering. M events are waiting behind it." when the subscription has failures, and `Warn` "Delivery is paused. M events are waiting behind it." when it was paused with none.
  - The event is written `<topic> #<short id>` in the poison Event row and in the skip confirm's Event impact; the contract version is no longer shown.
  - Skip's `prepare` reads the subscription's topic with `asyncevents::admin::info`, the only reader that returns it. The topic is fixed per subscription, so this names the event the operator saw. The declared skip `ConfirmSpec` keeps only its variant, title, body, tone, reason and label.
  - Service health's `meta` is "<running>/<total> healthy · probed every 10 s" (the probe interval is FAKE) and its `note` is the fake history's window, "last 4 h".
  - Review fix: a panel form's `admin.forms` row stores its panel's chrome. `StoredSpec::Form` is `{ form, chrome }` (meta, heading, note, banner; empty for a modal form), and the 422/409 re-render rebuilds the whole `PanelBody` from it with no re-fetch. Rows written before this change no longer decode; they expire within the 1 h TTL (wipe rule).
  - Review fix: the poison `meta` is "since <Mon D> <HH:MM>" (for example "since Nov 17 14:02"), from the paused row's timestamp in the database session's time zone. The provider reads no clock, so the date is always shown, unlike the mockup's "since 14:02".
  - Review fix: the input inventory resolves a request type through its own type index only for a one-segment path or a `crate::`, `self::` or `super::` path. A foreign multi-segment path such as `other::Kind` goes to the opaque-type check or the error, so it no longer resolves to a local type of the same name.
- Step 9b:
  - The error tile's last value is the expired copy's `KpiTile.value` followed by its `unit` when it has one ("13.29 TB"). The mockup's error tile (`bundle.js:180`) shows the value alone; its one example has no unit.
  - A bar cell renders with an empty `CellView.tone`, so the existing `cell` macro already shows the value plain; the fill class is `CellView.bar_tone` (`""`, `w` or `e`) and the clamped width is `CellView.bar`.
  - `SparkView.color` comes from `KpiTile.spark`, and `primitives.html.tmpl` already reads it, so the spark colour needs no Step 10 markup.
  - The Lag bar is `(lag / 3).clamp(3, 100)` in whole percent, because `Cell.bar` is a `u8`; the mockup computes it in floating point.
  - Review fix: `admin.forms` rows written before Step 9b with `StoredSpec::Confirm` hold the string `impact` and no longer decode; they expire within `FORM_TTL` (1 h) under the wipe rule, as the Step 9a errata says for Form rows.
- Step 10a:
  - The `when_empty` filter is applied where the panel view model binds its tools (`render::panel`); the template shows whatever `p.tools` holds in state empty.
  - The write-affordance rule is `has_write_affordance` in `core/admin/src/engine/panels.rs`, called by `resolve` on a fresh `ok` body only. A stale body keeps state `stale`, as before.
  - A `ServiceItem` with no `value` renders an empty value cell. The dashboard's down `wallet-svc` sets `"—"` itself (`data.js:97`), which the renderer used to supply for a missing `p99_ms`.
  - `fakes::delta` takes `tone: Option<Tone>`. The tiles whose delta has no coloured value (Lore seats, Matches in progress, Subscriptions, Head sequence, Dead-lettered) set `None`; "— steady" is `flat`, as in the mockup.
- Step 11 (part a):
  - The shared `cp.invoke` input cap is the contract const `controlplane::MAX_INPUT_VALUE_BYTES` (4096); `core.events` and `mail` both apply it, so the cap is not written twice.
  - G5's To link is `Link::Page { mail, outbox, {q: <recipient>, state: <row state>} }`. The page route passes the page URL's `q` and `state` to its layout panels, the keys named by the contract consts `controlplane::PAGE_PARAM_SEARCH` and `PAGE_PARAM_QUEUE_STATE`; before this nothing read a `Link::Page` param.
  - `WorkQueue` also gets `item_noun`, the foot's plural ("7 messages"). The view models gain `page_label` ("1 / N", first page only; a cursor does not say how far in it is) on both `TableView` and `QueueView`.
  - A `PanelTool` on a WorkQueue draws in the panel tools; only `QueueRow` actions draw on items (`ActionItem.queue_row`). `has_write_affordance` counts `QueueRow` too.
  - The outbox pages by 25 rows (the mockup's selected "25 / page") with a keyset cursor `<created_at µs>.<id>`, not the checklist's 50. The Template filter is `FilterKind::Text`, honoured from `params.template`; the chip itself stays inert.
  - Retrying is pending with `attempts > 0` (the data-sources DERIV), so a row on its first in-flight attempt shows as retrying for the length of that attempt.
  - G6(a): `is_test_key` admits `admin-send-test-` + a non-empty printable-ASCII suffix within the key column's 128-byte cap; `TEST_KEY_HEX` is gone. The restored probe's cap is that 128 (`mail::conformance::TEST_KEY_BYTES`), not the old exact 48.
  - The tile's "▲ relay throttled" delta value is FAKE and, like the banner and the attention item (G8), shows only while a row is retrying; its period is the real "· N parked". The spark is a constant FAKE series.
  - `OutboxStats` drops `oldest_pending_overdue_secs` (no element shows it) and splits pending into `pending` and `retrying`; `recent_tx` became `list_tx`.
  - An unknown `state` is an `Invalid` panel error. A test-send recipient that does not parse is `FieldErrors` on `to`; a key reused for a different recipient is a `conflict` error.
- Step 13 (part a):
  - The fan-out is `core/admin/src/engine/columns.rs`. It runs for an `ok` or `readonly` DataTable only, so a `stale` cached table shows without its list columns. Every column error, `NotFound` included, is the failure marker; a row whose entity the column did not answer draws an empty cell.
  - A list column has no fragment of its own: the panel route answers its URL with a 404 toast, and `mode_of` names it `column`. A panel of any other point that answers `ColumnCells` is an error panel (`panels::resolve`).
  - The `(order, module, id)` contribution order is `Snapshot::contributions`, shared by the routes and the fan-out.
  - `PanelQuery.refs` enters `cache_key` as its count followed by each id, ahead of `params`.
  - G6: with a `DataTable.item_noun` the foot reads "M accounts · showing 1–N" on the first page, "M accounts" past it (a cursor does not say how far in it is) and "N accounts" without a total. A table with no noun keeps "N of M" / "N rows".
  - G7: the portal draws the empty `th` and the trailing `td` of `TableRow` actions; a module declares no empty column. A row button is `btn sm`, plus `ghost` for a ghost emphasis and `danger-ghost` for a danger action, as in the mockup's Unlink. A queue-row action with no icon draws its label instead of an empty icon.
  - G7: Linked identities rows bind the provider name as `Row.item`, Active sessions rows the refresh family id; both actions are `implemented: false`, so nothing posts them yet.
  - G10: the header fragment swaps the last breadcrumb for `EntityHeader.crumb`, or `EntityHeader.title` when it is `None`. The player header's title is the display name and its subtitle `@Name#1234` (checklist §2); its `crumb` is the `Name#1234` handle (owner, 2026-11-17, round-1 review fix).
  - G12: Last seen is "—" for a player with no refresh token left. Active sessions lists the newest 50 live families; the meta counts every live one. Started is `YYYY-MM-DD HH:MM` UTC and Expires is relative ("in 6 h").
  - G13: the palette and the table share one reader, `Store::players_page`; the palette asks for at most 25 hits. The table refuses a search over 256 bytes, and an unknown `provider` filter value is `Invalid`.
  - G14: `Route::FirstRow(<list page>)`. `validate` refuses a `FirstRow` that names no `Mapped` page. A fleet with no row prints `EMPTY <page>` and fails the crawl (no CLEAN marker) unless `crawl --allow-empty`; the crawl summary counts the followed pages too, and `diff --page player` on an `--allow-empty` run refuses with that reason (round-1 review fix).
  - `Store::stats`, `Store::list_players` and `PlayerRow`, unused since Step 1, are replaced by the provider's readers (`player_count`, `players_page`, `player_row`, `identity_rows`, `live_families`, `active_players_24h`).
- **Step 28 (owner, 2026-11-18).** The covered-enum set is every enum `variants!` registers as drawn (`controlplane::declared_enums()`, a `linkme` distributed slice), not the enums seen in the golden run: an enum whose only golden is deleted, or that no golden converts (today `Point`, `Layout`, `TimeRange`), fails the gate. `Role` is drawn (`role_name`) and is covered. Every `pub enum` in `controlplane` is declared through `variants!` as `drawn` or `undrawn`; the gate fails on a converted undrawn enum, and a declaration-form test (the test-author's) fails on a `pub enum` outside `variants!`.
- **Step 13c.1a (2026-11-18).**
  - K18: `controlplane::Noun { one, many }` with `Noun::of(count)`; `DataTable.item_noun` is
    `Option<Noun>` (`None` counts "row"/"rows"), and `WorkQueue.item_noun` is a `Noun` too,
    since mail's queue is the other foot count.
  - K6: a `Toggle` cell is on when its value is `"true"`.
  - K14: a module is down when its source missed the latest manifest refresh
    (`Snapshot::module_ok`, the signal the nav's `down` uses).
  - X3: `controlplane::fake_hash(salt, id)` hashes the salt, a zero byte and the id; the
    portal's fakes now pass a salt, so their drawn values (service history, event-plane
    checkpoint and handler) changed. Accounts' values are unchanged.
  - K4: the event plane's `subscription` kind declares an `EntityHeader`, so its rows stay
    clickable and the two `tr.clickable` template-choice lines in `events.txt` stay; only the
    two muted-mono contract-gap lines are deleted.
  - No request field is added, so `tools/conformance` gets no policy row.
- **Step 13c.1b (2026-11-18).**
  - K12: the read-only banner draws `ContributionDecl.readonly` when set. A `writes: false`
    module keeps the template's default text, whose `<b>module</b>` markup a plain-text field
    cannot carry, so that text stays in `primitives.html.tmpl` as the default instead of
    dying.
  - Write admission (owner, 2026-11-18, review of 63a10deb): a declared `readonly` gates
    writes through one controlplane function, `Manifest::write_refusal`, used by
    `LocalFace::invoke` (refused `forbidden` before the provider runs), the portal's `admit`
    (drawn controls and submits; the hint is the contribution's declared text) and
    admincheck (an implemented action it refuses for a `readonly` contribution is a
    finding). A `Then::Link` action is never a write; `LocalFace::invoke` refuses invoking
    one as `invalid`, as the portal's submit already did, so a non-writer still never sees an
    invoke. admincheck also requires an `EntityTabCount` or `EntityTabTile` to name a tab an
    `EntityTab` of its kind declares.
  - One place each (owner, 2026-11-18, round-2 proof audit): `ActionPoint::contribution`
    names the contribution an action sits on, `Then::invokes` says whether it is invoked, and
    the panel's read-only state is derived from `Manifest::write_refusal` (`engine::panels`
    `is_readonly`; `has_write_affordance` is deleted). One state changes: an
    `implemented: false` `PanelForm` action on a non-`Form` body of a module that does not
    write no longer draws the panel read-only; every existing golden and test draws the same.
  - K13: a tab is down when a module contributing an `EntityTab` to it is down
    (`Snapshot::module_ok`); a down tab skips the count slots of down modules, which would
    draw a second dot.
  - K10: `Timeline.days: false` draws every entry in one list with no label; `true`
    labels every day, one day included (`bundle.js:192-201`). The date-only format is
    `Timeline.date_only` ("Oct 14"). The two-day golden case sets `days: true`, so no
    golden changed.
  - K11: an `EntityTabTile` draws in tile mode; a `KpiTiles` body draws one tile per entry
    inside its one slot (`.slot` is `display: contents`); an empty `KpiTiles` is the empty
    state. `make_finite` covers `KpiTiles`.
  - K15: `Layout::Rows` flattens nested rows; every row but the last carries
    `margin-bottom:12px`. `PageView.layout` and `columns` became `rows`
    (`render::layout_rows` replaces `layout_columns` and `layout_name`); admincheck reads
    the ids of every row.
  - K9: `PanelBody.tag` draws as `span.badge.plain` in the tools of every tooled state;
    `foot` shares the one `div.panel-foot` with Load older. The stored panel-form
    `Chrome` carries both.
  - K16: `FormSpec.title` titles the modal only. `Field.placeholder` draws on text,
    textarea and ref inputs (a ref falls back to "<kind> id"). `admin.forms` rows written
    before this step lack `Field.mono` and no longer decode; they expire within `FORM_TTL`
    under the wipe rule. `admin.reveals` rows written before it still decode: a missing
    `SecretReveal.extra` reads as `None` (pinned by a controlplane test), so no stored
    show-once secret is lost.
  - The accept entries for the restore link (K10) and the Grant/Revoke seg (K16) are written
    by their ports (Steps 23 and 15), since no accept file maps those pages yet; their todo
    items (ADMIN2-39, ADMIN2-40) land now.
  - Red by design until 13c.1g, besides the declared-variant gate: the two `Primitive`
    case-count checks (`golden_cases_cover_every_macro_variant_and_state`,
    `variant_coverage_fails_on_a_new_enum_variant`), since `KpiTiles` names the
    `kpi_tiles` case 13c.1g adds.
  - No request field is added, so `tools/conformance` gets no policy row.
- **Step 13c.2a2a (2026-11-19).**
  - Step 28 observes `Point`: `render::mode_of` records every point it converts, so
    `golden_renders_convert_every_drawn_enum_variant` fails on `Point::EntityLabel` until
    13c.2a2b records it at the labelled draw (owner, 2026-11-19; red by design, listed under
    the intermediate gate).
    The Cross-module line's "`Point` and `Layout` are placement enums Step 28 does not
    observe" is wrong for both.
  - No golden is added here. 13c.2a2b adds the labelled-draw golden and records
    `Point::EntityLabel` and `Point::EntityListColumn` where production draws them, not
    through `mode_of`; both leave the `slot_every_point` case (owner, 2026-11-19).
  - The panel route refuses an `EntityLabel`'s own URL as not found, as it refuses a list
    column's, so a label draws only through the refs it names; `mode_of` names it `label`.
  - The chunked call of one contribution under its decl's one deadline is
    `engine::columns::cells`, shared by the list-column fan-out and the label pass.
  - The pass (`engine::labels::pass`) is not yet called from the slot route; 13c.2a2b calls
    it there, after the body and its columns, and draws its labels.

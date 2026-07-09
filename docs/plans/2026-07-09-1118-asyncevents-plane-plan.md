# asyncevents: durable-events plane owned by app (de-modulize messaging)

**Date:** 2026-07-09 11:18
**Status:** reviewed (grumpy punch list 2026-07-09 addressed in place — see Risks)
**Decided with user:** the durable event plane is *process infrastructure*, not a
peer module. `requires()` becomes reserved for domain capabilities from `modules/`.
The crate/concept renames `messaging` → `asyncevents`.

## Context

### The problem (what accreted)

The durable plane's mechanics (outbox → relay → `POST /events` → inbox dedup) are
sound — they are the transactional-outbox answer to the dual-write problem and are
NOT touched by this plan. What accreted is the plane's *attachment*: it was dressed
as a `lifecycle::Module`, and every module-facing mechanism then had to be manually
fooled around it:

1. `Bus::set_transport` — capability registration outside the registry
   (`core/messaging/src/lib.rs:312`), with a panic-on-double-set imitating
   `registry::provide`.
2. `requires("messaging")` in 9 module manifests — a string with no observable
   counterpart (never keyed-`require`d), verified only by name-presence in
   `validate_requires`.
3. `ALLOWLIST: &["messaging"]` in requirecheck (`tools/requirecheck/src/main.rs:68`)
   — the checker must pretend not to see messaging in both directions.
4. A dead marker: `messaging::Service` provided under `"messaging"`
   (`core/messaging/src/lib.rs:313-315`) "for validate_requires" — but
   `validate_requires` (`core/app/src/lib.rs:149`) only checks `Module::name()`;
   the marker is resolved by nothing except messaging's own test.
5. 10 × `Box::new(messaging::Messaging::new())` in `cmd/*` mains with fragile
   ordering comments ("messaging LAST for Stop ordering"; inventory-svc/match-svc
   additionally need it BEFORE stubs because `configrpc`'s stub factory calls
   `on_tx` during `register`).
6. `Bus::on_tx` panics at runtime wiring (BLOCKER-2) and `emit_tx` returns
   `Error::NoTransport` — late, manual enforcement of "the plane exists here".

Evidence for the reclassification: 10 of 12 processes host messaging (all except
`gateway-svc` and `admin-svc`, both `without_db`); the plane's presence is exactly
DB-presence. A dependency with ~100% coverage among DB processes, no alternative
provider, and no possible `remote::Stub` (the transport must share the caller's
DB transaction — it is constitutively co-hosted) is a *plane of the process*, like
the HTTP listener, not a capability.

### Why not the alternatives (Research-before-planning rationale)

- **Why not keep the Module and fix only the checkers** (advisory allowlist +
  topiccheck deriving `on_tx ⟹ requires(messaging)`): the checker harness only
  runs `register`+`init`, so `emit_tx`-only producers (accounts, characters, match,
  config, scheduler — emits happen in request handlers / `start` tasks) stay
  invisible. Half the hole stays, plus all six accretions above stay.
- **Why not durable-plane-as-registry-capability** (`provide::<dyn Transport>` +
  keyed `require` per module): keeps the false classification. The registry seam
  exists for capabilities with alternative providers (local impl vs `remote::Stub`);
  the transport can never have a stub. It would also keep 9 manifest entries +
  per-module handle acquisition, i.e. ceremony repeating "I have a DB" ten times.
- **Why not a `messaging-svc` broker process**: physics. `emit_tx` rides the
  producer's open Postgres transaction; a remote enqueue reintroduces dual-write.
  A broker with its own storage still requires a local outbox (the Kafka pattern),
  so it removes zero blocks and adds a process. Rejected with user 2026-07-09.
- **Why not split durable methods off `Bus` into a `DurableBus` type**: 5 Service
  structs / OnceLock fields hold `Arc<Bus>` today (accounts, characters, match,
  config, scheduler) purely for `emit_tx`; a type split churns all of them plus all
  9 `on_tx` sites for zero behavioral gain once the *dependency* is honest. The
  in-process plane (`emit`/`on`) has **zero users outside `core/bus` tests**
  (verified by full sweep), so the facade carries no live ambiguity. Deferred; can
  be a later cleanup if the in-process plane ever gains users.

### Target design (summary)

- `core/asyncevents` (renamed from `core/messaging`) exposes a **`Plane`** — not a
  `lifecycle::Module`. `core/app::run` owns its lifecycle: constructed when the
  process has a DB, its `Transport` injected into the `Bus` **at Context
  construction**, its schema migrated before module migrations, its relay/LISTEN/
  housekeeping started after module starts, stopped (delivery halted) before any
  module stops.
- Modules: **zero declaration**. They keep calling `ctx.bus().emit_tx` /
  `on_tx` / `on_tx_raw` exactly as today. `requires()` shrinks to domain
  capabilities only. Fail-loud survives: `on_tx` in a plane-less process still
  panics at `init` (boot time); `emit_tx` still returns `NoTransport` (only
  reachable in a DB-less process, where no producer module can live anyway — they
  all own schemas).
- `Bus::set_transport` is deleted; the transport becomes a constructor argument.
  The double-set panic class disappears structurally.
- Renames: crate/dir `asyncevents`, DB schema `asyncevents` (guarded
  `ALTER SCHEMA ... RENAME`), NOTIFY channel `asyncevents_outbox`, env
  `MESSAGING_ORIGIN` → `EVENTS_ORIGIN`, `MESSAGING_RETENTION` →
  `EVENTS_RETENTION`, `MESSAGING_HOUSEKEEP_INTERVAL` →
  `EVENTS_HOUSEKEEP_INTERVAL`. `EVENTS_SUBSCRIBERS` unchanged. `core/outbox`
  keeps its name (generic relay library, schema name is a parameter).

### Key facts the steps rely on (from 11-subagent research, 2026-07-09)

- Durable call sites: 14 production sites total — subscribes in `init` (inventory
  ×3, rating, audit ×2, leaderboard) + one in `configrpc`'s remote factory (runs
  during `remote::Stub::register`, `api/config/rpc/src/lib.rs:147`); emits in
  request handlers via stored `Arc<Bus>` (accounts `lib.rs:174`, characters
  `lib.rs:219,262`, match `lib.rs:109`) and in detached `start` tasks (scheduler
  `lib.rs:177`, config `lib.rs:271`). Zero in-process `emit`/`on` users anywhere.
- Local-target snapshot timing: today messaging's `init` sees all module
  subscriptions because module inits precede it in list order (NB: in
  inventory-svc/match-svc the *stubs* come after messaging in the list — that is
  safe only because stub subscriptions happen in phase-1 `register`; the
  match-svc "messaging LAST" comment is factually wrong today and dies with this
  plan). In the new design the snapshot moves to `Plane::start()`, which runs
  after `App::build` — strictly safer; the `configrpc` stub-factory subscription
  (register phase) is also covered. Only inventory-svc hosts that factory;
  `ratingrpc`'s factory does no `on_tx`.
- `Bus`/`Context` constructors: `Bus` is built only inside `Context::new()`
  (`core/lifecycle/src/context.rs:39`); `Context` is built by `app::run`
  (`core/app/src/lib.rs:258-261`), the two checker tools, and ~30 test sites.
  Raw `Bus::new()` outside that: bus's own tests, scheduler test helper
  (`modules/scheduler/src/tests.rs:125-126`), and transport-less Service harnesses
  in characters/accounts tests (validation-only, never emit).
- Module `messaging` deps are all `[dev-dependencies]` (characters, config,
  accounts, match, leaderboard, inventory) — integration tests install the real
  transport and clean `messaging.outbox`.
- `verify` invokes: fortress = build 12 binaries + `archcheck` +
  `requirecheck --strict` + `topiccheck --durability-strict`; advisory
  `topiccheck --strict`; `public-api` diffs only `api/*` contract crates (the
  asyncevents crate is NOT in that set — no additive-only constraint on it).
- archcheck has three messaging carve-outs (`tools/archcheck/src/main.rs:15,130,211`).
- Scripts touch only env vars + SQL, not the crate name: `run.sh`/`run.ps1`,
  `split-proof.sh` (SQL at 717-721) / `split-proof.ps1` (SQL at 647-651),
  `scripts/smoke-split-messaging.sh` (SQL at 92-121).

---

## Steps

### Step 1 — `core/bus` + `core/lifecycle`: transport at construction `[fable]`

**(a) What:** `core/bus/src/lib.rs`, `core/bus/src/tests.rs`,
`core/lifecycle/src/context.rs`.

**(b) Why first:** every later step builds on the new construction path; this is
the seam change everything else compiles against.

**(c) How:**
- `Bus`: replace `transport: Mutex<Option<Arc<dyn Transport>>>` with
  `transport: Option<Arc<dyn Transport>>` (immutable). `Bus::new()` keeps meaning
  "no durable plane" (in-process only); add
  `Bus::with_transport(t: Arc<dyn Transport>) -> Bus`. Delete `set_transport`
  and the double-set panic. `require_transport` drops the lock. Keep
  `Error::NoTransport` and the `on_tx`/`on_tx_raw` panic, with the message
  rewritten to "this process hosts no durable-events plane (no DB); a durable
  subscriber cannot run here" (the old message names messaging's register phase).
- `Context`: keep `new()` / `with_db()` (both `Bus::new()`, no transport); add
  `pub fn with_db_and_transport(db: PgPool, transport: Arc<dyn bus::Transport>)`
  building `bus: Arc::new(Bus::with_transport(transport))`. `lifecycle` already
  depends on `bus`, so no new edge.
- Bus tests: delete `set_transport_panics_on_double_set`; rewrite
  `no_transport_resolves_to_err` and `on_tx_without_transport_panics` against
  `Bus::new()` (they stay meaningful — DB-less processes); switch
  `on_tx_and_on_tx_raw_record_topic_and_subscriber` and the `_tx` threading tests
  to `Bus::with_transport(...)`. `codec`/`TypedAdapter` tests untouched.
- Update `core/bus` module docs (`lib.rs:16-26`): the durable seam paragraph now
  says the transport is constructor-injected by the composition root (`core/app`),
  implemented by `core/asyncevents`.
- `core/registry/src/lib.rs:115-130`: the require-observer docs describe
  themselves against `Bus::set_transport` three times ("the honest analogue of",
  "unlike `Bus::set_transport` there is no double-install", and the manual
  rustdoc link target). Rewrite against the new model (constructor injection) in
  this step — the referent dies here, so its references die here.

NOTE: after this step `core/messaging` no longer compiles (calls `set_transport`)
— expected; Step 2 fixes it. **Commit boundary (reviewer #7):** the workspace
first compiles again after Step 6 (checkers also call the deleted
`set_transport`), so Steps 1–6 land as ONE commit (built by the lane agents as a
reviewed stack, committed by the coordinator); Steps 7 and 8 commit per step.
Lane agents must NOT ad-hoc "fix" cross-step breakage outside their step's scope.

### Step 2 — `core/messaging` → `core/asyncevents`: Module → Plane `[fable]`

**(a) What:** `git mv core/messaging core/asyncevents`; `core/asyncevents/Cargo.toml`
(`name = "asyncevents"`); root `Cargo.toml` (member path line 16, workspace-dep
line 172 → `asyncevents = { path = "core/asyncevents" }`); `src/lib.rs` +
`src/tests.rs` rewrite.

**(b) Why now:** Step 3 (`app::run`) needs the `Plane` API to exist.

**(c) How:**
- Delete: `impl Module for Messaging`, the `Messaging` struct, the `Service`
  marker trait + its `provide`, `caps()`, the register/init/migrate/start/stop
  split. Keep verbatim: `Inner` (the `Transport` impl: `enqueue_tx`, `consume`,
  `subscribe_tx`, `build_local_targets`), `handle_inbound`, `listen`, `housekeep`,
  `prune_once`, `origin_collision`, env/duration helpers.
- New public surface:
  ```rust
  pub struct Plane { inner: Arc<Inner>, pool: PgPool, listen_dsn: String,
                     cfg: StartCfg, subscribers: HashMap<String, Vec<String>>,
                     stop: Option<(watch::Sender<bool>, Vec<JoinHandle<()>>)> }
  impl Plane {
      /// Reads EVENTS_ORIGIN (default "monolith"), EVENTS_SUBSCRIBERS,
      /// EVENTS_RETENTION, EVENTS_HOUSEKEEP_INTERVAL. The LISTEN dsn is PASSED
      /// IN by app (the authoritative cfg.database_url) — the old env re-read of
      /// DATABASE_URL inside the crate was a second source of truth and dies here.
      pub fn new(pool: PgPool, listen_dsn: String) -> anyhow::Result<Plane>;
      /// The bus::Transport to inject at Context construction.
      pub fn transport(&self) -> Arc<dyn bus::Transport>;
      /// POST /events inbound sink (merged into the process router by app).
      pub fn router(&self) -> axum::Router;
      /// Own-schema DDL (guarded rename + IF NOT EXISTS), runs before module migrates.
      pub async fn migrate(&self) -> anyhow::Result<()>;
      /// Origin-collision guard, local-target snapshot, Relay::new, spawn
      /// relay + listen + housekeep. Runs AFTER App::start.
      pub async fn start(&mut self) -> anyhow::Result<()>;
      /// Halt delivery: flip stop, await tasks. Runs before Bus::close/App::stop.
      pub async fn stop(&mut self);
  }
  /// Test helper: a bare transport over (pool, origin) — replaces the
  /// Messaging::new()+register pattern module integration tests use today.
  pub fn transport(pool: PgPool, origin: &str) -> Arc<dyn bus::Transport>;
  ```
  Relay construction moves from init-time to `start()` (local-target snapshot is
  taken there — after all module inits AND stub registers, see Key facts).
- Schema rename inside `SCHEMA_DDL`, prepended guarded rename so existing dev DBs
  migrate in place:
  ```sql
  DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'messaging')
       AND NOT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'asyncevents')
    THEN ALTER SCHEMA messaging RENAME TO asyncevents; END IF;
  END $$;
  ```
  **Dedup-key rewrite (reviewer MAJOR #1):** the inbox dedup `event_id` is
  `"{schema}:{row.id}"` (`core/outbox/src/lib.rs:304`), so renamed-in-place rows
  that are still unsent with PARTIAL delivery would re-deliver under a new prefix
  and re-run already-succeeded handlers (e.g. leaderboard double-counts a win).
  Close it in the same DO block, right after the ALTER:
  ```sql
  UPDATE asyncevents.inbox
     SET event_id = 'asyncevents:' || substr(event_id, length('messaging:') + 1)
   WHERE event_id LIKE 'messaging:%';
  ```
  (Inside the IF branch, so it runs exactly once, atomically with the rename.)
  then the existing DDL with `asyncevents.` names; `notify_outbox` re-created via
  `CREATE OR REPLACE` with `pg_notify('asyncevents_outbox', ...)`;
  `NOTIFY_CHANNEL = "asyncevents_outbox"`. All SQL strings (`INSERT INTO`,
  housekeeping `DELETE`s) switch to `asyncevents.`.
- Env consts: `EVENTS_ORIGIN`, `EVENTS_RETENTION`, `EVENTS_HOUSEKEEP_INTERVAL`
  (defaults unchanged: `monolith`, `168h`, `1h`).
- Crate tests: `register_installs_transport_before_init` is replaced by a test
  that `Plane::new(pool)` + `Context::with_db_and_transport(pool, plane.transport())`
  lets `ctx.bus().on_tx(...)` register into `inner` (same BLOCKER-2 intent, new
  mechanism). The four `Inner`/`Relay` DB tests survive with table names updated
  to `asyncevents.outbox`/`asyncevents.inbox`; pure-fn tests untouched.

### Step 3 — `core/app::run` owns the plane `[fable]`

**(a) What:** `core/app/src/lib.rs` (+ its `Cargo.toml`: add `asyncevents`
workspace dep), `core/app/src/tests.rs` (docs-level assertions only — existing
tests use `Context::new()` and stay green).

**(b) Why now:** completes the compile chain 1→2→3; after this the workspace
builds except modules/cmd (Step 4).

**(c) How, in `run()`:**
1. After the pool (line ~253): `let mut plane = match (&pool, &cfg.database_url)
   { (Some(p), Some(dsn)) => Some(asyncevents::Plane::new(p.clone(), dsn.clone())?),
   _ => None };` (app passes its authoritative DSN for the LISTEN connection).
2. Context (replaces lines 258-261): DB+plane →
   `Context::with_db_and_transport(pool, plane.transport())`; no DB →
   `Context::new()`. (DB present ⇔ plane present; the pairing is the design.)
3. After `App::build` (line ~271): `if let Some(p) = &plane { ctx.mount(p.router()); }`
4. Before `app.migrate()` (line ~274): `if let Some(p) = &plane { p.migrate().await?; }`
5. After `app.start()` (line ~275): `if let Some(p) = &mut plane { p.start().await?; }`
6. Shutdown (lines ~400-413), new order documented in the Step-10 comment:
   player edge close → internal edge close → **`plane.stop().await`** (delivery
   halts first — replaces the "messaging registered last stops first" convention)
   → `ctx.bus().close().await` → `app.stop().await`.
- Rewrite the `run()` doc header: the durable plane is an app-owned plane like the
  HTTP/edge planes; a DB-less process has no durable plane and any `on_tx` in it
  fails loud at init.

### Step 4 — modules + cmd sweep: drop the ceremony `[sonnet]`

**(a) What:** 9 module `lib.rs` manifests, 10 `cmd/*/src/main.rs`, 10
`cmd/*/Cargo.toml`, 6 module `Cargo.toml` dev-dep renames,
`modules/rating/Cargo.toml` comment.

**(b) Why now:** pure mechanical unblocking of the workspace build after 1–3;
no design decisions left here.

**(c) How:**
- Delete `"messaging"` from `requires()` in: accounts `lib.rs:362-366`, audit
  `269-271`, characters `361-363`, config `596-601`, inventory `572-574` (keep
  `characters`, `config`), leaderboard `122-124`, match `158-160` (keep
  `rating`), rating `123-125`, scheduler `327-329`. Where the vec becomes empty,
  delete the whole `fn requires` override (trait default). Delete/rewrite the
  justifying comments (config `:596`, accounts `:362` explicitly explain the
  messaging require).
- In each of the 10 mains: delete the `Box::new(messaging::Messaging::new())`
  line and the "messaging LAST/BEFORE stubs" ordering comments; where a doc
  header explains `MESSAGING_ORIGIN`, reword to `EVENTS_ORIGIN` and "the durable
  plane is app-owned (DB ⇒ plane)". Remove `messaging = { workspace = true }`
  from the 10 `cmd/*/Cargo.toml`.
- Also (reviewer #4): `cmd/gateway-svc/src/main.rs:2-4` doc header ("no messaging
  module — the async plane … bypasses the front door") — reword to the plane
  vocabulary (gateway-svc has no DB ⇒ no plane); `modules/gateway/src/lib.rs:19`
  ("`POST /events` (messaging)") and `:201` ("messaging and `app` add plain
  routes") — reword. gateway-svc is NOT one of the 10 mains, so it needs this
  explicit line-item to not escape the sweep.
- Rename the 6 module `[dev-dependencies]` entries `messaging` → `asyncevents`
  (tests are fixed in Step 5; this keeps the dep graph consistent).

### Step 5 — test-harness adaptation `[opus]`

**(a) What:** `modules/{match,config,inventory,characters,accounts}/src/tests.rs`,
`modules/accounts/src/tests/dev_auth_gate.rs` (note: under `src/tests/`),
`modules/scheduler/src/tests.rs:125-126`, `core/asyncevents/src/tests.rs`
(follow-through from Step 2), `cmd/gateway-svc/tests/stub_swap.rs` (audit only —
it uses `Context::new()`; verify no durable sub trips the panic).
Leaderboard correction (reviewer #6b): `modules/leaderboard/src/tests.rs` does
NOT construct Messaging nor touch outbox/inbox SQL — only comments mention
messaging. Verify its `messaging` dev-dep (`Cargo.toml:33`) is actually unused
and DROP it rather than renaming (adjust the Step 4 bullet accordingly: 5 renames
+ 1 drop, executing agent confirms by compile). Also sweep comment-only mentions:
`modules/audit/src/tests.rs:2,4,66`, `modules/leaderboard/src/tests.rs:2,50,66`,
`core/outbox/src/tests.rs:27` (`valid_ident("messaging")` — switch the example
identifier to `asyncevents`).

**(b) Why now:** needs the Step-2 `asyncevents::transport(pool, origin)` helper
and the Step-1 constructors; must land before verify (Step 9) can run
`cargo test --workspace`.

**(c) How:**
- Every test that today installs the real transport by constructing
  `messaging::Messaging::new()` + driving `register` switches to:
  `let t = asyncevents::transport(pool.clone(), "test-origin");
   let ctx = Context::with_db_and_transport(pool, t);`
- `modules/scheduler/src/tests.rs` `bus_with_fake()`:
  `Bus::with_transport(ft.clone())` instead of `Bus::new()` + `set_transport`.
- `modules/match/src/tests.rs:134-155` (`match_requires_rating_and_fails_validate_without_it`):
  drop `Messaging` from both module sets; the test now asserts `requires(["rating"])`
  drift only. Update its comment (`:130-133`).
- All direct SQL against `messaging.outbox`/`messaging.inbox` in module tests
  (match `:76,93,103`, inventory `:295`, characters `:125,134,154`, config
  `:150,196,227`, accounts `:289,297`, asyncevents' own cleanup helpers) →
  `asyncevents.` names.
- Transport-less raw `Bus::new()` Service harnesses (characters `tests.rs:16`,
  accounts `tests.rs:183`, `dev_auth_gate.rs:20,84`) are correct as-is (validation
  paths, never emit) — leave, but confirm they compile against the new `Bus`.

### Step 6 — checkers: requirecheck, topiccheck, archcheck `[opus]`

**(a) What:** `tools/requirecheck/src/{main,tests}.rs` + `Cargo.toml`,
`tools/topiccheck/src/main.rs` + `Cargo.toml`, `tools/archcheck/src/main.rs`.

**(b) Why now:** they compile against Steps 1–2 APIs and gate Step 9
(`fortress` runs all three).

**(c) How:**
- requirecheck: delete `const ALLOWLIST` (pass `&[]` at the two call sites — the
  `undeclared()` signature keeps its generic `allow` param for testability);
  replace `Context::with_db(pool)` + `set_transport(NoopTransport)` with
  `Context::with_db_and_transport(pool, Arc::new(NoopTransport))` (NoopTransport
  struct stays — the harness still must not panic on `on_tx`); delete the
  `messaging_is_allowlisted_as_provider_and_declaration` test; strip `"messaging"`
  from the two declared-fixtures; rewrite the doc header (drop "the honest
  analogue of `Bus::set_transport`" framing and the ALLOWLIST rationale).
- topiccheck: same constructor swap for `RecordingTransport`; reword the
  "stands in for core/messaging" / "MINUS messaging" comments (there is no module
  to exclude — the tool simply provides its own recording plane); Cargo.toml
  comment update.
- archcheck: update the three carve-outs (`main.rs:15,130,211`): crate name →
  `asyncevents` in the "module tests may reach a core crate" exception; delete or
  reword any "which processes require messaging" logic per what actually exists
  there (the researcher saw comments + a name check; the executing agent reads the
  file and keeps the enforced law identical in strength).

### Step 7 — scripts: env + SQL rename `[sonnet]`

**(a) What:** `run.sh`, `run.ps1`, `split-proof.sh`, `split-proof.ps1`,
`scripts/smoke-split-messaging.sh` (→ rename file to
`scripts/smoke-split-asyncevents.sh`).

**(b) Why now:** must land before Step 9 runs `./verify.sh` (split-proof is a
blocking stage and reads these env names + SQL).

**(c) How:** mechanical, in lockstep with Step 2's env consts:
- `MESSAGING_ORIGIN=` → `EVENTS_ORIGIN=` at every assignment site (run.sh ×11,
  run.ps1 ×11, split-proof.sh ×16, split-proof.ps1 ×14, incl. the ps1 monolith-stage
  env-neutralization lines).
- SQL: `messaging.outbox` → `asyncevents.outbox` (split-proof.sh:717-721,
  split-proof.ps1:647-651, smoke script:92-121).
- Reword inline comments that explain "messaging's relay/origin-collision guard/
  Stop-ordering-last" to the plane vocabulary. `EVENTS_SUBSCRIBERS` lines untouched.

### Step 8 — docs + memory `[opus]`

**(a) What:** `CLAUDE.md`, stale rustdoc outside the already-touched crates
(`core/outbox/src/lib.rs` header mention, `tools/topiccheck` header — if not fully
covered in Step 6), `memory/durable-event-plane-bus-owned.md`, `memory/MEMORY.md`,
live agent memory + `scripts/memory-sync.ps1 push`.

**(b) Why now:** after the code settles so the docs describe reality; before the
final verify commit so the tree is coherent.

**(c) How — CLAUDE.md edits (all six sites from research):**
- Core-crate list (line ~45): `messaging` → `asyncevents`, described as "the
  durable async-events plane, owned by `app::run` (DB ⇒ plane), not a module".
- Hard constraint 4 (line ~60): drop "(`messaging` counts …)" — new sentence:
  `requires()` names domain capabilities from `modules/` only; process
  infrastructure (asyncevents plane, metrics, DB, HTTP) is never declared.
- Constraint 8 (line ~73): drop "messaging registers last…"; note the plane's
  ordering is structural in `app::run` (transport at Context construction;
  delivery halts before module stop).
- Seam #3: subscriber wording unchanged; add "the plane is present iff the
  process has a DB; `cmd/*` never lists it".
- Recipe steps 4–5 (lines ~94-107): svc main template loses the messaging line;
  `MESSAGING_ORIGIN` → `EVENTS_ORIGIN`.
- Layout (line ~212): `outbox/ asyncevents/ — durable plane: relay lib; plane`.
- Stale "messaging precedent" comments (reviewer #3): `core/metrics/src/lib.rs:2`
  and `core/metrics/Cargo.toml:11` justify metrics-as-core-infra-Module via "the
  `messaging` precedent" — the precedent dissolves; reword to stand on its own
  ("core-infra Module listed by every main").
- Memory: rewrite `durable-event-plane-bus-owned.md` (plane is app-owned, not
  `modules/messaging`; `SetTransport` no longer exists; keep the don't-re-propose-
  per-module-sinks warning; update the smoke-script filename it cites) and the
  `MEMORY.md` index line; also touch the residue in
  `memory/rust-sketch-split-verified-m1.md:12` and
  `memory/dont-descope-transport-for-simplicity.md:12-14` (vocabulary only);
  mirror all of it into live agent memory, then `scripts/memory-sync.ps1 push`.

### Step 9 — verify, both topologies `[inline]`

**(a) What:** the full safety net + the one migration-specific check.

**(c) How:**
1. `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings
   && cargo test --workspace`
2. `cargo run -p archcheck`, `cargo run -p requirecheck -- --strict`
   (expect: table with NO allowlist column-noise, zero violations),
   `cargo run -p topiccheck -- --strict`.
3. Migration check on the dev DB (which has a live `messaging` schema):
   boot the monolith once, then
   `psql -c "\dn"` shows `asyncevents` and no `messaging`;
   `SELECT count(*) FROM asyncevents.outbox` returns the pre-rename rows.
4. `./verify.ps1` (blocking tiers include fortress + split-proof — the split
   topology with the renamed env vars is exercised end-to-end, monolith parity
   included). Per the verify-the-at-risk-path memory: split-proof IS the at-risk
   path here (env rename + plane start/stop ordering + relay snapshot timing).
5. Commits per the Step-1 boundary note: Steps 1–6 as one commit
   (`refactor(asyncevents,bus,app): de-modulize messaging into an app-owned
   durable-events plane`), Steps 7–8 per step; trailer = coordinator/executing
   model per commit-format rules; trailer audit before "done".

## Risks / decisions taken

- **Schema rename migrates in place** via guarded `ALTER SCHEMA` + inbox
  dedup-prefix rewrite (see Step 2 — closes the re-delivery-after-rename hazard
  the reviewer flagged). If both schemas somehow exist, the guard leaves
  `messaging` orphaned — accepted (impossible unless someone hand-created
  `asyncevents` earlier).
- **No mixed-version window** (reviewer #9): an old binary running against a
  renamed schema hard-fails every `emit_tx` (relation gone → domain tx fails)
  and LISTENs on a dead channel. This commit requires a whole-fleet restart; no
  rolling deploy across it. Single-box dev with full-restart scripts — moot in
  practice, stated for honesty.
- **`EVENTS_ORIGIN` chosen over `ASYNCEVENTS_ORIGIN`** — pairs with the existing
  `EVENTS_SUBSCRIBERS`; both describe the events plane, not the crate.
- **No `ctx.durable()` handle / no DurableBus type** — see "why not" above;
  module API is 100% unchanged, which is the main churn-limiter of this plan.
- **Plane ⇔ DB coupling** is a deliberate simplification (true for all 12
  processes today). If a DB-process-without-plane ever appears, `app::Config`
  grows an explicit `without_events_plane()` — one line, later.
- **Old plan docs** (`2026-07-07-1527-bus-owned-transport-plan.md`, checker plan
  `2026-07-09-0839`) stay as dated historical records — not updated.

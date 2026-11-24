# GameBackend Architecture

A game backend in **Rust** (Cargo workspace), built as a serious architecture
experiment — a **modular monolith with a proven split**: one repo, one
`cmd/server` binary for the monolith, AND every domain module compiles and
boots as its own `cmd/<name>-svc` process. The system grows by addition
(Open/Closed at the architecture level): new modules, contract traits, and
subscriptions, with no module importing another's implementation.
The retired Go original lives at `experiments/go-sketch/` (archived reference
— do not evolve it).

## The point of this codebase

Three seams carry all extensibility; almost everything else follows from them:

1. **Module registry** (`core/lifecycle`) — every feature is a `lifecycle::Module`
   and self-registers in a `cmd/*` main. Foundations never import a module.
   Dependencies are a manifest (`requires()`), **NOT** topologically sorted: the
   two-phase build (every provider's `register` runs before any module's `init`)
   makes init order commutative. A missing required capability in a process's module
   set fails loudly at startup (`app::validate_requires`).
2. **Service registry** (`registry::provide` / `require`, over `ctx.registry()`) —
   for *synchronous* needs ("ask B now, get an answer"). The consumer imports the
   provider's **contract crate** (`<name>api`, a trait — never the impl crate) and
   `require`s `dyn Trait` under `registry::key(provider, snake_trait)`. In a split
   process, a `remote::Stub` provides an edge-backed client under the SAME key —
   the registry swap is the only difference between topologies.
   Generated RPC operations default to `opsapi::RetryMode::Never`. Only a
   read/idempotent contract method explicitly marked `#[retry_safe]` gets one replay
   after reconnect (`RetryMode::OnceAfterReconnect`); mutating methods remain
   fail-closed unless their contract supplies its own idempotency semantics. An
   internal-edge method-name mismatch is a typed `edge::Error::UnknownMethod`
   mapped to `opsapi::Error::Status::NotFound` (non-retryable) — a deliberate
   choice that a gateway→svc unknown-method surfaces front-visibly the same as
   a domain-level 404. Each internal stream's request read and response-write
   half are independently bounded by `EDGE_STREAM_GRACE` (30s) so a peer stuck
   at the application level can't leak the stream task/slot past the (higher)
   connection idle timeout; the handler dispatch in between stays unbounded.
3. **Event bus** (`ctx.bus()`) — the async glue. Each publishing domain owns
   `api/<name>/<name>events` declaring versioned contracts via
   `bus::define(topic, version, HistoryPolicy)`. **Every cross-module event is
   DURABLE**, over an XID-ordered shared Postgres log with consumer-owned pull
   subscriptions (*publisher owns the event, consumer owns the subscription*):
   producer `emit_tx(AnyTx::new(&mut *tx), …)` inside a real DB tx — append once,
   never knowing its consumers; consumer `on_tx(SubscriptionSpec, …)`/`on_tx_raw`
   with a globally unique versioned subscription id (`inventory.character-created.v1`)
   and an explicit `StartPosition`. The handler receives `Delivery { event_id, tx }`
   and its effect + checkpoint commit in ONE transaction — no inbox, no dedup. The
   contract: *delivery is at-least-once per subscription with a stable `event_id`;
   effects are exactly-once for a `TransactionalPg` consumer; ordering is
   per-subscription in XID-allocation order; a poison event backs off and pauses
   its one subscription, never auto-skipped (operator surface:
   `cargo run -p eventctl`).* The plane is installed by `app::run` at `Context`
   construction (DB ⇒ plane), never by a module; a DB-less process hosts no plane.
   There is NO per-process event-routing config — monolith and split run identical
   producer/consumer code. Plain `emit`/`on` is in-process only, for same-module
   reactions; replica-local cache refresh is **`core/invalidation`** (LISTEN/NOTIFY
   broadcast + authoritative refresh callbacks — freshness, not delivery), never a
   durable subscription.

Plus two minor seams: **`ctx.contribute(slot, v)` / `ctx.contributions(slot)`** — a
multi-value registry for cross-cutting collections (control-plane providers via `controlplane::PROVIDER_SLOT`,
ops via `opsapi::{SLOT,BINDING_SLOT,LOCAL_SLOT}`, readiness checks, remote boot
hooks) — and **`edge::EDGE_SLOT`**: a module contributes its QUIC edge registrations
(`edge::EdgeReg` wrapping its own generated `register_server` glue) UNCONDITIONALLY
in `init`; `app::run` applies them iff the process serves an internal edge. The
module never knows the topology. Wire-method names are unique per process:
`edge::Server::handle/handle_identity` PANIC on a duplicate registration — a
collision is a loud boot failure (same convention as `registry::provide`),
never a silent last-writer-wins overwrite.

## Hard constraints (do not violate without discussing)

1. **Foundations (`core/*`) never depend on a module or an `api/` crate.**
   Dependency only ever points module → core. (`core/` = app, bus, contrib, edge, push,
   lifecycle, opsapi, registry, asyncevents, invalidation, remote, metrics, httpmw,
   controlplane, controlplane-rpc, admin —
   `asyncevents` (durable event log + pull workers) and `invalidation` (broadcast
   cache refresh) are app-owned planes (DB ⇒ plane), NOT modules; remote/metrics/
   httpmw are process infrastructure, not domains; `admin` (`core/admin`, the GameOps
   portal) is core, not a module, and `controlplane`/`controlplane-rpc` are the
   provider contract and its `cp.*` edge glue; `push` is the server→client
   delivery model (`Target`/`Message`/`Sink`), transport-free — the socket-owning
   transport lives in `modules/gateway`. Module SQL may call the plane's
   SQL functions (`asyncevents.append_event`, `asyncevents.ensure_history_contract`)
   but never touch plane tables — archcheck-enforced.)
2. **Fortress rule.** Every folder in `modules/` is a fortress: it never imports
   another module's impl crate, and every domain module compiles + boots as its own
   `cmd/<name>-svc`. The only gates are the contract crates under `api/<name>/`.
   Enforced mechanically by the blocking `fortress` stage in `verifyctl`: it builds
   `server` and every on-disk `cmd/*-svc`, then runs `archcheck`, strict capability
   requirement checking, and durability-strict topic checking. `archcheck` rejects
   module→module edges, foreign `<name>rpc` edges, `Option<edge::Server>` in
   `modules/`, missing `cmd/<name>-svc` roots, and illegal demo consumers. NO
   exceptions — non-shipping demo crates live under `demos/`
   (importable ONLY by `cmd/server`, archcheck-enforced), not in `modules/`.
3. **Contract surface per domain, under `api/<name>/`:** `<name>api` (pure traits +
   `#[rpc]`, transport-free — importable by any module), `<name>events` (payloads +
   `bus::define` descriptors — importable by any module), `<name>rpc` (generated
   transport glue via the meta-callback macro — importable ONLY by its own module,
   `cmd/*` roots, and other `api/*/rpc` crates; never by a foreign module).
4. Depend on a capability trait, not an impl crate. Declared `requires()` names
   domain capabilities from `modules/` only and must match real sync deps; process
   infrastructure (the `asyncevents` plane, metrics, the DB, HTTP) is never declared.
5. **Modules are topology-blind.** No `Option<transport>`, no `if split`, no env
   topology branches in domain code. Edge exposure goes through `EDGE_SLOT`;
   remote resolution through the registry swap; durable delivery through the bus.
   `cmd/*` mains differ only in module list + which QUIC planes the process serves.
6. Evolve events additively; never mutate a published payload shape — a breaking
   change is a NEW contract version (`define(topic, 2, …)`) and new subscription
   ids. Guarded by the `public-api` verify stage (each contract crate's surface
   diffed against a committed snapshot in `docs/reference/public-api-baseline/`;
   any diff FAILs — removed symbols BREAKING, added ADDITIVE — re-bless intentional
   changes with `cargo run -p verifyctl -- --bless-public-api`) and the advisory
   `topiccheck` stage (profile-aware: defined-vs-subscribed
   drift (blocking under `--durability-strict`; sanctioned sinkless topics live in
   topiccheck's `ALLOW_UNSUBSCRIBED`), version match, globally unique subscription
   ids, exactly one host per subscription per deployment profile).
7. **The bus is async fire-and-forget** — no request/response through it; that's a
   registry capability's job. State projected from events is eventually consistent.
8. Lifecycle: `register` (phase 1, provide services, no I/O) → `init` (wiring only,
   no I/O — contribute slots, subscribe, mount routes) → `migrate` (own schema
   only) → `start` (background work, first I/O) → `stop` (reverse registration
   order). Both planes' ordering is structural in `app::run`: transport +
   invalidation handle injected at `Context` construction, plane schema migrates
   before any module migrates, planes start after modules start (invalidation
   completes every callback's first refresh BEFORE durable delivery starts — a
   durable handler must never read a cold replica-local cache — or startup
   fails), delivery halts before any module stops, and BOTH QUIC planes drain
   in-flight handlers before
   modules stop (`RunningServer::shutdown`, `EDGE_DRAIN_GRACE_MS` default 5000 —
   read in `core/app`, never in modules), and the HTTP graceful drain is itself
   time-bounded (`HTTP_DRAIN_GRACE_MS` default 5000 — read in `core/app`, never in
   modules) so a hung connection can't stall shutdown before teardown begins;
   every process's inbound HTTP is bounded whole-request by
   `HTTP_REQUEST_TIMEOUT_MS` (default 30000, `0` disables — read in `core/app`,
   never in modules; elapse = deliberate 408) so a trickle upload can't pin a
   handler; and
   each module's `stop` (in both ordered teardown and the start-unwind) is itself
   bounded (`MODULE_STOP_GRACE_MS` default 5000 — read in `core/app`, never in
   modules) so one hung module can't stall the rest. A
   failed startup unwinds what started, in reverse, through the same teardown.
   Durable workers visit subscriptions fairly with a fixed 64-delivery quantum per
   subscription/pass. `ASYNCEVENTS_HANDLER_TIMEOUT` (default `10s`, invalid values
   fail startup) bounds each cooperative handler; plane stop has a 5s global grace,
   terminates still-active dedicated Postgres delivery backends, then aborts tasks.
   A Tokio timeout cannot preempt handler code that synchronously CPU-spins without
   yielding, so handlers must remain async-cooperative. `/readyz` flips not only
   when a worker task died but also when delivery has gone STALE (no healthy
   pass completed in 30s — e.g. a worker alive but looping on connection
   errors) and when retention has gone STALE the same way (no successful sweep
   in 3x the housekeep interval; sweep errors also count in
   `asyncevents_retention_sweep_errors_total`), and each worker's own delivery session carries an
   `idle_in_transaction_session_timeout` (2x the handler timeout) as a belt
   against that worker leaking its OWN open transaction — it does not cover a
   rogue idle-in-tx session elsewhere in the cluster (see
   `docs/reference/event-plane-ops.md`).
9. Events are typed at the seam: declare with `bus::define`, publish/subscribe via
   `emit_tx`/`on_tx`. `on_tx_raw` (untyped JSON) is for deliberately zero-coupling
   sinks (audit) only.
10. **Persistence = one shared Postgres, full logical isolation.** Schema-per-module,
    no cross-module FKs; a relation to another module is a plain id column, resolved
    via capability or synced via durable events. **Tests live in separate files**
    (`src/tests.rs` / `src/<file>_tests.rs`), never inline in impl files. One shared
    HTTP framework (axum) is blessed the same way — `ctx.mount(Router)` is the
    sanctioned surface for the HTTP-surface owners (webui, `core/admin`,
    accounts-OAuth, gateway).

## Adding a module

Use the `add-game-module` skill (`.agents/skills/add-game-module/SKILL.md`); a
module must boot as its own `cmd/<name>-svc` and ship in both topologies.

## Domain modules

`.claude/rules/domain-modules.md` (loaded by path in Claude Code).

## Commands

```
cargo build -p demoseed               # build before the fleet boots
cargo run -p devctl -- up monolith    # or: up split; then status / down
cargo run -p verifyctl -- --fast      # blocking tiers; --all, --all --strict, --slow for more
target/debug/demoseed                 # with the monolith up, run by hand; `--help` lists the flags
```

`demoseed` seeds through the gateway's public API, reads every seeded kind back, then runs
throwaway client scenarios. Flags: `--base-url`, `--players` (default 30, at least 2),
`--matches` (100, 0 allowed), `--concurrency` (16, at least 1), `--clients` (8, at least 1),
`--event-timeout <secs>` (10, at least 1). Exit 1 when any call failed (unexpected status,
transport, shape or timeout) or an op of `opscatalog::OPERATIONS`
never answered its declared success; otherwise 0. It is not a `verifyctl` stage.

Stages and flags: read `tools/verifyctl` and `tools/devctl`. For `weles` see
`docs/reference/weles-design.md`.

## Dev tooling scope

`.claude/rules/dev-tooling.md` (loaded by path in Claude Code).

## One test rollout at a time

At most ONE rollout-bearing command (`devctl up`, `verifyctl`, or an explicitly
requested ad-hoc `cargo test`) runs on this machine at a time; they share one
local Postgres and concurrent runs hang or fail with `tuple concurrently updated`.
The `pgrep`/`tasklist` check for a live cargo/rustc is its own call, read before
the launch; never start a second cargo "to check something". At most one
subagent runs tests at a time and its prompt carries this check. Load the
`safe-verification` skill before any such run. A fleet found running may be the
owner's: ask before stopping it or waiting on it. Wait for "running", never
"healthy".

## Database

Connection from `DATABASE_URL`, default
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.
Integration tests target this local Postgres directly (no Docker/testcontainers).
(Admin/superuser credentials for provisioning are in local agent memory, not
committed.)

**A DB test that cannot reach Postgres FAILS the run.** Every live-DB test takes its
pool from the one authority, `tools/testdb` (`testdb::test_pool`, a dev-dependency),
which panics when the cluster is unreachable — a green `cargo test --workspace` can
never mean "the database tests silently did not run". The single opt-out is
`TESTDB_ALLOW_SKIP=1` (explicitly truthy, unset = strict), for working without a
cluster — and it is a LOCAL convenience only: **`verifyctl`'s `test` stage FAILS while
it is on**, because libtest captures a passing test's output, so an opted-out run
would be green and silent about having proven nothing. `testdb`'s own suite scans the
workspace crate by crate and fails if any one grows a second skip authority.

**No data migrations — wipe is the migration strategy (current phase).** This is
pre-production with no persistent users yet: when a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole DB)
and boot fresh — do NOT build bridges, dual-writes, backfills, or versioned
data-migration machinery (the event-log rollout deliberately deleted exactly that
class of code). Module `migrate` is create-only DDL through `lifecycle::Ddl`
(`core/lifecycle/src/ddl.rs`: one bounded transaction, indexes and triggers
created only when missing), nothing more. If losing dev data hurts, the answer
is a **seed script** minting fake data (like the `APIKEYS_DEV_SEED` dev-keys
upsert), not a migration.
Revisit only if this ever grows real persistent users.

```
PGPASSWORD=gamebackend psql -U gamebackend -h localhost -d gamebackend
```

Installers that keep `psql` off `PATH` (Windows, Homebrew) need the full path —
see [platform notes](../../docs/reference/platform-notes.md).

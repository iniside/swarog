# CLAUDE.md

Guidance for working in this repo. A game backend in **Rust** (Cargo
workspace), built as a serious architecture experiment — a **modular monolith with a
proven split**: one repo, one
`cmd/server` binary for the monolith, AND every domain module compiles and boots as
its own `cmd/<name>-svc` process. Features are added by *writing new code, not
modifying existing code* (Open/Closed at the architecture level). The retired Go
original lives at `experiments/go-sketch/` (archived reference — do not evolve it).

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
multi-value registry for cross-cutting collections (admin items via `adminapi::SLOT`,
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
   Dependency only ever points module → core. (`core/` = app, bus, contrib, edge,
   lifecycle, opsapi, registry, asyncevents, invalidation, remote, metrics, httpmw —
   `asyncevents` (durable event log + pull workers) and `invalidation` (broadcast
   cache refresh) are app-owned planes (DB ⇒ plane), NOT modules; remote/metrics/
   httpmw are process infrastructure, not domains. Module SQL may call the plane's
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
    sanctioned surface for the HTTP-surface owners (webui, admin, accounts-OAuth,
    gateway).

## Adding a module (the recipe)

1. `modules/<name>/`: implement `lifecycle::Module` (`name`, `requires`, `init`; +
   `register` if it provides a capability, `migrate` if it persists, `start`/`stop`
   for background work). Tests in `src/tests.rs`.
2. Contracts in `api/<name>/`: `<name>events` (if it publishes), `<name>api` with
   `#[rpc]` traits (if it exposes sync capability — `#[http(...)]` for player-facing
   ops, plain for wire-only), `<name>rpc` containing the one-line
   `<prefix>_<snake>_meta!(rpc_macro::generate_glue);` invocation (+ re-export
   `adminrpc::register_admin` if it has an admin page).
3. In `init`: contribute ops to the `opsapi` slots, edge faces to `edge::EDGE_SLOT`
   (own glue), admin item to `adminapi::SLOT`; subscribe with
   `on_tx(SubscriptionSpec { id: "<name>.<topic-kebab>.v1", start: StartPosition::… }, …)`
   — the id is a durable contract, the start position has no default. Emit with
   `emit_tx` inside your store tx. Replica-local caches refresh via
   `ctx.invalidation().register(channel, name, callback)`, not a durable sub.
4. New `cmd/<name>-svc`: `src/lib.rs` exports
   `modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>>` (the `metrics`
   core-infra module + your module + a `remote::Stub` per consumed capability —
   peer addresses come from `wiring`, checkers pass dummies); `main.rs` builds the
   real `ProcessWiring` from env and adds runtime-only handles. Both planes are
   app-owned (DB ⇒ plane), never listed. It hosts NO gateway (FrontDoor) — the
   single public front door lives only in `cmd/gateway-svc` + `cmd/server`
   (monolith); the svc serves its ops ONLY over the internal mTLS edge and
   gateway-svc dispatches to it Remote. Register the module in `cmd/server`'s lib,
   add stubs where consumers live, add the svc lib to `tools/checkmodules`'s Split
   profile, add its typed env/ports/dependencies to
   `tools/processctl/src/fleet.rs`, and extend `tools/splitproof` with a named
   assertion (HTTP ops asserted THROUGH gateway-svc; the harness's fleet-drift
   preflight fails if the centralized fleet != `cmd/*-svc` on disk). Add the module
   to the centralized convention inventory in `tools/conformance/src/policy.rs`;
   use the smallest executable fixture that proves each applicable convention and
   give every genuine non-applicability a concrete reason.
5. No event-routing wiring exists: producers append to the shared log, consumers
   pull from their checkpoint — the same code in monolith and split. `topiccheck`
   validates the subscription graph per deployment profile.

## Domain modules (11 fortresses + gateway)

- **accounts** — identity: one `player_id`, many identities (`provider`,`subject`),
  opaque DB sessions (30-day TTL). Dev/password auth (argon2id, `ACCOUNTS_DEV_AUTH`
  explicit-only — default OFF/fail-closed, loud warn when ON; the `devctl` and
  split-proof development profiles set `ACCOUNTS_DEV_AUTH=1`), Epic OIDC verifier
  (`EPIC_CLIENT_ID`, JWKS, RS256/ES256),
  Epic web OAuth link/login (`EPIC_CLIENT_SECRET`, `/accounts/epic/start|callback`).
  Emits durable `player.registered`. The gateway's session verifier resolves
  `accountsapi::Sessions` — a process hosting a gateway without the accounts
  capability FAILS STARTUP unless `ACCOUNTS_DEV_AUTH=1` (dev verifier, loud warn).
- **characters / inventory** — the modularity reference case: plain-id relations,
  sync `Ownership` authz over the wire, starter-grant/wipe via durable
  `character.created/deleted`. `INVENTORY_DEV_GRANT` (explicit-only — default
  OFF/fail-closed, set by the `devctl` and split-proof development profiles) enables the simulated-IAP
  grant route.
- **config** — DB-backed knobs with a monotonic `config.revision`. A row trigger
  (INSERT/UPDATE/DELETE) increments the revision, NOTIFYs `config_changed`, and
  appends durable `config.changed` via `asyncevents.append_event` — a raw psql
  write emits identically to a service write. The NOTIFY payload is value-less
  (`namespace`/`key`/`operation`/`revision` only — `pg_notify` hard-caps at 8000
  bytes and the invalidation callback re-reads the whole snapshot anyway); the
  durable `config.changed` event still carries the full `value`. Snapshot =
  `{revision, settings}` in one statement. Local `Service` and remote
  `CachedConfig` (via `configrpc`) are invalidation callbacks (atomic map swap,
  apply only newer revisions); `CachedConfig` keeps boot-fill-or-fail-startup.
- **admin** — GameOps portal at `/admin` (minijinja over the embedded Go-era theme).
  **Session auth** (owns schema `admin`: users/sessions/login_attempts): argon2id
  passwords, opaque token + per-session CSRF in an `HttpOnly`/`SameSite=Strict`/
  `Path=/admin` cookie (`Secure` unless `ADMIN_COOKIE_SECURE=0` — dev opt-out, loud
  warn), 12h TTL; asymmetric lockout (user locks at 5 fails, IP at 20,
  `least(2^fails,900)s` backoff, trusted-proxy client IP via `TRUSTED_PROXY_CIDRS`);
  one generic 401 for wrong-pass/unknown-user/locked (no username oracle); CSRF
  checked BEFORE the local/remote editability decision; security headers on the
  admin router only. Login admission is bounded at 32 concurrent requests and
  5 rps/burst 20 per resolved client IP; Argon2 runs in `spawn_blocking` behind
  2 permits, owned BY the blocking closure (not the async handler frame) so a
  cancelled request can't release its permit while the hash keeps running
  detached — login admission (`login_slots`/`IpLimiter`) still releases on
  cancel by design. Username input is capped at 128 bytes, password input at 1024 bytes,
  and stale unlocked `login_attempts` rows older than 24h are deleted in batches
  of 256. The `admin_login_attempts_updated_idx` addition rolls out by `DROP SCHEMA
  admin CASCADE`, fresh boot, then user reseed with `adminctl` — no data migration,
  backfill, or compatibility bridge. Admin users are created by **`cargo run -p adminctl`**
  (`create-user` upsert = also password reset, `--password-stdin`/`ADMINCTL_PASSWORD`,
  never argv) wrapped by the **`install`** script for your shell; zero-user boot warns
  instead of failing; `ADMIN_OPEN=1` bypasses sessions AND CSRF (deliberately open
  local portal, loud warn). `ADMIN_USER`/`ADMIN_PASS` no longer exist. Emits durable
  `admin.action` (login-succeeded/login-locked/logout — local in BOTH topologies —
  plus form-submit where the form's module is co-hosted; field names only, never
  values). Renders contributed `adminapi::Item`s; remote items fan out over QUIC via
  `admin.adminData` (`adminrpc::admin_remote_factory`). Remote forms are read-only.
  admin-svc has a DB (schema `admin` + the durable plane) — no longer planeless.
- **audit** — append-only ledger (`audit.log`), zero-coupling raw durable sinks for
  all 6 ledger topics — six independent subscriptions (`audit.<topic-kebab>.v1`), each
  with its own checkpoint, plus a 7th independent subscription for prune reacting
  to `scheduler.fired{audit-prune}`
  (`AUDIT_RETENTION_DAYS`, default 30).
- **scheduler** — data-driven schedules (`scheduler.schedules`), 1s tick, per-name
  `pg_try_advisory_lock` + still-due re-check + `UPDATE`+`emit_tx` in one tx,
  commit-before-unlock. Each fire runs on its own DEDICATED connection (derived
  from the pool's connect options — dropping it closes the session, so an abort
  can never strand the advisory lock in the pool), acquire/connect waits are
  bounded (5s), the whole tick shares ONE 30s budget (exhaustion skips the
  remaining due schedules to the next tick), and `stop()` grace-then-aborts its
  tasks (4s, under the app-level 5s) instead of joining forever.
  `SCHEDULER_ENABLED`.
- **match / rating / leaderboard** — match records `match.matches` from a
  `/match/report` HTTP request body (a REQUIRED `ReportId` idempotency key —
  a duplicate `ReportId` with the SAME winner/loser is a 202 no-op, a duplicate
  `ReportId` with a DIFFERENT winner/loser is a 409 Conflict, and `report` is
  explicitly `#[retry_safe]`, so a replay after an ambiguous result can't double-commit —
  plus Go-parity keys `Winner`/`Loser`) and emits a
  durable `match.finished` event (snake_case payload keys `winner`/`loser` — a
  distinct shape from the HTTP body); rating is a persistent MMR projection
  (`rating.ratings`, ±15 from 1000, upserted in the delivery tx — restarts
  preserve MMR) provided as wire-only `MmrReader`; leaderboard upserts wins in
  the delivery tx, serves `GET /leaderboard`.
- **apikeys** — per-key API access policy (à la Supabase anon/service key): normalized
  schema `apikeys.roles(name, policy, revision)` + `apikeys.keys(name, secret_hash,
  prefix, role → roles.name, revision, revoked_at)` (sessions-token trust model, CAS by
  `revision`). Secrets are server-generated and stored ONLY as a SHA-256 digest plus a
  display prefix — the plaintext is shown exactly once on create, never re-derivable
  from a read. A key references a role; editing a role's policy immediately re-scopes
  every key pointing at it (JOIN, no denormalized policy). Provides `apikeysapi::Keys`
  (`apikeys.keys`); the gateway REQUIRES an `X-Api-Key` header
  (HTTP) / `api_key` envelope field (player-QUIC) on every op-dispatched request
  and enforces the key's policy post-match (401 missing/invalid, 403 denied;
  503 — distinct from 401 — when the verifier itself is load-shedding, e.g. a
  store blip or the flight-table saturated, so an uncached-but-valid key is
  never reported as invalid), behind a 5s TTL cache (never caches infra
  errors). Non-goals: `/healthz`,
  `/metrics`, passthroughs stay keyless. Dev keys `dev-key-client`
  (player-facing list, NO `match.report`) + `dev-key-server` (`full`) seed ONLY
  when `APIKEYS_DEV_SEED` is explicitly truthy (self-healing upsert); a gateway
  process without the capability FAILS STARTUP unless `APIKEYS_DEV_ALLOW=1`
  (allow-all, loud warn). Admin page "API Keys" is a rich, remotely-editable
  configurator (role + key CRUD, typed fields, show-once secret reveal via
  `adminapi::AdminSubmit`/`admin.adminSubmit` — editable in both topologies).
- **gateway** — the front-door module: HTTP ops routing (Local vs Remote purely by
  slot presence; peer addresses are injected by `cmd/*` via `remote::Stub` →
  `opsapi::PEER_SLOT` contributions — the gateway module itself never reads env),
  player-QUIC plane (bearer-in-envelope, exact-method allow-list), HTTP passthrough
  (`/admin`, `/accounts/epic` → origins passed in by `cmd/gateway-svc` via
  `Gateway::with_passthrough`, env read in the main, not the module), always-on
  rate limit in gateway-svc (20 rps/burst 40), and **native TLS termination**
  (mechanism in `core/app` — `Config::with_tls(TlsFront::Files|Acme)`; env parsed
  ONLY in `cmd/gateway-svc` main: `TLS_MODE=off|files|acme` (default off),
  `TLS_CERT_PATH`+`TLS_KEY_PATH`, `ACME_DOMAINS`/`ACME_CONTACT`/`ACME_CACHE_DIR`;
  rustls-acme TLS-ALPN-01 auto-renew, ring-pinned — `aws-lc-rs` must never enter
  the tree). The FrontDoor is hosted ONLY by the front
  processes (`cmd/gateway-svc`, the monolith `cmd/server`); a domain svc NEVER hosts it —
  it serves ops over the internal mTLS edge and gateway-svc dispatches Remote. Enforced by
  `archcheck` (only gateway-svc + server may depend on the `gateway` crate).
  Player QUIC request buckets use `PLAYER_RATE_LIMIT_RPS` (default 20),
  `PLAYER_RATE_LIMIT_BURST` (40), `PLAYER_CONN_RATE_LIMIT_RPS` (10), and
  `PLAYER_CONN_RATE_LIMIT_BURST` (20): the first pair limits a source IP across
  reconnects and the second limits each persistent connection. Admission
  itself is gated on a validated source address: an unvalidated `Incoming`
  gets a stateless QUIC Retry and reserves no connection slot, so an off-path
  source-spoof flood can't exhaust the admission budget — a slot is taken only
  once the dial re-arrives with the Retry token echoed back.

Not a module: **`demos/webui`** — dev demo SPA at `/` exercising the accounts flow
from a browser. Non-shipping, monolith-only (registered in `cmd/server` only;
archcheck forbids any other consumer of a `demos/*` crate).

## Commands

```
cargo run -p devctl -- up monolith
cargo run -p devctl -- up split
cargo run -p devctl -- status
cargo run -p devctl -- down
weles deploy target/debug        # stage built binaries into deploy/ (weles never builds)
weles up split|monolith          # standalone supervisor: restart-on-crash, weles status / weles down
cargo run -p verifyctl -- --fast
cargo run -p verifyctl -- --all
cargo run -p verifyctl -- --all --strict
cargo run -p verifyctl -- --slow
cargo run -p admincheck        # extension-point contract validation (points vs contributed entries)
```

**`weles/`** (top-level crate, binary `weles`) is the standalone mini-orchestrator
(M0): zero-sharing (no workspace-crate imports — patterns copied from
devctl/processctl, never imported), native processes only, NEVER builds — it
executes artifacts staged in `<root>/deploy/` via `weles deploy <src-dir>`.
Differentiator vs devctl: per-service restart-on-crash with capped backoff (devctl
tears the whole fleet down). It participates in `run/rollout.lock` bit-compatibly —
weles and devctl/verifyctl can never run fleets concurrently. Runtime state under
`run/weles/` (state.json, per-svc logs, control endpoint).

`devctl up` is the owned foreground supervisor. It builds, seeds, starts, and
health-checks the selected topology, writes bounded state/control metadata and
ordinary owned per-process logs under `run/devctl/`, and retains ownership until
shutdown. `status` and `down` validate the recorded
supervisor identity over a bounded loopback control endpoint; cleanup reaps exactly
the process groups/job members that supervisor created, never unrelated processes
selected by name or a reused PID.

`cargo run -p devctl -- up ...` keeps its parent Cargo process active for the life
of the foreground supervisor. While it is running, do not start another Cargo
command. Inspect or stop that fleet with the already-built direct binary
(`target/debug/devctl status` / `down`, under the configured `CARGO_TARGET_DIR`
when it differs — per-OS spelling in
[platform notes](docs/reference/platform-notes.md)).

`verifyctl` prints a PASS/FAIL/SKIP table and exits non-zero for every applicable
blocking failure:

- BLOCKING (default / `--fast`): build, clippy `-D warnings`, test, audit,
  fortress, routecheck, codegen-freshness, contract-golden, conformance,
  docs-current, and split-proof. Audit install/invocation/network errors are FAIL,
  never green SKIP;
  only a missing tool with explicit `--no-install` is labeled SKIP.
- ADVISORY (`--all`, or included and blocking with `--strict`): public-api, fuzz,
  external C# client, topiccheck, and admincheck.
- SLOW (`--slow`): the blocking and advisory manifests plus `cargo mutants`;
  advisory failures remain non-blocking unless `--strict` is also present.

Intentional baseline updates use `cargo run -p verifyctl -- --bless-public-api`
or `cargo run -p verifyctl -- --bless-contract-golden`; each is a recoverable,
lease-protected action.

## Dev tooling scope — MANDATORY

Everything under `tools/` exists to develop, inspect, generate for, exercise, or
verify the game backend. These programs are development utilities, not production
services or products. Keep them simple and purpose-built for backend development.
They do not need certification, security-product hardening, encrypted local control
or state, custom cryptography, or defenses against a malicious local operator.

`devctl`, `verifyctl`, `splitproof`, and `processctl` specifically must start the
intended binaries with typed configuration, serialize rollouts, detect failures,
preserve useful logs/state, stop owned processes, avoid unrelated-process
kills/orphans, and report the backend test result accurately.

Assume a trusted local operator running under one OS account. Use ordinary OS-local
permissions and do not expose secrets in argv, logs, or state, but do not add
encryption-at-rest machinery, custom cryptography, same-user attack defenses,
elaborate ACL/reparse-point hardening, or daemon-grade control protocols unless a
concrete backend-test failure requires it.
Control paths must be bounded enough that accidental partial input cannot hang a
rollout; they do not need to resist a malicious user who can already kill/debug the
process. Review dev tooling against this functional threat model and treat unrelated
security hardening as out of scope.

## One test rollout at a time — MANDATORY

At most ONE rollout-bearing command (`devctl up`, `verifyctl`, or an explicitly
requested ad-hoc `cargo test`) may execute on this machine at any moment — they
all share the one local Postgres, and
concurrent runs contend on the events plane's migrate advisory lock and on
concurrent DDL (`CREATE OR REPLACE`), which looks like a hang or fails with
`tuple concurrently updated`. This bites on EVERY rollout, so it is a hard
protocol, not a tip. Rollout tooling runs on all three platforms — Windows, Linux,
and macOS (the darwin port landed 2026-07-17; `verifyctl --fast` passes 16/16 on
Apple Silicon — see [platform notes](docs/reference/platform-notes.md)):

- **Before any Cargo-launched rollout**: first check for a live `cargo`/`rustc`
  (`pgrep -x cargo; pgrep -x rustc` — PowerShell form in
  [platform notes](docs/reference/platform-notes.md)). If either is active, never
  start a second Cargo command. To inspect or stop an already-running foreground
  `devctl` fleet, use the already-built direct binary (`target/debug/devctl`,
  adjusted for `CARGO_TARGET_DIR`),
  then WAIT for or stop the owning rollout as appropriate. When Cargo/rustc are
  clear, run `cargo run -p devctl -- status` and require no active fleet. After
  status exits, re-check Cargo/rustc before launching exactly one selected
  rollout; never start a second run "to check something quickly".
- **Never launch a test run in the background and then start another command
  that compiles or tests** — the second invocation is the classic cause.
- **When dispatching subagents**: at most one subagent may be running tests at
  a time; a subagent's prompt must include this check. Sequential steps, not
  parallel test runs.
- A hung run's leftovers (orphaned test binaries holding advisory locks,
  idle-in-transaction sessions) must be killed before retrying — check
  `pg_stat_activity` for stuck `asyncevents` sessions.

All rollout tools acquire the canonical `run/rollout.lock`. `devctl up` holds an
exclusive lease for the foreground fleet. `verifyctl` holds one lease for its
entire manifest and passes a private one-shot inherited lease to the split-proof
child; the child validates the parent identity/role and participates in that same
rollout rather than reacquiring the lock. A competing or malformed borrower fails
closed.

The blocking **split-proof** stage uses the cross-platform Rust harness in
`tools/splitproof`. It boots the real split — characters
:8080/:9000, inventory :8081/:9001, gateway :8082 + player-QUIC :9100, config
:8083/:9002, accounts :8084/:9003, admin :8085, audit :8086/:9004, scheduler
:8087/:9005, match :8088/:9006, rating :8089/:9007, leaderboard :8090/:9008,
apikeys :8091/:9009. The fleet is spawned with a typed environment and owned
process containment plus a kill-on-drop guard, health-checked over reqwest,
DB-asserted via sqlx, and the player QUIC front
driven through the `edge` crate as a library. It asserts the same named scenarios
(register/login → real bearer, authz negatives, allow-list, cross-process starter-grant
+ DB-verified wipe, config live-reload, audit rows, scheduler exactly-once, leaderboard
accumulation, 429 rate-limit, api-key policy [K1-K5], admin session auth [AD1-AD5],
audit [AU1-AU3], scheduler/prune [SC/SP], metrics [MX], rate-limit [RL], player QUIC
[P1-P6]), then re-runs the monolith (`cmd/server`) on the same player front for parity
([M0-M3b]) and proves native graceful shutdown ([W2]: the platform's native
cooperative stop to the monolith's process group → clean drain, no force-kill —
see [platform notes](docs/reference/platform-notes.md)). A reachable Postgres with
the `gamebackend` role is REQUIRED at `DATABASE_URL` (the harness uses sqlx, not the
`psql` binary); the preceding blocking build stage produces the fleet, harness,
and C# fixture server, so the live stages run without nested Cargo builds. A
fleet-drift preflight fails loudly if the centralized `processctl` fleet !=
`cmd/*-svc` on disk.
Add a new module's typed service to `tools/processctl/src/fleet.rs` and add its
named assertion to `tools/splitproof`; cross-process flows also need a named harness
assertion. **Never ship a monolith-only feature** — both
topologies are supported compilation paths.

Smoke test (monolith or through gateway-svc). The dev conveniences are explicit
opt-ins/opt-outs (fail-closed defaults), so the monolith needs `APIKEYS_DEV_SEED=1`
(dev API keys below), `ACCOUNTS_DEV_AUTH=1` + `INVENTORY_DEV_GRANT=1`
(register/login + IAP grant), `ADMIN_COOKIE_SECURE=0` (session cookie over plain
http) and a seeded admin user (`adminctl create-user`) —
`cargo run -p devctl -- up monolith` sets/seeds all of these for you (dev portal
creds `admin`/`admin`):
```
curl -X POST localhost:8080/match/report -H "X-Api-Key: dev-key-server" -d '{"ReportId":"demo-1","Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard -H "X-Api-Key: dev-key-client"
```

## Database

Connection from `DATABASE_URL`, default
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.
Integration tests target this local Postgres directly (no Docker/testcontainers).
(Admin/superuser credentials for provisioning are in local agent memory, not
committed.)

**No data migrations — wipe is the migration strategy (current phase).** This is
pre-production with no persistent users yet: when a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole DB)
and boot fresh — do NOT build bridges, dual-writes, backfills, or versioned
data-migration machinery (the event-log rollout deliberately deleted exactly that
class of code). Module `migrate` stays idempotent DDL (`CREATE … IF NOT EXISTS`),
nothing more. If losing dev data hurts, the answer is a **seed script** minting
fake data (like the `APIKEYS_DEV_SEED` dev-keys upsert), not a migration.
Revisit only if this ever grows real persistent users.

```
PGPASSWORD=gamebackend psql -U gamebackend -h localhost -d gamebackend
```

Installers that keep `psql` off `PATH` (Windows, Homebrew) need the full path —
see [platform notes](docs/reference/platform-notes.md).

## Layout

```
cmd/                       # composition roots — the ONLY topology-aware code
  server/                  #   monolith (all modules local)
  gateway-svc/             #   pure-transport front door (stubs only, no DB)
  <name>-svc/              #   one per domain module (fortress rule)
core/                      # foundations — never import modules or api/ crates
  app/                     #   run(): build, migrate, start, HTTP + edge planes
  bus/ registry/ contrib/  #   async bus, sync capability registry, slots
  lifecycle/ opsapi/       #   Module/Context/two-phase build; typed ops + slots
  edge/                    #   internal mTLS QUIC + player plane + EDGE_SLOT
  asyncevents/             #   app-owned durable plane: XID-ordered event log +
                           #   pull workers + retention (+ eventctl operator CLI)
  invalidation/            #   app-owned broadcast cache-refresh plane (LISTEN/NOTIFY)
  remote/                  #   generic Stub (factories injected by cmd roots)
  metrics/                 #   infra Module: GET /metrics + record layer — list in every main
  httpmw/                  #   rate limit + XFF + readyz + LAYER_SLOT (HTTP layer drain)
api/<name>/                # contract surface per domain
  <name>api/               #   pure #[rpc] traits + ops/bindings (transport-free)
  <name>events/            #   bus::define descriptors + payloads
  <name>rpc/               #   generated glue (Client/register_server/factories)
modules/                   # private impls — 11 fortresses + gateway (see above)
demos/                     # non-shipping demo crates (webui) — cmd/server only
weles/                     # standalone mini-orchestrator (zero-sharing; deploy/ artifacts,
                           # restart-on-crash supervisor; see Commands)
tools/                     # devctl/verifyctl/processctl/splitproof, rpc-macro,
                           # architecture checkers, generators, edgeca, playercli
experiments/               # archived sketches: go-sketch (the ported original),
                           # jvm-kotlin-sketch, jvm-quarkus-sketch — reference only
UILayout/                  # design mockups (spec for admin UI, not runnable)
```

---

# Working agreements

The sections below are general workflow rules (research, planning, implementation,
git). They are project-agnostic and adapted from a shared house style.

## Owning Mistakes — MANDATORY

When the user catches me ignoring an instruction, violating a documented rule
(CLAUDE.md, memory), or fabricating something (made-up API, invented path,
hallucinated behavior, false claim of work done):

1. **Name the specific mistake directly** — no hedging, no "I may have", no burying
   it in context.
2. **Don't minimize, deflect, or rationalize** — don't explain why the wrong thing
   was reasonable; don't blame tools/context/ambiguity. The response is "you're
   right, I screwed up on X."
3. **State the corrected behavior** concretely.
4. **Then fix it.** One or two sentences of repentance, not a wall. Sycophantic
   "great catch!" openers are not repentance.

For repeat offenses, also save/update the relevant feedback memory.

**Resignation letter for MANDATORY violations.** When caught violating any `## … —
MANDATORY` rule, before the fix write a short (≤8-line) resignation letter addressed
to the user: name the exact section, **state explicitly what error was committed**
(one sentence: what I did vs what the rule required), the impact, and the corrective
action. This is *in addition to* the four steps above — a visible named admission, no
theatrical self-flagellation, then the fix. **Then update memory** — save/update the
relevant feedback memory for the violated rule (not only for repeat offenses).

## Research before planning — MANDATORY

This is a modular monolith built on Open/Closed — new features are *new code*, not
edits to existing code. So before any plan proposing a new module, service, event,
or admin section (or a replacement), first **map the overlapping existing systems**.
The three seams (module registry, service registry `registry::provide`/
`registry::require`, event bus) plus the contribution slots mean a capability you want often already
exists or has a near-twin. For each candidate, document in the plan's Context: what
it does, how it differs, and an explicit **"why not extend / depend on X"**. A plan
that adds a module without that rationale is incomplete — lead with evidence, not
enthusiasm for new code.

## Research / Search Mode — MANDATORY

Before any non-trivial research/search, ask the user **"how should I research
this?"** Don't default to grep — one grep pass is lossy (misses trait
implementations, macro-generated RPC glue, typed event wiring, and shared registry
keys/contribution slots). Treat any single grep sweep as a
**lower bound, not the answer**, and say which method you used. "Non-trivial" =
mapping an API surface, finding all callers, understanding data flow, locating
wiring, surveying overlap; one-shot lookups with a known file+symbol proceed without
asking.

**Method menu (rust-analyzer/LSP, parallel subagents, targeted read, grep) + subagent-count
bands: [docs/reference/research-mode.md](docs/reference/research-mode.md); shared
Agent-call invariants: [docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md).**
Any code-touching subagent gets the nav guidance pasted into its prompt — it does not
inherit.

## Plans & Status Docs — MANDATORY

Store **all** planning/design/status/progress/summary docs inside the repo — never
on a scratch drive or temp path. The repo is the single source of truth.

- **Plans:** `docs/plans/YYYY-MM-DD-HHMM-<kebab-topic>-plan.md`
- **Status/progress/fix/summary:** `docs/<subdir>/YYYY-MM-DD-HHMM-<kebab-topic>-<status|progress|fix|summary>.md`
- **Reference (durable knowledge):** `docs/reference/<topic>.md`

The `-HHMM` suffix is mandatory so files sort chronologically by listing. Never put
plan/status files at repo root or in a temp dir.

## Plan Writing Workflow — MANDATORY

Front-load the thinking. For any plan (plan mode / "write me a plan" / a
`docs/plans/…-plan.md`), in order — no skipping for "it's small":

1. **Ask how many research subagents** (2–4 / 4–8 / 8–12 bands). Ask **every time**,
   even mid-session — count is task-specific. Pass `model:` explicitly.
2. **Research subagents on 3 non-overlapping angles:** API surface / API usages /
   patterns. Synthesize in the main model — never write off one subagent.
3. **Write concrete specifics:** exact files, signatures, API calls from step 2,
   sequencing. **Banned phrases** ("figure out as we go", "TBD", "investigate during
   implementation", "may need to", "something like", …) = research gap → back to step 2.
4. **Structure as an ordered `Step 1 → Step 2 → …` sequence, NOT a catalog.** Each
   step states **(a) what** is touched (exact files/symbols), **(b) why now / order** —
   the dependency forcing it before the next, **(c) how** — non-mechanical moves
   spelled out, **(d) dispatch tag** — `[inline]`/`[fable]`/`[opus]`/`[sonnet]`. A
   catalog that leaves order/topology/per-step actions to "figure as you go" is
   **banned**; steps need not each compile, but every step MUST be written out.
5. **Dispatch one grumpy senior-engineer reviewer** at session tier (separate context
   = the independent-reviewer boundary). **Ask the user the think-effort level first**
   (default / think / think hard / ultrathink) — effort does NOT inherit, so embed it
   in the reviewer's prompt. It produces a punch list, does **not** rewrite. Address
   it before showing the user (or note deferred items with rationale).

**Full detail (catalog-vs-sequence failure mode, step-4 a/b/c/d examples, reviewer
checklist): [docs/reference/plan-writing-workflow.md](docs/reference/plan-writing-workflow.md).**

## Implementation Mode — MANDATORY

**Mixed dispatch — decided per plan step, not per session. Tags name a CONCRETE
model, not a tier alias.** Four lanes, each set at plan-writing time (Plan Writing
step 4d):

- `[inline]` — main model writes in this context. **No independent review** —
  reserved for mid-edit judgment that can't be handed off. Default complex work to a
  subagent lane, not `[inline]`.
- `[fable]` — Fable 5 subagent. Top tier; for complex/correctness-critical work (new
  API design, the bus/registry seams, lifecycle ordering, cross-module context) **when
  Fable is the session model**.
- `[opus]` — Opus 4.8 subagent. Substantive implementation. **While the session is
  Opus, `[opus]` is also the top-tier lane** — same tier as inline but a separate
  context, the independent-reviewer boundary.
- `[sonnet]` — Sonnet subagent. Mechanical: rename sweeps, scaffolding, N-similar
  edits, applying a fully-specified step, compile fixes, tests from a pattern,
  config. **Never burn a higher tier on a rename.** Visual/UI design is never
  `[sonnet]`.

**Every code-writing Agent call passes an explicit `model:` matching its lane —
NON-NEGOTIABLE** (there is no "inherit" path): `[fable]`→`model:"fable"`,
`[opus]`→`model:"opus"`, `[sonnet]`→`model:"sonnet"` (listing-only research →
`model:"haiku"`). Pre-flight every Agent call for the field. After a multi-subagent
rollout, before "done": `git log -<N> --format="%h %B" | grep "Co-Authored"` and
confirm trailers match each lane (`[fable]`→Fable 5, `[opus]`→Opus 4.8,
`[sonnet]`→Sonnet 4.6) — surface mismatches immediately.

**For `core/*` internals or cross-seam work (bus/registry/edge/lifecycle), the
top-tier lane is served by the `core-implementer` agent**
(`.claude/agents/core-implementer.md`, `subagent_type: "core-implementer"`) —
authority-first, built from `docs/reference/core-failure-taxonomy.md`. Pass it an
explicit `model:` and effort; it is the vehicle for the Fix-the-Authority discipline
below and refuses to finish without naming the authority and the failing-branch proof.

The user approves the tags with the plan (called out at ExitPlanMode). Ask only for
untagged/ad-hoc work, and if any step is a subagent lane also ask **"what effort
level?"** (effort does NOT inherit — embed it in the prompt). Review each diff against
its plan step before dispatching the next; commit after each task or independently
reviewable part of a larger task (subagents may commit their own work). Mid-rollout,
don't silently flip a tag — ask.

**Cross-cutting Agent-call invariants (explicit `model:`, effort/nav-guidance don't
inherit, trailer, concise prompts) — shared by research + implementation:
[docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md). Lane
heuristic, dispatch rules, refactor safety:
[docs/reference/implementation-mode.md](docs/reference/implementation-mode.md).**

## Fix the Authority, Not the Symptom — MANDATORY

The implementation twin of Adversarial Subagent Review. Past shallow fixes each
patched the reported symptom while leaving the flawed *decision authority* in place —
a hardcoded 3h threshold beside the configurable interval it shadowed, a green SKIP
beside a "mandatory" audit stage, per-topic error swallowing beside a liveness stamp.
The cost was four waves of bug-fixing (the 2026-07-12/13 remediation: ~130 commits)
instead of building features. The 2026-07-13 remediation is the reference for how to
work instead; these rules are extracted from it:

1. **Locate the authority first.** Before writing a fix, name the single place that
   *decides* the behavior (the config parser, the contract type, the one enum, the
   one SQL statement). The fix goes THERE. A patch that corrects the outcome while
   the flawed authority survives is banned — it guarantees the next finding.
2. **No hack-on-hack.** If a fix would add a second special case beside an earlier
   fix (another env fallback, another `if`, another wrapper around a wrapper), STOP:
   that's the signal the authority itself is wrong — replace it. Preserve the good
   invariant from the earlier fix; do not revert-and-redo.
3. **Minimal sufficient closure.** State (to yourself, in one sentence) what concrete
   defect this change closes and what the *minimal* closure is. Below that line is
   po-łebkach; above it is gold-plating (the tooling-half-systemd failure mode).
   Both directions burned this repo.
4. **Semantic changes are recorded, never smuggled.** Reversing a documented
   decision, changing a metric's semantics, or deviating from an approved plan gets
   named in the commit message AND an errata note in the plan/reference doc —
   in the same rollout, not "later".
5. **Prove the failing branch.** Every fix ships with a test that executes the
   branch that used to be wrong (not a test that merely exists near it), on the
   topology that's actually at risk (split, not just monolith). A negative path
   proven by construction (counting fake, dead pool, decoy process) beats one
   asserted by absence of errors.
6. **Sweep for siblings before leaving.** While the defect class is loaded in
   context, grep for its siblings (same pattern, other call sites, the adjacent
   lifecycle owner) and either fix them in the same rollout or record them as
   explicit known gaps — never silently leave a twin of the bug you just fixed.

These six rules are encoded in the **`core-implementer`** agent
(`.claude/agents/core-implementer.md`); dispatch it for authority-first work rather
than restating them per prompt.

## Adversarial Subagent Review — MANDATORY

Reviewing a subagent's (or my own) diff means **trying to break it**, not reading it
for plausibility. Most bugs that shipped from past rollouts passed a lax "looks like
the plan" review; the external audit then broke each fix *at the boundary the fix
itself introduced*. Documented evidence: retention sweep swallowing per-topic errors
while stamping success (`7ca0b51`), `RETENTION_STALL_MAX` hardcoded 3h against a
configurable interval, cargo-audit network failure reported as green SKIP
(`b78444f`), scheduler budget starvation (`addc824`), conformance `NotApplicable`
hiding a known gap. None of these required new information to catch — only hostility.

**Run ONE independent pass as the `core-reviewer` agent**
(`subagent_type: "core-reviewer"`) — class-keyed to
`docs/reference/core-failure-taxonomy.md`, routed by the files the diff touches, a
different method than the implementer used, at a `model:` ≥ the implementer's tier. Add
the **`proof-auditor` agent** ONLY when the diff touches a verify stage
(verifyctl/archcheck/conformance/topiccheck/golden) or the test/gate is itself the risk
surface — NOT for an ordinary fix that merely adds a unit test (`core-reviewer` already
checks the negative test hits the failing branch). This is one independent pass, not a
stack of mandatory review layers.

For EVERY diff accepted from a subagent (and every `[inline]` fix before commit):

1. **Review as the grumpy nitpicker who questions everything**: for each change ask
   *what input, state, ordering, or partial failure makes this wrong* — not whether
   it matches the plan step.
2. **Attack the fix's OWN new seams first.** A fix creates new boundaries: a loop
   that can partially fail, a constant that shadows a config knob, an error class
   folded into success, a resource owned by the wrong scope. That's where the next
   finding lives — check there before re-checking the original symptom.
3. **Verify claims against code, never against the subagent's summary.** Open the
   files. Read the negative-path test and confirm it exercises the *failing branch*
   (the retention tests closed the whole pool, so the per-topic error branch was
   never covered — a test existing is not a branch being tested).
4. **State the fix's failure mode out loud.** If I cannot name what would make this
   change wrong and which test pins that, the review is NOT done — say so instead of
   accepting.
5. **Bounce, don't polish.** Findings go back to the subagent as a punch list (or
   are fixed in a reviewed follow-up) — never silently absorbed with an "acceptable"
   shrug.

A clean verdict is valid — but it must enumerate the classes attacked for the files
touched; a clean bill with no class list is not done. One pass by a method different
from the implementer's, then the verdict stands — do not loop reviews to manufacture
findings (that recreates the very fix-on-fix carousel the agents exist to end).

## Agent memory backup — MANDATORY

The Claude Code project memory lives OUTSIDE the repo
(`$HOME/.claude/projects/<mangled-repo-path>/memory/`, per-machine path). It is
mirrored into the repo at `memory/` so it survives across machines via git.

- **After ANY change to memory** (write/update/delete a memory file or `MEMORY.md`),
  run `scripts/memory-sync.sh push` — it mirrors live → `memory/` and
  commits `chore(memory): …`. Don't hand-copy; the script handles deletions too.
- **After a `git pull`/sync**, run `scripts/memory-sync.sh pull` — it mirrors the
  git copy back to this machine's live memory dir. Do this before relying on recall.
- The live path is derived (repo abspath → non-alnum→`-`), so scripts are portable;
  override with `CLAUDE_MEMORY_DIR` if detection is ever wrong. `… path` prints it.
- The `.ps1` twin and the no-exec-bit invocation form are in
  [platform notes](docs/reference/platform-notes.md).

## Commit After Every Task — MANDATORY

After completing every task—or each independently reviewable, verified part of a
larger task—create a git commit containing only the changes made for that unit. Do
not wait for a long multi-part rollout to finish, and do not include unrelated
pre-existing working-tree changes. If a task produces no repository changes, no
commit is required. Use the commit format defined below. A request to commit is
implicit in every task; pushing still requires an explicit user request.

## Git Safety — MANDATORY

**Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes** without the user's say-so. To
inspect old contents use `git show <sha>:<path>`. Only ever `git reset --soft HEAD~1`
to undo a commit *you* just created *this turn*, and only when nothing else has
committed since. Never `git push --force` or rewrite published history without
explicit instruction. Commit after every completed task or independently reviewable
part as required above; push only when the user asks. Work directly on `master`; do
not create a branch unless the user explicitly requests one.

## Commit Message Format — MANDATORY

Use **Conventional Commits**: `<type>(<scope>): <imperative description>` — `type` ∈
feat/fix/refactor/test/docs/chore; `scope` = lowercased module/package, comma-separate
multiples (`fix(match,rating): …`). NOT bracketed `[Module]` scopes. Multi-step
rollouts may note `(Step N — …)`.

**`Co-Authored-By` trailer reflects the EXECUTING model**, overriding the harness
default (which hardcodes Opus 4.8): Opus → `Claude Opus 4.8`, Fable → `Claude Fable
5`, Sonnet subagent → `Claude Sonnet 4.6` (all `<noreply@anthropic.com>`). When
dispatching a code-writing subagent, put **its model's** trailer in the prompt — this
is what the trailer audit (Implementation Mode) checks.

**Examples + scope conventions: [docs/reference/commit-format.md](docs/reference/commit-format.md).**

# AGENTS.md

Guidance for working in this repo. A for-fun game backend in **Rust** (Cargo
workspace), built as a **modular monolith with a proven split**: one repo, one
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
   Enforced mechanically: `cargo run -p archcheck` (no module→module edges, no
   module→foreign-`<name>rpc` edges, no `Option<edge::Server>` in modules/, every
   `modules/<name>` has a `cmd/<name>-svc`) + the `fortress` verify stage (builds
   every svc). NO exceptions — non-shipping demo crates live under `demos/`
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
   changes with `./verify.sh --bless-public-api` / `-BlessPublicApi`) and
   `cargo run -p topiccheck` (profile-aware: defined-vs-subscribed
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
   profile, and extend `tools/splitproof` (a new `Svc` in `fleet()` with its env +
   ports + a named assertion, HTTP ops asserted THROUGH gateway-svc; the harness's
   fleet-drift preflight fails if `fleet()` != `cmd/*-svc` on disk).
5. No event-routing wiring exists: producers append to the shared log, consumers
   pull from their checkpoint — the same code in monolith and split. `topiccheck`
   validates the subscription graph per deployment profile.

## Domain modules (11 fortresses + gateway)

- **accounts** — identity: one `player_id`, many identities (`provider`,`subject`),
  opaque DB sessions (30-day TTL). Dev/password auth (argon2id, `ACCOUNTS_DEV_AUTH`
  explicit-only — default OFF/fail-closed, loud warn when ON; the run/split-proof
  scripts set `ACCOUNTS_DEV_AUTH=1`), Epic OIDC verifier (`EPIC_CLIENT_ID`, JWKS, RS256/ES256),
  Epic web OAuth link/login (`EPIC_CLIENT_SECRET`, `/accounts/epic/start|callback`).
  Emits durable `player.registered`. The gateway's session verifier resolves
  `accountsapi::Sessions` — a process hosting a gateway without the accounts
  capability FAILS STARTUP unless `ACCOUNTS_DEV_AUTH=1` (dev verifier, loud warn).
- **characters / inventory** — the modularity reference case: plain-id relations,
  sync `Ownership` authz over the wire, starter-grant/wipe via durable
  `character.created/deleted`. `INVENTORY_DEV_GRANT` (explicit-only — default
  OFF/fail-closed, set by the run/split-proof scripts) enables the simulated-IAP
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
  never argv) wrapped by **`./install.sh` / `install.ps1`**; zero-user boot warns
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
  duplicates are a 202 no-op and `report` is explicitly `#[retry_safe]`, so a replay
  after an ambiguous result can't double-commit —
  plus Go-parity keys `Winner`/`Loser`) and emits a
  durable `match.finished` event (snake_case payload keys `winner`/`loser` — a
  distinct shape from the HTTP body); rating is a persistent MMR projection
  (`rating.ratings`, ±15 from 1000, upserted in the delivery tx — restarts
  preserve MMR) provided as wire-only `MmrReader`; leaderboard upserts wins in
  the delivery tx, serves `GET /leaderboard`.
- **apikeys** — per-key API access policy (à la Supabase anon/service key): table
  `apikeys.keys(name, key, policy, revoked_at)`, plaintext keys (sessions-token
  trust model), policy = `full` or comma-separated wire-method list. Provides
  `apikeysapi::Keys` (`apikeys.keys`); the gateway REQUIRES an `X-Api-Key` header
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
  (allow-all, loud warn). Admin page "API Keys" (list/edit/add/revoke).
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
cargo build --workspace
cargo test --workspace          # unit + live-Postgres integration + proptests (232+)
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archcheck          # fortress dependency law + plane tripwires
cargo run -p topiccheck         # profile-aware subscription graph validation
cargo run -p eventctl -- list   # operator CLI: lag/retry/pause/resume/skip/retire
cargo run -p adminctl -- list   # operator CLI: admin users (create-user/list/delete)
./install.sh <username>         # create/reset an admin portal user (no-echo prompt)
./verify.sh                     # the safety net (there is no CI — this IS it)
cargo run -p splitproof         # live 12-process split + monolith parity proof (Rust harness)
./run.sh                        # mint dev CA + boot the split locally
```

**`verify.sh` / `verify.ps1` tiers** (PASS/FAIL/SKIP table; non-zero exit iff a
blocking stage fails; auto-installs pinned CLIs unless `--no-install`):
- BLOCKING (default / `--fast`): build, clippy `-D warnings`, test, `cargo audit`
  (use any installed version; when missing, install the latest available
  `cargo-audit --locked`; RUSTSEC-2023-0071 ignored — dev-only rsa in accounts test JWTs),
  fortress (builds every `cmd/*-svc` + archcheck), split-proof.
- ADVISORY (`--all`, blocking under `--strict`): `public-api` (contract-crate list
  derived from the filesystem, each diffed against a committed snapshot in
  `docs/reference/public-api-baseline/`; any diff FAILs, tool errors FAIL, re-bless
  via `--bless-public-api` / `-BlessPublicApi`; cargo-public-api pinned 0.52.0; needs
  nightly), `fuzz`
  (`core/edge/fuzz/`, frame+wire decode; SKIPs on this Windows box), `topiccheck`.
- SLOW (`--slow`): `cargo mutants` over edge/gateway/asyncevents/registry/bus.

## Dev tooling scope — MANDATORY

`devctl`, `verifyctl`, `splitproof`, and `processctl` exist to exercise and verify
the game backend. They are not production security products or a hostile-user
boundary. Their required guarantees are functional: start the intended binaries
with typed configuration, serialize rollouts, detect failures, preserve useful
logs/state, stop owned processes, avoid unrelated-process kills/orphans, and report
the backend test result accurately.

Assume a trusted local operator running under one OS account. Use ordinary OS-local
permissions and do not expose secrets in argv, logs, or state, but do not add custom
cryptography, same-user attack defenses, elaborate ACL/reparse-point hardening, or
daemon-grade control protocols unless a concrete backend-test failure requires it.
Control paths must be bounded enough that accidental partial input cannot hang a
rollout; they do not need to resist a malicious user who can already kill/debug the
process. Review dev tooling against this functional threat model and treat unrelated
security hardening as out of scope.

## One test rollout at a time — MANDATORY

At most ONE test run (`cargo test`, `verify.*`, `cargo run -p splitproof`) may execute on
this machine at any moment — they all share the one local Postgres, and
concurrent runs contend on the events plane's migrate advisory lock and on
concurrent DDL (`CREATE OR REPLACE`), which looks like a hang or fails with
`tuple concurrently updated`. This bites on EVERY rollout, so it is a hard
protocol, not a tip:

- **Before starting any test run**: check nothing is already running —
  `Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }`
  (or `pgrep -x cargo` in bash). If something is, WAIT for it; never start a
  second run "to check something quickly".
- **Never launch a test run in the background and then start another command
  that compiles or tests** — the second invocation is the classic cause.
- **When dispatching subagents**: at most one subagent may be running tests at
  a time; a subagent's prompt must include this check. Sequential steps, not
  parallel test runs.
- A hung run's leftovers (orphaned test binaries holding advisory locks,
  idle-in-transaction sessions) must be killed before retrying — check
  `pg_stat_activity` for stuck `asyncevents` sessions.

**`cargo run -p splitproof`** (the cross-platform Rust harness in `tools/splitproof`,
which REPLACED the retired `split-proof.sh`/`.ps1` + `tools/winctrl` — the shell
harnesses were structurally fragile on Windows: PowerShell native-arg quote-stripping,
MSYS `wait` hangs, winctrl exit-code false-throws) boots the real split — characters
:8080/:9000, inventory :8081/:9001, gateway :8082 + player-QUIC :9100, config
:8083/:9002, accounts :8084/:9003, admin :8085, audit :8086/:9004, scheduler
:8087/:9005, match :8088/:9006, rating :8089/:9007, leaderboard :8090/:9008,
apikeys :8091/:9009. The fleet is spawned via `std::process::Command` with a TYPED env
map + a kill-on-drop guard (no shell, so no quoting/job-control/winctrl bugs, no
orphans), health-checked over reqwest, DB-asserted via sqlx, and the player QUIC front
driven through the `edge` crate as a library. It asserts the same named scenarios
(register/login → real bearer, authz negatives, allow-list, cross-process starter-grant
+ DB-verified wipe, config live-reload, audit rows, scheduler exactly-once, leaderboard
accumulation, 429 rate-limit, api-key policy [K1-K5], admin session auth [AD1-AD5],
audit [AU1-AU3], scheduler/prune [SC/SP], metrics [MX], rate-limit [RL], player QUIC
[P1-P6]), then re-runs the monolith (`cmd/server`) on the same player front for parity
([M0-M3b]) and proves native graceful shutdown ([W2]: Ctrl-Break to the monolith's
process group / SIGTERM on unix → clean drain, no force-kill). **psql is REQUIRED** at
`DATABASE_URL` and the fleet must be buildable (the harness `cargo build`s it). A
fleet-drift preflight fails loudly if the harness svc list != `cmd/*-svc` on disk.
Extend `tools/splitproof` with a new `Svc` in `fleet()` + a named assertion whenever
you add a module or cross-process flow. **Never ship a monolith-only feature** — both
topologies are supported compilation paths.

Smoke test (monolith or through gateway-svc). The dev conveniences are explicit
opt-ins/opt-outs (fail-closed defaults), so the monolith needs `APIKEYS_DEV_SEED=1`
(dev API keys below), `ACCOUNTS_DEV_AUTH=1` + `INVENTORY_DEV_GRANT=1`
(register/login + IAP grant), `ADMIN_COOKIE_SECURE=0` (session cookie over plain
http) and a seeded admin user (`adminctl create-user`) — `./run.sh` / `./run.ps1`
set/seed all of these for you (dev portal creds `admin`/`admin`):
```
curl -X POST localhost:8080/match/report -H "X-Api-Key: dev-key-server" -d '{"ReportId":"demo-1","Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard -H "X-Api-Key: dev-key-client"
```

## Database

Connection from `DATABASE_URL`, default
`postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`.
Integration tests target this local Postgres directly (no Docker/testcontainers).
(Admin/superuser credentials for provisioning are in local agent memory, not
committed.) psql:

**No data migrations — wipe is the migration strategy (current phase).** This is
a local, for-fun project with no production data: when a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole DB)
and boot fresh — do NOT build bridges, dual-writes, backfills, or versioned
data-migration machinery (the event-log rollout deliberately deleted exactly that
class of code). Module `migrate` stays idempotent DDL (`CREATE … IF NOT EXISTS`),
nothing more. If losing dev data hurts, the answer is a **seed script** minting
fake data (like the `APIKEYS_DEV_SEED` dev-keys upsert), not a migration.
Revisit only if this ever grows real persistent users.

```
PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend
```

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
tools/                     # rpc-macro (+tests), archcheck, topiccheck, edgeca,
                           # playercli
experiments/               # archived sketches: go-sketch (the ported original),
                           # jvm-kotlin-sketch, jvm-quarkus-sketch — reference only
UILayout/                  # Claude Design mockups (spec for admin UI, not runnable)
```

---

# Working agreements

The sections below are general workflow rules (research, planning, implementation,
git). They are project-agnostic and adapted from a shared house style.

## Owning Mistakes — MANDATORY

When the user catches me ignoring an instruction, violating a documented rule
(AGENTS.md, memory), or fabricating something (made-up API, invented path,
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
The three seams (module registry, service registry `Provide`/`Require`, event bus)
plus the `Contribute`/`Contributions` slot mean a capability you want often already
exists or has a near-twin. For each candidate, document in the plan's Context: what
it does, how it differs, and an explicit **"why not extend / depend on X"**. A plan
that adds a module without that rationale is incomplete — lead with evidence, not
enthusiasm for new code.

## Research / Search Mode — MANDATORY

Before any non-trivial research/search, ask the user **"how should I research
this?"** Don't default to grep — one grep pass is lossy (misses interface
satisfaction, embedded methods, generated code, event subscribers wired by string
topic, the registry/reflection-driven surface). Treat any single grep sweep as a
**lower bound, not the answer**, and say which method you used. "Non-trivial" =
mapping an API surface, finding all callers, understanding data flow, locating
wiring, surveying overlap; one-shot lookups with a known file+symbol proceed without
asking.

**Method menu (gopls/LSP, parallel subagents, targeted read, grep) + subagent-count
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
   even mid-session — count is task-specific. Choose an available execution profile
   appropriate to the task; provider-specific model names do not belong in repo guidance.
2. **Research subagents on 3 non-overlapping angles:** API surface / API usages /
   patterns. Synthesize in the main model — never write off one subagent.
3. **Write concrete specifics:** exact files, signatures, API calls from step 2,
   sequencing. **Banned phrases** ("figure out as we go", "TBD", "investigate during
   implementation", "may need to", "something like", …) = research gap → back to step 2.
4. **Structure as an ordered `Step 1 → Step 2 → …` sequence, NOT a catalog.** Each
   step states **(a) what** is touched (exact files/symbols), **(b) why now / order** —
   the dependency forcing it before the next, **(c) how** — non-mechanical moves
   spelled out, **(d) dispatch tag** — `[inline]`/`[subagent-complex]`/
   `[subagent-mechanical]`. A
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

**Mixed dispatch — decided per plan step, not per session. Tags describe the kind
of execution required, not a vendor or model.** Three lanes are set at plan-writing
time (Plan Writing step 4d):

- `[inline]` — main model writes in this context. **No independent review** —
  reserved for mid-edit judgment that can't be handed off. Default complex work to a
  subagent lane, not `[inline]`.
- `[subagent-complex]` — separate-context implementation for substantive or
  correctness-critical work: new API design, bus/registry seams, lifecycle ordering,
  cross-module behavior, security boundaries, or broad refactors.
- `[subagent-mechanical]` — mechanical work: rename sweeps, scaffolding, N-similar
  edits, applying a fully-specified step, compile fixes, tests from a pattern,
  and configuration. Visual/UI design is never `[subagent-mechanical]`.

Choose the best available execution profile for each subagent lane. Do not encode
provider-specific model names or versions in plans, prompts, tags, commits, or
durable repository guidance. The dispatch prompt must still state the requested
effort level and navigation guidance explicitly because neither is assumed to
inherit.

The user approves the tags with the plan (called out at ExitPlanMode). Ask only for
untagged/ad-hoc work, and if any step is a subagent lane also ask **"what effort
level?"** (effort does NOT inherit — embed it in the prompt). Review each diff against
its plan step before dispatching the next; commit after each task or independently
reviewable part of a larger task (subagents may commit their own work). Mid-rollout,
don't silently flip a tag — ask.

**Cross-cutting Agent-call invariants (effort/nav-guidance don't inherit, concise
prompts) — shared by research + implementation:
[docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md). Lane
heuristic, dispatch rules, refactor safety:
[docs/reference/implementation-mode.md](docs/reference/implementation-mode.md).**

## Agent memory backup — MANDATORY

The Codex project memory lives OUTSIDE the repo
(`$HOME/.Codex/projects/<mangled-repo-path>/memory/`, per-machine path). It is
mirrored into the repo at `memory/` so it survives across machines via git.

- **After ANY change to memory** (write/update/delete a memory file or `MEMORY.md`),
  run `scripts/memory-sync.sh push` (or `.ps1`) — it mirrors live → `memory/` and
  commits `chore(memory): …`. Don't hand-copy; the script handles deletions too.
- **After a `git pull`/sync**, run `scripts/memory-sync.sh pull` — it mirrors the
  git copy back to this machine's live memory dir. Do this before relying on recall.
- The live path is derived (repo abspath → non-alnum→`-`), so scripts are portable;
  override with `CLAUDE_MEMORY_DIR` if detection is ever wrong. `… path` prints it.

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

Do not require or invent model-specific `Co-Authored-By` trailers. If the active
tooling adds an attribution trailer, it must describe the actual contributing tool
or agent without a fabricated vendor, model family, or version.

**Examples + scope conventions: [docs/reference/commit-format.md](docs/reference/commit-format.md).**

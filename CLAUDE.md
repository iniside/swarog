# CLAUDE.md

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
module never knows the topology.

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
   completes every callback's first refresh or startup fails), delivery halts
   before any module stops, and BOTH QUIC planes drain in-flight handlers before
   modules stop (`RunningServer::shutdown`, `EDGE_DRAIN_GRACE_MS` default 5000 —
   read in `core/app`, never in modules), and the HTTP graceful drain is itself
   time-bounded (`HTTP_DRAIN_GRACE_MS` default 5000 — read in `core/app`, never in
   modules) so a hung connection can't stall shutdown before teardown begins, and
   each module's `stop` (in both ordered teardown and the start-unwind) is itself
   bounded (`MODULE_STOP_GRACE_MS` default 5000 — read in `core/app`, never in
   modules) so one hung module can't stall the rest. A
   failed startup unwinds what started, in reverse, through the same teardown.
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
   profile, extend `split-proof.sh`/`.ps1` (new process + a named assertion, HTTP
   ops asserted THROUGH gateway-svc) and the `fortress` stage port list.
5. No event-routing wiring exists: producers append to the shared log, consumers
   pull from their checkpoint — the same code in monolith and split. `topiccheck`
   validates the subscription graph per deployment profile.

## Domain modules (12 fortresses + gateway)

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
  write emits identically to a service write. Snapshot = `{revision, settings}`
  in one statement. Local `Service` and remote `CachedConfig` (via `configrpc`)
  are invalidation callbacks (atomic map swap, apply only newer revisions);
  `CachedConfig` keeps boot-fill-or-fail-startup.
- **admin** — GameOps portal at `/admin` (minijinja over the embedded Go-era theme).
  **Session auth** (owns schema `admin`: users/sessions/login_attempts): argon2id
  passwords, opaque token + per-session CSRF in an `HttpOnly`/`SameSite=Strict`/
  `Path=/admin` cookie (`Secure` unless `ADMIN_COOKIE_SECURE=0` — dev opt-out, loud
  warn), 12h TTL; asymmetric lockout (user locks at 5 fails, IP at 20,
  `least(2^fails,900)s` backoff, trusted-proxy client IP via `TRUSTED_PROXY_CIDRS`);
  one generic 401 for wrong-pass/unknown-user/locked (no username oracle); CSRF
  checked BEFORE the local/remote editability decision; security headers on the
  admin router only. Admin users are created by **`cargo run -p adminctl`**
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
  all 7 topics — seven independent subscriptions (`audit.<topic-kebab>.v1`), each
  with its own checkpoint — prune reacting to `scheduler.fired{audit-prune}`
  (`AUDIT_RETENTION_DAYS`, default 30).
- **scheduler** — data-driven schedules (`scheduler.schedules`), 1s tick, per-name
  `pg_try_advisory_lock` + still-due re-check + `UPDATE`+`emit_tx` in one tx,
  commit-before-unlock. `SCHEDULER_ENABLED`.
- **match / rating / leaderboard** — match records `match.matches` from a
  `/match/report` HTTP request body (Go-parity keys `Winner`/`Loser`) and emits a
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
  and enforces the key's policy post-match (401 missing/invalid, 403 denied),
  behind a 5s TTL cache (never caches infra errors). Non-goals: `/healthz`,
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
./split-proof.sh                # live 12-process split + monolith parity proof
./run.sh                        # mint dev CA + boot the split locally
```

**`verify.sh` / `verify.ps1` tiers** (PASS/FAIL/SKIP table; non-zero exit iff a
blocking stage fails; auto-installs pinned CLIs unless `--no-install`):
- BLOCKING (default / `--fast`): build, clippy `-D warnings`, test, `cargo audit`
  (pinned 0.22.2; RUSTSEC-2023-0071 ignored — dev-only rsa in accounts test JWTs),
  fortress (builds every `cmd/*-svc` + archcheck), split-proof.
- ADVISORY (`--all`, blocking under `--strict`): `public-api` (contract-crate list
  derived from the filesystem, each diffed against a committed snapshot in
  `docs/reference/public-api-baseline/`; any diff FAILs, tool errors FAIL, re-bless
  via `--bless-public-api` / `-BlessPublicApi`; cargo-public-api pinned 0.52.0; needs
  nightly), `fuzz`
  (`core/edge/fuzz/`, frame+wire decode; SKIPs on this Windows box), `topiccheck`.
- SLOW (`--slow`): `cargo mutants` over edge/gateway/asyncevents/registry/bus.

## One test rollout at a time — MANDATORY

At most ONE test run (`cargo test`, `verify.*`, `split-proof.*`) may execute on
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

**`split-proof.sh` / `.ps1`** boots the real split — characters :8080/:9000,
inventory :8081/:9001, gateway :8082 + player-QUIC :9100, config :8083/:9002,
accounts :8084/:9003, admin :8085, audit :8086/:9004, scheduler :8087/:9005,
match :8088/:9006, rating :8089/:9007, leaderboard :8090/:9008,
apikeys :8091/:9009 — and asserts named
scenarios (register/login → real bearer, authz negatives, allow-list, cross-process
starter-grant + DB-verified wipe, config live-reload, audit rows, scheduler
exactly-once, leaderboard accumulation, 429 rate-limit, api-key policy
[K1-K4]: 401 no/bad key, 403 client-key on match.report, 202 server-key; admin
session auth [AD1-AD5]: login redirect, asymmetric lockout via DB rows, session
cookie + remote admin page, CSRF 403, durable admin.action; monolith-parity
[M3/M3b] incl. a real form-submit emit), then re-runs the monolith
on the same player front for parity. **psql is REQUIRED** (the script dies at
startup without it — DB assertions are mandatory, no HTTP fallbacks) and the SQL
helper follows `DATABASE_URL`, same as the services. Extend it with a named
assertion whenever you add a module or cross-process flow. **Never ship a
monolith-only feature** — both topologies are supported compilation paths.

Smoke test (monolith or through gateway-svc). The dev conveniences are explicit
opt-ins/opt-outs (fail-closed defaults), so the monolith needs `APIKEYS_DEV_SEED=1`
(dev API keys below), `ACCOUNTS_DEV_AUTH=1` + `INVENTORY_DEV_GRANT=1`
(register/login + IAP grant), `ADMIN_COOKIE_SECURE=0` (session cookie over plain
http) and a seeded admin user (`adminctl create-user`) — `./run.sh` / `./run.ps1`
set/seed all of these for you (dev portal creds `admin`/`admin`):
```
curl -X POST localhost:8080/match/report -H "X-Api-Key: dev-key-server" -d '{"Winner":"alice","Loser":"bob"}'
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
modules/                   # private impls — 12 fortresses + gateway (see above)
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

The user approves the tags with the plan (called out at ExitPlanMode). Ask only for
untagged/ad-hoc work, and if any step is a subagent lane also ask **"what effort
level?"** (effort does NOT inherit — embed it in the prompt). Review each diff against
its plan step before dispatching the next; commit after each task (subagents may
commit their own work). Mid-rollout, don't silently flip a tag — ask.

**Cross-cutting Agent-call invariants (explicit `model:`, effort/nav-guidance don't
inherit, trailer, concise prompts) — shared by research + implementation:
[docs/reference/subagent-dispatch.md](docs/reference/subagent-dispatch.md). Lane
heuristic, dispatch rules, refactor safety:
[docs/reference/implementation-mode.md](docs/reference/implementation-mode.md).**

## Agent memory backup — MANDATORY

The Claude Code project memory lives OUTSIDE the repo
(`$HOME/.claude/projects/<mangled-repo-path>/memory/`, per-machine path). It is
mirrored into the repo at `memory/` so it survives across machines via git.

- **After ANY change to memory** (write/update/delete a memory file or `MEMORY.md`),
  run `scripts/memory-sync.sh push` (or `.ps1`) — it mirrors live → `memory/` and
  commits `chore(memory): …`. Don't hand-copy; the script handles deletions too.
- **After a `git pull`/sync**, run `scripts/memory-sync.sh pull` — it mirrors the
  git copy back to this machine's live memory dir. Do this before relying on recall.
- The live path is derived (repo abspath → non-alnum→`-`), so scripts are portable;
  override with `CLAUDE_MEMORY_DIR` if detection is ever wrong. `… path` prints it.

## Git Safety — MANDATORY

**Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes** without the user's say-so. To
inspect old contents use `git show <sha>:<path>`. Only ever `git reset --soft HEAD~1`
to undo a commit *you* just created *this turn*, and only when nothing else has
committed since. Never `git push --force` or rewrite published history without
explicit instruction. Commit or push only when the user asks; if on the default
branch, branch first.

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

# Go → Rust full port plan (single project from here on)

**Date:** 2026-07-08 15:17 (revised 16:0x after independent review — BLOCK punch list addressed)
**Decision basis:** [decision-migrate-everything-to-rust] — the whole backend moves to Rust.
The Go backend is archived at `experiments/go-sketch/` and stops evolving; after this
plan completes, the Rust workspace at repo root is the only developed project.

## Context (research synthesis)

Six research/audit passes ran before this plan (2 Go inventories, 1 foundations/tooling
diff, 2 adversarial Rust audits, 1 live verification run), plus an independent reviewer
pass on this plan itself. Findings that drive the plan:

**The Rust sketch is genuinely modular and split-verified** — live `split-proof`
passed all assertions (3-process split + monolith parity, negative auth/allow-list
cases, DB-row-level wipe verification). Registry swap, bus-owned durable plane
(outbox written in the same tx as domain state, inbox dedup per subscriber, relay
`FOR UPDATE SKIP LOCKED` by origin), schema-per-module with no cross-module FK: all
real SQL, all exercised. 128+3 tests green, clippy clean.

**But four structural sins must be fixed BEFORE porting 8 more modules**, or each
port multiplies them:

1. **Topology leak (audit F1):** `modules/characters/src/lib.rs:334,354,451-458` and
   `modules/inventory/src/lib.rs:527,543,691-696` hold `edge: Option<Arc<Mutex<edge::Server>>>`,
   constructed via `with_edge` in split mains, `if let Some(edge)` in `init` — the
   literal `if m.Edge != nil` regression. Root cause: wire-only capabilities
   (`characters.ownership`) are not in the `opsapi` slots, so nothing generic exposes
   them on the edge; the hand-rolled registration landed inside the domain module.
2. **Fused contracts (audit F2):** `api/characters/rpc` and `api/inventory/rpc` claim
   "pure, transport-free" but depend on `edge` (quinn/rustls) because the `#[rpc]`
   macro emits trait + transport glue into one crate. Go kept `<name>api` (pure) and
   `<name>rpc` (glue) separate.
3. **No per-module service:** `config` compiles only inside `cmd/inventory-svc`; it has
   no sync edge exposure at all, so it *cannot* be split out today. Owner rule
   (2026-07-08): **every domain module in `modules/` MUST compile and boot as its own
   `<name>-svc` binary, with zero module→module dependencies; the only gate is the
   `api/<name>/` contract crates.**
4. **Tests inline:** every `lib.rs` carries `#[cfg(test)] mod tests` inline (~⅓ of each
   file: config 630/961, inventory 708/1007, characters 469/731, plus all of `core/`).
   Owner rule: tests live in separate files.

**Owner decision:** `messaging` and `remote` are process infrastructure, not domains —
they move to `core/`. The fortress rule ("own -svc, no cross-deps") then applies
uniformly to everything remaining in `modules/`. `gateway` stays a module (it already
has `cmd/gateway-svc`).

### Global design rule introduced by this plan: cross-module events are durable

The fortress topology (every domain module its own process) makes the sync bus's plain
`emit`/`on` an **intra-module-only** tool: it never crosses a process boundary. Review
found three cross-module topics currently (or planned) on the plain bus that would
silently die in split: `config.changed` (emitted via plain `bus.emit` from config's
LISTEN/NOTIFY listener at `modules/config/src/lib.rs:255,328`, consumed via plain `on`
by inventory at `modules/inventory/src/lib.rs:646`), Go's `player.registered`
(plain Emit), and Go's `match.finished` (plain Emit, no producer schema/tx at all).

**Rule:** every event whose subscriber lives in another module uses the durable plane —
producer `emit_tx` inside a real tx, consumer `on_tx`/`on_tx_raw` with a subscriber
name. Plain `emit`/`on` remains only for same-module reactions. Consequences are
worked into Steps 5, 6, 8, 10 (each names its tx source). `core/bus` already has
everything needed: `on_tx_raw` exists at `core/bus/src/lib.rs:237`.

**What the port must bring over from Go** (paths under `experiments/go-sketch/`):
modules `accounts` (~1.8k LOC incl. contracts: argon2id dev auth, Epic OIDC verifier,
Epic web OAuth link/login, sessions — closes the `DevSessionVerifier` stub), `admin`
(portal + local/remote item fan-out), `audit`, `scheduler` (+`scheduler-svc`,
per-schedule `pg_try_advisory_lock`), `match`/`rating`/`leaderboard` (86/45/115 LOC),
`webui`; cross-cutting infra `metrics/` (private Prometheus registry, `/metrics`),
`httpmw/` (per-IP token bucket + trusted-proxy XFF walk, readiness slot, skip-infra),
gateway HTTP passthrough (`/admin`, `/accounts/epic/*` are HTML/browser flows, not
typed ops); verification net (`govulncheck`→cargo-audit, fuzz, proptest for
outbox/edge, mutation, apidiff-equivalent, topiccheck-equivalent, verify tiering).

**Already at parity (do NOT re-port):** config (incl. LISTEN/NOTIFY live-reload +
admin editor), characters, inventory, messaging, all of `core/`, rpc codegen
(proc-macro), split-proof (ahead of Go), QUIC player front (ahead of Go).

### Why not extend / why new code (Open/Closed check)

Every ported module maps 1:1 onto an existing Go module with no Rust twin — no overlap
with existing Rust modules exists (verified by inventory diff: B.6). The refactor
steps modify existing code deliberately: they fix seam violations, which is exactly the
"discuss before violating" carve-out — discussed and decided with the owner 2026-07-08.

### Target end-state topology

- `core/`: app, bus, contrib, edge, lifecycle, opsapi, outbox, registry, **messaging**,
  **remote**, (new) **metrics**, (new) **httpmw**.
- `api/<name>/`: `<name>api` (pure traits + method consts + HTTP bindings, NO edge dep),
  `<name>events`, `<name>rpc` (generated glue, depends on api + edge).
- `modules/`: characters, config, inventory, gateway, accounts, admin, audit,
  scheduler, match, rating, leaderboard, webui — each a fortress; each domain module
  has `cmd/<name>-svc`. **Dependency law:** a module may import foundations, contract
  crates (`<name>api`/`<name>events` of any domain), and **its own** `<name>rpc` glue;
  it must never import another module's impl crate or another domain's `<name>rpc`.
- `cmd/`: server (monolith), gateway-svc, + one `<name>-svc` per domain module.
  Mains differ only in module list + which QUIC planes the process serves.

---

## Phase 0 — Fortress refactor (before any port)

### Step 1 — Split inline tests out of implementation files `[sonnet]`

**(a)** Every crate with inline `mod tests`: `core/{app,bus,contrib,lifecycle,opsapi,
outbox,registry}/src/lib.rs`, `core/edge/src/{codec,frame,player,tls,wire}.rs`,
`modules/{characters,config,gateway,inventory,messaging,remote}/src/lib.rs`.
**(b)** First because it's pure motion: later steps rewrite these files and should not
carry the test bulk through every diff.
**(c)** For each file: cut the `#[cfg(test)] mod tests { … }` block into
`src/tests.rs` (or `src/<file>_tests.rs` for edge's per-file modules) and replace with
`#[cfg(test)] mod tests;` (`#[cfg(test)] #[path = "codec_tests.rs"] mod tests;` where
named). Same crate ⇒ private items stay visible; zero production-code changes.
Gate: `cargo test --workspace` count unchanged (128 unit + 3 integration).
**(d)** `[sonnet]` — mechanical sweep.

### Step 2 — Split `<name>api` (pure) from `<name>rpc` (glue) `[fable]`

**(a)** `tools/rpc-macro/src/lib.rs`; `api/characters/{rpc→api+rpc}`,
`api/inventory/{rpc→api+rpc}`; consumers: `modules/inventory/Cargo.toml`
(→ `charactersapi`), `modules/remote`, `modules/gateway`, `cmd/*`.
**(b)** Before Step 3/4: the edge-slot seam and generic remote need a place to put
transport glue that domain consumers don't import.
**(c)** Metadata handoff between the two crates — decided mechanism (review item 4:
a proc macro cannot resolve names in another crate, and today's `#[rpc]` strips
`#[http(...)]` attrs when re-emitting the trait, `tools/rpc-macro/src/lib.rs:216-226`,
so re-parsing the api crate's public trait cannot work): the `#[rpc]` macro in
`<name>api` **additionally emits a `macro_rules!` metadata-callback macro** (e.g.
`characters_ownership_meta!`) whose expansion passes the full method metadata token
tree — names, signatures, and the pre-strip `#[http]` attributes — to a caller-supplied
callback macro (standard token-tree callback pattern; `#[macro_export]` so the glue
crate sees it). The `<name>rpc` crate contains one invocation:
`charactersapi::characters_ownership_meta!(rpc_macro::generate_glue);` where
`generate_glue` is a new proc macro in `tools/rpc-macro` that emits today's
edge-dependent output (`Client`, `register_server`, `IdentityHandler`) plus the
`remote_factories()` helper Step 4 needs. The transport-free output (trait, method-name
consts, `operations()`, `route_bindings()`) stays generated in `<name>api`.
Fallback if the callback pattern hits a wall: a `build.rs` in `<name>rpc` that
re-parses the api crate's `src/lib.rs` with `syn` and generates the glue into
`OUT_DIR` — decided now as the named fallback, no third option.
Gate (rescoped per review item 6, consistent with Step 3's design): no module imports
a **foreign** domain's `<name>rpc` crate — `cargo tree -p inventory` shows
`charactersapi` but no `charactersrpc`; a module importing its OWN `<name>rpc` is
sanctioned (it is its private impl surface, Go rule 5).
**(d)** `[fable]` — macro surgery on the codegen seam; the plan's highest-risk step.

### Step 3 — Topology-blind edge exposure: kill `Option<edge::Server>` `[fable]`

**(a)** `core/edge` (new `pub struct EdgeReg(Box<dyn FnOnce(&mut Server) + Send>)` +
slot constant `edge::EDGE_SLOT` — placed in `core/edge`, NOT opsapi: `edge` already
depends on `opsapi`, so an opsapi-hosted type closing over `edge::Server` would be a
dependency cycle, review item 7); `core/app/src/lib.rs` (`run` applies contributions);
`modules/characters/src/lib.rs` (drop `edge` field, `with_edge`, `if let Some`),
`modules/inventory/src/lib.rs` (same); `cmd/{server,characters-svc,inventory-svc}/src/main.rs`
(constructors lose edge injection).
**(b)** Must precede the per-module-svc step: `config-svc` and every future
`<name>-svc` get edge exposure through this seam instead of copying the hack.
**(c)** Final design: each domain module contributes its edge registrations
**unconditionally** in `init` — `ctx.contribute(edge::EDGE_SLOT,
EdgeReg::new(move |s| charactersrpc::ownership_rpc::register_server(s, svc.clone())))`
— using its OWN `<name>rpc` glue (sanctioned, see dependency law above). `app::run`,
which already owns the process's `Option<edge::Server>` (`core/app/src/lib.rs:222-238`,
`mem::take` before listen), drains `Contributions(EDGE_SLOT)` **after Build (all
`init`s done) and before `listen`**, applying them iff the process has an edge server;
in the monolith the contributions are simply not applied. Lifecycle fits: contribute
happens in `init`, application happens post-Build in `run` — no ordering hazard, and
the served `Arc<Service>` exists because `init` constructs it before contributing.
The module never sees an `Option`, never knows the topology.
Gate: `grep -rn "with_edge\|Option<.*edge::Server" modules/` → empty; split-proof PASS.
**(d)** `[fable]` — new seam across core/app + two modules.

### Step 4 — Move `messaging` and `remote` to `core/`; make `remote` generic `[opus]`

**(a)** `modules/messaging` → `core/messaging` (deps already core-only: bus, outbox,
lifecycle, sqlx). `modules/remote` → `core/remote`; delete the provider `match` at
`modules/remote/src/lib.rs:254` — `Stub::new(provider, addr, factories)` takes
client-registration closures produced by each domain's glue:
`remote::Stub::new("characters", addr, charactersrpc::remote_factories())` in
`cmd/{inventory-svc,gateway-svc}/src/main.rs`. The monolith (`cmd/server`) constructs
no stubs and therefore no factories — providers are local, nothing changes there.
Workspace `Cargo.toml` members + arch docs updated.
**(b)** After Steps 2–3 so remote's factories exist and no module still needs
`with_edge`. Before Step 5 so the fortress rule applies to a clean `modules/`.
**(c)** Pure crate moves + the factory injection. Core stays a leaf: `core/remote`
imports no `api/` crate (factories arrive as boxed closures from `cmd`). Admin remote
fan-out (Go's `Stub.adminFetcher`) is NOT added here — it lands with the admin port
(Step 7).
Gate: `cargo tree -p remote` shows no `api/` crates; split-proof PASS.
**(d)** `[opus]` — substantive but well-specified.

### Step 5 — `config-svc` + durable `config.changed` + fortress enforcement `[opus]`

**(a)** Files: `api/config/api` (extend), new `api/config/rpc`, `modules/config/src/lib.rs`,
`modules/inventory/src/lib.rs:646` (subscription), new `cmd/config-svc`,
`cmd/inventory-svc/src/main.rs`, `split-proof.sh`/`.ps1`, new `tools/archcheck`,
`verify.sh`/`.ps1`, `core/messaging` (origin assertion).
**(b)** This is the owner's fortress rule made real and enforced; needs Steps 2–4.
**(c)** Three sub-designs, each concrete:

*Config remoting.* The existing `configapi::Config` trait is **sync, non-Result**
(`fn get_string(&self,…) -> String`, `api/config/api/src/lib.rs:15-28`) — it cannot go
through `#[rpc]` (which requires `async` + `Result<T, opsapi::Error>`,
`tools/rpc-macro/src/lib.rs:307-331`), and consumers rely on it being a cheap cached
read. It stays as-is. Remoting works via a **snapshot + durable-invalidation client**:
a new `#[rpc]` trait `configapi::ConfigSnapshot { async fn snapshot(&self) ->
Result<Vec<Setting>, Error> }` (wire-only, no `#[http]`), implemented by the config
module over its existing store, exposed via the Step-3 edge slot. `api/config/rpc`
generates its glue + `remote_factories()`. The factory for provider `"config"`
provides under the `config.reader` registry key a `CachedConfig` adapter: implements
the sync `Config` trait over an in-process `RwLock<HashMap>` cache, boot-filled by one
`snapshot()` call in `start` (fail loud if the peer is down — config is a hard dep),
refreshed by subscribing `on_tx(configevents::CHANGED, "config-cache")` and re-reading
the changed key from the snapshot service. Consumers keep calling the same sync trait;
the registry swap stays the only difference between topologies.

*Durable `config.changed`.* Producer side: config's LISTEN/NOTIFY listener
(`modules/config/src/lib.rs:255,328`) currently plain-`emit`s; it gains a tx source by
**opening its own short transaction per notification batch** (`pool.begin()` →
`bus.emit_tx(&mut tx, …)` per changed key → `commit`) — the durable plane only needs
the outbox insert to ride a tx; the listener owns no domain write, so a dedicated tx
is correct. The admin-edit path (`apply_edit` → `set`) already runs in a store tx and
switches from post-commit `emit` to `emit_tx` inside it. Consumer side: inventory's
starter-spec reload switches `on` → `on_tx(…, "inventory")`
(`modules/inventory/src/lib.rs:646`). Split-proof gains the assertion: change a config
key via config-svc's admin/API → inventory-svc's starter spec updates (poll its
behavior, not logs).

*Fortress enforcement + origin assertion.* New `cmd/config-svc` (gateway + config +
messaging). `cmd/inventory-svc` drops `config::Config`, gains
`remote::Stub::new("config", &env_addr("CONFIG_EDGE_ADDR", "127.0.0.1:9002"), configrpc::remote_factories())`.
`split-proof` extends to 4 processes (config-svc HTTP :8083 / edge :9002). New verify
stage `fortress`: (i) builds every `cmd/<name>-svc`; (ii) `tools/archcheck` reads
`cargo metadata` and fails on any `modules/X → modules/Y` edge and on any
`modules/X → <foreign>rpc` edge (dependency law); (iii) grep-gate for
`Option<.*edge::Server` under `modules/`. `MESSAGING_ORIGIN` assertion (review item
11): implemented **inside `core/messaging`**, which knows its own config — `start`
bails when `MESSAGING_ORIGIN` is unset/`"monolith"` while `EVENTS_SUBSCRIBERS` names
at least one remote sink (the exact condition under which a shared-DB origin collision
can mis-drain another process's outbox rows). No `app::run` stub-awareness needed.
Gate: fortress verify stage PASS; 4-process split-proof PASS incl. the live-reload
assertion.
**(d)** `[opus]`.

---

## Phase 1 — Port the missing modules (each with contracts + own svc + tests in files)

Order: accounts first (it un-stubs auth for everything), then admin (renders what
others contribute), then the rest smallest-last.

### Step 6 — `accounts` module + real session verification `[fable]`

**(a)** New `api/accounts/{api,events,rpc}` (traits `Sessions{verify_session}` —
**wire-only, no `#[http]`**, so it rides the edge like `characters.ownership`;
`Auth{register,login,login_epic,me}` with HTTP bindings; `Admin{admin_data}`; event
`accountsevents::PLAYER_REGISTERED = bus::define("player.registered")`); new
`modules/accounts` (`lib.rs`, `store.rs`, `password.rs`, `epic.rs`, `epic_oauth.rs`,
`ops.rs`, `admin.rs`, `tests.rs`); new `cmd/accounts-svc`;
`modules/gateway/src/verifier.rs` + `cmd/gateway-svc/src/main.rs`.
**(b)** First port because it closes the HIGH auth finding; admin fan-out and future
modules assume real sessions.
**(c)** Port from `experiments/go-sketch/modules/accounts/` 1:1: schema `accounts`
(players/identities/sessions, 30-day TTL, 32-byte base64url tokens); argon2id via
`argon2` crate (keep Go's encoded format for parity tests); Epic OIDC via `jsonwebtoken`
+ JWKS fetch/cache enforcing alg∈{RS256,ES256}, aud, iss prefix, exp; Epic OAuth
start/callback as HTTP-native routes (browser flow, not typed ops), in-memory state map
with 10-min TTL; ops gated by `ACCOUNTS_DEV_AUTH` / `EPIC_CLIENT_ID` /
`EPIC_CLIENT_SECRET` exactly as Go. `PLAYER_REGISTERED` is emitted via **`emit_tx`
inside the registration store tx** (durable rule; Go used plain Emit — upgraded here
because audit-svc consumes it cross-process from Step 8 on).

*Gateway verification wiring (review item 5 — no silent dev fallback):* gateway keeps
its consumer-defined `SessionVerifier` trait; a new adapter resolves
`registry.require::<dyn Sessions>(key("accounts","sessions"))` — provided locally by
the accounts module in the monolith and accounts-svc, and **by a mandatory
`remote::Stub::new("accounts", …, accountsrpc::remote_factories())` in
`cmd/gateway-svc`'s module list** (added in this step). Fallback policy: if the
`accounts.sessions` capability is absent at init, the gateway **fails startup loudly**
— UNLESS `ACCOUNTS_DEV_AUTH=1` is explicitly set, which enables `DevSessionVerifier`
with the existing loud warn. A mis-configured split gateway can no longer silently
accept `dev-` tokens.
Gate: parity tests ported from `accounts_test.go`/`parity_test.go`; split-proof
replaces `dev-` tokens with "register+login via gateway front → real token → op
authorized; stale/garbage token → 401".
**(d)** `[fable]` — security-critical (token verify, OIDC, argon2id) + new seam usage.

### Step 7 — Gateway HTTP passthrough + `admin` portal module `[opus]`

**(a)** Passthrough: `modules/gateway/src/lib.rs` front-door fallback (path-prefix →
reverse proxy table from env, Go's `experiments/go-sketch/gateway/httpproxy.go`
semantics) — `/admin → ADMIN_HTTP_ADDR`, `/accounts/epic → ACCOUNTS_HTTP_ADDR`.
Portal: new `modules/admin` (embed `admin.html.tmpl`+`theme.css` from Go — minijinja
templating; declarative `adminapi::Content{KPIs,Table,Form}` rendering; Basic-auth gate
`ADMIN_USER`/`ADMIN_PASS`, open+warn when unset); `api/admin/api` extended with
`Form`/`RemoteFetch`/`ErrItemAbsent` parity; `core/remote::Stub` gains the admin
`RemoteFetch` contribution (the deferred M2 TODO, formerly
`modules/remote/src/lib.rs:290-294`) fed by each provider's admin glue factory (part
of `remote_factories()`).
**(b)** After accounts so the portal has ≥4 real contributors (config, characters,
inventory, accounts) and the fan-out has a remote peer to prove against.
**(c)** Slugify/grouping/POST-form semantics 1:1 from
`experiments/go-sketch/modules/admin/admin.go`; remote item failure → error card,
`ErrItemAbsent` → silent skip. Admin hosted in its own `cmd/admin-svc` (fortress rule;
differs from Go's co-hosting with inventory). Split-proof asserts `/admin` through
gateway passthrough renders items from ≥2 remote peers.
**(d)** `[opus]`. UI translation is 1:1 from the Go template, not new design.

### Step 8 — `audit` module `[opus]`

**(a)** New `modules/audit` + `cmd/audit-svc`. No api crate (pure sink, like Go).
**(b)** After admin (it contributes an admin item) and before scheduler (its prune
consumer must exist to receive `scheduler.fired`).
**(c)** Port `experiments/go-sketch/modules/audit/audit.go` with one deliberate
deviation (durable rule): Go's split of `durableTopics` vs `bestEffortTopics` assumed
co-hosting with producers; in the fortress topology **all audited topics are durable**
— `character.created`, `character.deleted`, `player.registered`, `config.changed`,
`match.finished` all consumed via `on_tx_raw(topic, "audit", …)` (exists:
`core/bus/src/lib.rs:237`; raw JSON, no payload-type import — preserves Go's
zero-coupling design). Producers already emit all five durably by their respective
steps (characters today; config Step 5; accounts Step 6; match Step 10). Schema
`audit.log` (bigserial, topic, jsonb, at + index); `scheduler.fired`-reactive prune via
typed `on_tx` (`AUDIT_RETENTION_DAYS`, default 30) in the handed tx; anti-drift test
ported from Go, adjusted to the single durable topic list.
**(d)** `[opus]` — touches the bus seam (raw durable subscribe).

### Step 9 — `scheduler` module + `cmd/scheduler-svc` `[opus]`

**(a)** New `api/scheduler/events` (`Fired{name}` = "scheduler.fired"),
`modules/scheduler`, `cmd/scheduler-svc` (gateway + scheduler + messaging, no edge
server — pure durable producer).
**(b)** After audit (its only consumer).
**(c)** Port `experiments/go-sketch/modules/scheduler/scheduler.go` precisely: schema
`scheduler.schedules(name PK, interval_seconds, last_fired)` seeded with
`('audit-prune', 86400)`; 1s tick loop in `start`; per-name fire on a DEDICATED
connection: `pg_try_advisory_lock(fnv1a(name))` → re-check `stillDue` under lock →
`UPDATE last_fired` + `emit_tx` in one tx → commit → unlock in a drop-guard running on
a non-cancellable scope (Go NOTE #10 parity: a cancelled shutdown ctx must not skip
the unlock — in Rust, perform the unlock via a blocking-safe `tokio::spawn` in the
guard, awaited by `stop`). `SCHEDULER_ENABLED` env. Integration test: two scheduler
module instances against the same DB, exactly one fire per interval.
**(d)** `[opus]` — advisory-lock subtleties.

### Step 10 — `match` + `rating` + `leaderboard` `[opus]`

**(a)** New `api/match/{api,events,rpc}` (`Report`, `Finished{match_id,winner,loser}`
= "match.finished"), `api/rating/{api,rpc}` (`MmrReader{mmr}` — wire-only),
`api/leaderboard/{api,rpc}` (`TopScores`); `modules/match`, `modules/rating`,
`modules/leaderboard`; `cmd/{match,rating,leaderboard}-svc`.
**(b)** Last of the domain ports — smallest, and they exercise every seam already
proven by then.
**(c)** These three go beyond a 1:1 port because Go kept them monolith-only and the
fortress rule + durable rule forbid that (memory: never-monolith-only-features). The
deltas, spelled out:
- **match gains a schema** (`match.matches(id uuid PK, winner, loser, at)` — Go had no
  persistence): `Report` inserts the match row and `emit_tx`s `match.finished` **in
  the same tx** — this is the durable rule's required tx source; recording matches is
  the natural domain write. Keep `Winner`/`Loser` JSON casing for parity. `Requires(["rating"])`
  via consumer-local trait resolved from the registry (`rating.mmr` key), remote-backed
  by `ratingrpc::remote_factories()` in `cmd/match-svc`.
- **rating** provides `MmrReader` (wire-only `#[rpc]` trait) and consumes
  `match.finished` via `on_tx(…, "rating")` (+15/−15). It stays in-memory as in Go —
  documented consequence: **a rating-svc restart resets MMR to 1000 while the rest of
  the system keeps running**; acceptable for a for-fun backend, noted in the module
  doc. (The inbox dedup tx is used only for exactly-once claim; the handler mutates
  memory.)
- **leaderboard** consumes `match.finished` via `on_tx(…, "leaderboard")` (upgraded
  from Go's plain `On` — required to cross processes; the "best-effort" framing is
  retired with the durable rule), upserts `leaderboard.scores`, serves top-100 via
  `#[http]` op.
Gate: split-proof (or a dedicated 3-process integration script) asserts
`POST /match/report` through the gateway → rating changes (query via a debug wire op
or repeat-report ordering) → `GET /leaderboard` shows the win, all across
match-svc/rating-svc/leaderboard-svc.
**(d)** `[opus]` (review item 10: the rating seam and durable upgrades are new design,
not mechanical translation).

### Step 11 — `webui` `[sonnet]`

**(a)** New `modules/webui`: serve embedded `index.html` (copy from Go, adjust fetch
paths if any changed) at `/`, exact-path-only; monolith-only registration in
`cmd/server` — **the one sanctioned exception to the fortress-svc rule** (a dev demo
SPA, owner-visible exemption recorded here).
**(b)** Needs accounts endpoints live (Step 6).
**(c)** `include_str!`, one route, no state.
**(d)** `[sonnet]`.

---

## Phase 2 — Cross-cutting infra

### Step 12 — `core/metrics` (Prometheus) `[opus]`

**(a)** New `core/metrics` wrapping the `prometheus` crate with a PRIVATE registry;
axum middleware layer recording `http_requests_total` + `http_request_duration_seconds`
labeled `{method, path(matched route via MatchedPath), status}`; `/metrics` route
mounted by `core/app` for module-hosting processes, NOT gateway-svc (Go parity).
**(b)** Independent of ports; before rate limiting so skip-infra covers `/metrics`.
**(c)** Port semantics from `experiments/go-sketch/metrics/metrics.go` incl. the
matched-route-not-raw-path label rule.
**(d)** `[opus]`.

### Step 13 — `core/httpmw`: rate limit + trusted-proxy client IP + readiness slot `[opus]`

**(a)** New `core/httpmw`: per-IP token bucket (`governor` or hand-rolled) with idle
eviction; `client_ip` extractor implementing Go's right-to-left XFF walk over
`TRUSTED_PROXY_CIDRS` (do NOT use tower_governor's default XFF extractor — it trusts
XFF unconditionally, the exact bug Go guards); `RATE_LIMIT_RPS`/`RATE_LIMIT_BURST`
opt-in in `core/app`, always-on default 20/40 in gateway front door; skip-infra for
`/healthz|/readyz|/metrics`; readiness contribution slot folded into `/readyz`
(JSON per-failed-check body).
**(b)** Gateway is the front door for real auth (post-Step 6) — needs abuse limits.
**(c)** Port `experiments/go-sketch/httpmw/httpmw.go` semantics 1:1; property-test the
XFF walk.
**(d)** `[opus]` — security-sensitive parsing.

---

## Phase 3 — Verification net parity

### Step 14 — verify tiering + missing gates `[sonnet]` (+ `[opus]` for topiccheck)

**(a)** `verify.sh`/`.ps1`: add `--fast/--all/--slow/--strict` tiering (Go's shape).
BLOCKING: build, clippy `-D warnings`, test, `cargo audit` (auto-install pinned, Go's
ensure_tool pattern), fortress stage (Step 5), split-proof. ADVISORY (`--all`):
`cargo public-api` diff on `api/*` crates vs HEAD (apidiff parity, additive-only
guard; **requires a pinned nightly toolchain for rustdoc JSON — the ensure_tool step
installs/pins it, and the stage SKIPs when nightly is unavailable**); port
`outbox/relay_prop_test.go` and `edge/prop_test.go` to proptest in `core/outbox`,
`core/edge`; cargo-fuzz targets `core/edge/fuzz/fuzz_targets/{frame,wire}_decode.rs`
run `-max_total_time=10` each (SKIP on toolchains without fuzz support). SLOW:
`cargo mutants -p edge -p gateway -p outbox -p registry -p bus`. topiccheck-equivalent:
`linkme` distributed slices — `bus::define` registrations and `on`/`on_tx` subscribe
sites record `{topic, role}` entries at link time; `tools/topiccheck` binary iterates
both slices and fails on defined-but-unsubscribed topics; allowlist via an explicit
`allow_unsubscribed!(TOPIC)` registration.
**(b)** Last: gates verify what the ports built; the topiccheck redesign needs the
final module set to be worth wiring.
**(c)** Per the foundations-diff report §2–3; `rpcgen -check` has no Rust analogue
needed (proc-macro can't drift).
**(d)** `[sonnet]` for script/stage work and proptest/fuzz ports; `[opus]` for the
linkme topiccheck design.

### Step 15 — Retire the Go sketch `[inline]`

**(a)** `experiments/go-sketch/` (delete), CLAUDE.md rewrite (it still documents the
Go commands/layout — becomes the Rust workspace's guide), memory updates.
**(b)** Only after Steps 1–14 verified (full verify `--all` + extended split-proof
green) — the archive is the porting reference until then.
**(c)** Deletion is destructive: **ask the owner before executing this step.**
CLAUDE.md rewrite: commands (`cargo`/verify/split-proof), layout, module recipe in
Rust terms, fortress rule + durable-events rule codified.
**(d)** `[inline]` + owner confirmation.

---

## Sequencing summary

Phase 0 (Steps 1–5) is strictly ordered. Phase 1: 6 → 7 → 8 → 9 → 10 → 11.
Phase 2 (12–13) can interleave after Step 6. Phase 3 (14) last, 15 after everything.
Commit after every step (Conventional Commits, `(Step N — …)` notes); split-proof is
the regression gate from Step 3 onward; each subagent prompt carries its lane's
Co-Authored-By trailer and nav guidance.

## Known risks

- Step 2's macro-callback handoff is the highest-uncertainty design; the committed
  fallback is the `build.rs`+`syn` glue generation (no third option).
- Epic OIDC/OAuth port (Step 6) touches live external endpoints — parity tests must
  run against recorded fixtures, not Epic.
- The durable-events rule upgrades several Go plain-Emit paths (`player.registered`,
  `config.changed`, `match.finished`) to `emit_tx` — a deliberate, reviewed deviation
  from 1:1 porting forced by the fortress topology; each upgrade names its tx source
  in its step.
- Splitting match/rating/leaderboard into svcs goes beyond Go; the rating rpc seam is
  new code (smallest possible scope) and rating-svc restarts reset in-memory MMR
  (documented, accepted).

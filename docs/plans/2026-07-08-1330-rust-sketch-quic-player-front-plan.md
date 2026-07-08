# rust-sketch: QUIC player front + dedicated gateway-svc (single front door)

**Date:** 2026-07-08 13:30
**Target:** `experiments/rust-sketch`
**Decision (user):** dedicated `gateway-svc` process as THE front door; external players
connect to it over QUIC. Edge servers added to provider processes ONLY where a real
remote sync path needs them.

---

## Context — research findings (3 subagents + inline reading, 2026-07-08)

### What exists today (rust-sketch, M1)

- `core/edge` is real QUIC (quinn 0.11 + rustls 0.23, TLS 1.3-only, ALPN `b"edge"`),
  **mutual TLS hard-required**: `DevCA::server_tls()` builds a `WebPkiClientVerifier`
  (`core/edge/src/tls.rs:165-178`); negative tests prove a cert-less or foreign-CA
  client is rejected (`core/edge/src/lib.rs:217-249`). `Server::listen` hard-codes
  `ca.server_tls()` (`server.rs:82-88`) — **no seam for a no-client-cert listener.**
- Wire envelope `wire::Request{method, identity: Option<String>, payload}` — `identity`
  is a TRUSTED field, readable only because the hop is mTLS (`wire.rs:12-17`). A
  player-facing listener must NOT accept this envelope (a player could stamp any
  identity).
- `modules/gateway` fronts every process over **HTTP/axum** (fallback handler): match
  op by verb+path → auth-once (bearer → `SessionVerifier` → `Identity`) → decode →
  `LocalBackend`/`RemoteBackend` → encode (`modules/gateway/src/lib.rs:279-334`).
  `RemoteBackend` dials peers over QUIC via `remote_caller` keyed by env
  `<PROVIDER>_EDGE_ADDR` (`lib.rs:222-244`). Route table (`RouteTable`, private) is
  built lazily from `opsapi::SLOT`/`BINDING_SLOT`/`LOCAL_SLOT`.
- `#[rpc]` macro already generates **`route_bindings()`** (Operation+OpBinding, no
  LocalOp) per `#[http]`-annotated trait (`tools/rpc-macro/src/lib.rs:449-457`;
  `opsapi::RouteBinding`, `core/opsapi/src/lib.rs:353-363`, whose doc says verbatim:
  built "for a dedicated split front-door process"). **Nothing calls it outside the
  macro's own test** — the front-door glue was pre-built but never wired.
- `modules/inventory` already has `Inventory::with_edge(...)` registering
  `holdings_rpc::register_server` (`modules/inventory/src/lib.rs:543-545, 693-696`)
  — **dead code today**: `cmd/inventory-svc` passes `None` for the edge server.
- `app::run(cfg, modules, edge_server: Option<Arc<Mutex<edge::Server>>>)` binds ONE
  optional mTLS edge listener + one axum listener; `PgPool::connect` is
  **unconditional** (`core/app/src/lib.rs:153-155`). `lifecycle::Context` already
  supports no-DB (`db: Option<PgPool>`, `Context::new()`, `context.rs:32-70`) — only
  `app::run` forces the pool.
- Cross-process sync calls today: exactly one — inventory-svc → characters-svc
  `characters.ownerOf` via `remote::Stub` (`modules/remote/src/lib.rs:216-240`).
  characters-svc serves edge `:9000`; inventory-svc serves nothing.

### The references (Go master + JVM sketch) — and what each got wrong

- **Go `cmd/gateway-svc`** (the closest model): pure transport process (no
  `app.Run`, no DB), static route table from generated `RouteBindings()`, HTTP front
  `:8082` with real bearer auth, QUIC front `:9100` (`GATEWAY_EDGE_ADDR`) — **but the
  QUIC front is a raw `HandlePrefix` byte relay with NO bearer verification AND it
  requires mTLS client certs** (`ServerTLS` = `RequireAndVerifyClientCert`,
  `edge/tls.go:178-195`), which a real player can never present. Docs admit it:
  "prefix relay with no bearer→player_id auth … future scope"
  (`docs/2026-07-07-2145-gateway-svc-single-front-door-status.md:111-113`).
- **JVM quarkus sketch**: gateway QUIC `:9200` is server-cert-only (players can
  handshake) and MessagePack — but **zero auth modeled anywhere** and it's a blind
  prefix byte-relay (any method forwarded, including internal ones).
- **This plan closes both gaps**: server-cert-only TLS on the player port (players
  can connect) + bearer-in-envelope verified at the front against the op route
  table's `AuthReq` (auth-once holds on QUIC exactly as on HTTP), and dispatch is
  route-table-gated (method allow-list), never a blind prefix relay.

### Why not extend / depend on X (mandated rationale)

- **Why not reuse `edge::Server` + `wire::Request` for the player port?** Two trust
  faults: (1) `server_tls()` requires client certs players don't have; (2) the
  envelope's `identity` field is trusted-by-mTLS — on a public port it becomes
  attacker-controlled. A prefix `ForwardHandler` drops identity but then the token
  would have to ride inside the domain payload (layer violation). A separate small
  player plane (own envelope with `token`, own TLS config, own ALPN) is the honest
  shape; it reuses `frame.rs` and the accept-loop pattern wholesale.
- **Why not per-process player QUIC fronts (no gateway-svc)?** User decided single
  front door (matches Go/JVM end-state and the `unified-operation-transport`
  memory). Per-process fronts would multiply the public surface and re-open the
  per-module-shim pattern the Go work explicitly killed.
- **Why not a new `modules/frontdoor` module for route bindings?** `remote::Stub`
  already exists, is per-provider, and already owns the "this provider is remote"
  knowledge; contributing the provider's `route_bindings()` from `Stub::register` is
  one match-arm each, no new module, and makes ANY process with a stub front-capable
  (the Go end-state `selectBackend` was aiming at).
- **Why does inventory-svc get an edge server (user's conditional)?** gateway-svc
  hosts NO providers, so `inventory.*` ops resolve `BackendKind::Remote` → it must
  dial an inventory edge. `Inventory::with_edge` + `register_server` already exist
  (dead); wiring is 3 lines in `main.rs` + a port. **Who does NOT get one:**
  `cmd/server` (monolith — all ops local), messaging/config (no sync capability;
  the async plane stays outbox → HTTP `POST /events`, bypassing the front door,
  per the `async-fanout-sync-grpc-brokerless` decision). Rule (same as Go): edge
  server ⇔ the process hosts a provider some peer must call synchronously.

### Decisions fixed by this plan

| Topic | Decision |
|---|---|
| Player transport | New `edge::PlayerServer`/`PlayerClient` plane: QUIC, server-cert-only TLS, ALPN `b"edge-player"`, envelope `{method, token, payload}` / shared `{ok, payload, error}` response, 4-byte frame reused, **1 MiB player frame cap** (mirrors gateway `MAX_BODY_BYTES`; internal edge keeps 16 MiB) |
| Error grammar (PINNED) | Transport envelope `ok:false` is reserved for transport faults (framing, JSON parse, missing handler). Every FRONT-originated domain outcome — including auth failures — returns `ok:true` with the payload being the **generated response envelope** `{status, err}` (field is `err`, NOT `error` — `tools/rpc-macro/src/lib.rs:521`). One grammar for the player client: decode payload, check `status`. |
| Player auth | Bearer token in the player envelope, verified ONCE at the front via `SessionVerifier` against the matched op's `AuthReq`; `DevSessionVerifier` for now (accounts = M2; seam already documented at `gateway/src/lib.rs:74`) |
| Dispatch gate | Route-table lookup **by method** — only `#[http]`-bound ops are reachable; wire-only internals (`characters.ownerOf`) are absent from the table ⇒ rejected |
| Topology | gateway-svc: HTTP `:8082` + player QUIC `:9100`, **no DB**; characters-svc edge `:9000`; inventory-svc edge `:9001` (new); A/B HTTP `:8080`/`:8081` become internal |
| Monolith parity | `cmd/server` ALSO wires the player QUIC front (all-local dispatch) — per the `never-monolith-only-features` memory, both topologies must serve the feature |
| Env | `PLAYER_EDGE_ADDR` (default `:9100`), `INVENTORY_EDGE_ADDR` (default `127.0.0.1:9001`); existing `CHARACTERS_EDGE_ADDR`, `EDGE_CA_CERT/KEY` unchanged |

---

## Steps

### Step 1 — `core/edge`: public-TLS player plane `[fable]`

**(a) What:** `core/edge/src/tls.rs`, new `core/edge/src/player.rs`, `core/edge/src/lib.rs`
(exports), `core/edge/src/wire.rs` (hoist `Response` to `pub` or add a
`PlayerResponse` twin — keep `Request` `pub(crate)`).

**(b) Why now:** every later step consumes this plane; it is the security-critical
core and must land with its negative tests first.

**(c) How:**
- `tls.rs`: add `PLAYER_ALPN: &[u8] = b"edge-player"`;
  `DevCA::server_tls_public() -> Result<ServerConfig, Error>` — same provider/TLS 1.3
  builder as `server_tls()` but `.with_no_client_auth()`, ALPN `PLAYER_ALPN`;
  **`DevCA::load_cert_only(cert_path) -> Result<TrustAnchor, Error>`** — a real
  player holds ONLY the CA cert, never the signing key (`DevCA::load` demands both,
  `tls.rs:78-90`), so add a trust-anchor-only type carrying `roots` + provider, with
  `client_tls_public() -> Result<ClientConfig, Error>` on it — TLS 1.3, verifies the
  server against the roots, `.with_no_client_auth()`, ALPN `PLAYER_ALPN` (dev
  stand-in for a real WebPKI cert). `DevCA` gets the same `client_tls_public()`
  delegating to it (for in-process tests).
- `player.rs`:
  - `pub struct PlayerRequest { pub method: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub token: Option<String>, pub payload: Box<RawValue> }` — `#[serde(default)]` is REQUIRED (without it an omitted `token` fails to parse and every AuthNone call dies malformed; copy the attribute pair from `wire.rs:16`). `token` is ATTACKER-CONTROLLED input, never an identity.
  - `pub type PlayerHandler = Arc<dyn Fn(String, Option<String>, Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync>` (method, token, payload).
  - `pub const MAX_PLAYER_FRAME: usize = 1 << 20;`
  - `pub struct PlayerServer { handler: OnceLock<PlayerHandler> }` with `set_handler(PlayerHandler)` and `listen(self, addr: SocketAddr, ca: &DevCA) -> Result<RunningServer, Error>` — accept/stream loop copied from `server.rs:96-113`/`serve_conn`/`serve_stream` shape, but: `server_tls_public()`, frame reads capped at `MAX_PLAYER_FRAME`, parse `PlayerRequest`, one call = one stream, respond with the `{ok, payload, error}` envelope. Missing handler ⇒ `ok:false` "front not wired". **Transport `ok:false` ONLY for transport faults** (frame/parse/missing-handler); a handler `Ok(bytes)` passes through as `ok:true` — domain outcomes ride inside `bytes` (the pinned error grammar above).
  - **Explicit `quinn::TransportConfig` on the player endpoint** (the internal plane keeps quinn defaults; the public port must not): max concurrent bidi streams per connection (e.g. 16), `max_idle_timeout` (e.g. 30 s), stream receive window clamped to `MAX_PLAYER_FRAME` — caps a certless attacker's per-connection cost. Full rate limiting stays out of scope; these are transport knobs, not a limiter.
  - `pub struct PlayerClient` — `dial(addr, trust: &TrustAnchor)` using `client_tls_public()` (SNI `"localhost"` like `Client::dial`, `client.rs:47`), `call(method, token: Option<&str>, payload) -> Result<Vec<u8>, Error>` mirroring `call_raw_id` (`client.rs:69-95`). While here: `raw_from_bytes` mislabels an invalid-payload error as `Error::Tls` (`client.rs:124`) — relabel to `Error::Codec` (the player path now exercises it).
- Tests (e2e, same style as `lib.rs::e2e_tests`): roundtrip with/without token
  (omitted-token envelope MUST parse — the serde(default) proof); `load_cert_only`
  → `client_tls_public` dials a live `PlayerServer` successfully (the key-less
  trust path playercli uses); **negative:** (1) `PlayerClient` (no client cert)
  succeeds against `PlayerServer` but is REJECTED by the internal mTLS `Server`
  (ALPN + cert mismatch — proves the planes don't cross); (2) internal
  `Client::dial` against `PlayerServer` fails (ALPN mismatch); (3) oversize frame
  (> 1 MiB) rejected.

### Step 2 — `modules/gateway`: `FrontDoor` façade + player handler `[fable]`

**(a) What:** `modules/gateway/src/lib.rs` (refactor `GatewayState` → pub
`FrontDoor`), `modules/gateway/src/backend.rs` untouched.

**(b) Why now:** the player handler needs the same table/auth/backends the HTTP
fallback uses; extracting the façade before wiring binaries keeps one dispatch path.

**(c) How:**
- Rename `GatewayState` → `pub struct FrontDoor { slots, verifier, table: OnceLock }`
  with `pub fn new(slots: Arc<Slots>, verifier: Arc<dyn SessionVerifier>)`,
  `pub fn router(self: &Arc<Self>) -> axum::Router` (today's `front_router`), and
  `pub fn player_handler(self: &Arc<Self>) -> edge::PlayerHandler`.
- `RouteTable`: add `fn find_by_method(&self, method: &str) -> Option<&Route>`
  (linear scan like `find`, `lib.rs:193-203`). **Also: evict-on-error for the
  remote-caller cache** — `remote_caller` today dials once and caches forever
  (`lib.rs:222-244`); a provider restart would brick gateway-svc's route to it
  permanently. On a `call` error the `RemoteBackend` path drops the cached
  `Arc<dyn Caller>` for that provider so the NEXT request re-dials (mirror of
  `remote::Reconnecting`'s reset, `remote/src/lib.rs:53-118`, without the inline
  retry — one failed request, then self-heal). Applies to the HTTP front too;
  cover with a unit test using a fake failing Caller.
- `player_handler` closure, per call, returning the PINNED grammar (every domain
  outcome = `Ok(serialized generated-envelope {status, err})`, transport `Err` never
  used for domain failures):
  1. **Well-formedness gate:** `serde_json::from_slice::<&RawValue>(&payload)` —
     malformed JSON ⇒ `{status: Invalid}` AT THE FRONT (without this, garbage gets
     topology-dependent errors: Local invokers answer `Invalid` but a Remote
     adapter's parse failure surfaces as transport `Unavailable` — same input, 400
     vs 503).
  2. `find_by_method` → miss ⇒ `{status: NotFound, err: "unknown operation"}`
     (wire-only methods like `characters.ownerOf` are not in the table — the
     allow-list gate).
  3. `AuthReq::Player` ⇒ token required + `verifier.verify(token)` →
     `Identity::player`, else `{status: Unauthorized}` (missing OR invalid token);
     `AuthReq::None` ⇒ `Identity::none()`.
  4. `table.backend_for(&op)` → `backend.invoke(&op, identity, payload)` → return
     wire response bytes verbatim (the domain `Status` already rides INSIDE the
     generated response envelope — no `OpBinding::decode/encode` on this path: the
     player speaks the wire request shape directly, there is no HTTP body/path to
     translate). A backend `Err(opsapi::Error)` is re-serialized as `{status, err}`.
  The envelope field is **`err`** (`tools/rpc-macro/src/lib.rs:521`) — emit exactly
  the macro's shape so a player client decodes one grammar.
- `Gateway` module: keep `new()`/`with_verifier()`; add
  `pub fn with_player_edge(self, shared: Arc<Mutex<edge::PlayerServer>>) -> Self`;
  `init` builds one `Arc<FrontDoor>`, mounts `front_door.router()` as today, and, if
  a player edge handle is held, `shared.lock().set_handler(front_door.player_handler())`.
- Tests: unit — `find_by_method` hit/miss; player handler: no token on
  `AuthReq::Player` ⇒ `{status: Unauthorized}` envelope; bad token ⇒ Unauthorized;
  `AuthNone` op passes with `Identity::none()`; unknown/wire-only method ⇒
  NotFound; malformed JSON payload ⇒ Invalid; happy path through the demo `OpSet`
  (reuse `demo_opset()` fixture, `lib.rs:534-564`); remote-cache eviction on a
  failing fake `Caller` (second call re-dials).

### Step 3 — `remote::Stub` contributes the provider's route bindings `[opus]`

**(a) What:** `modules/remote/src/lib.rs` (`Stub::register`), `modules/remote/Cargo.toml`
(+`inventoryrpc` dep).

**(b) Why now:** gateway-svc hosts no providers — without contributed
`Operation`+`OpBinding` its route table is empty (`RouteTable::build` reads only
slots, `gateway/src/lib.rs:164-189`). Stub is the component that already encodes
"provider X is remote".

**(c) How:** in the per-provider match in `Stub::register` (`remote/src/lib.rs:216-240`):
- `"characters"` arm: existing client provides stay; additionally
  `for rb in charactersrpc::player_rpc::route_bindings() { ctx.contribute(opsapi::SLOT, rb.operation); ctx.contribute(opsapi::BINDING_SLOT, rb.binding); }` — **no `LOCAL_SLOT`**, so `select_kind` resolves Remote (`gateway/src/lib.rs:258-264`).
- new `"inventory"` arm: **route bindings ONLY**
  (`inventoryrpc::holdings_rpc::route_bindings()`, same contribute loop) — NO
  capability-client provide: nothing in any process `require`s an inventory
  capability (inventory is a leaf; the characters provides exist because inventory
  requires ownership). A dead provide is noise; add the provide only when a
  consumer appears.
- **Stated side effect (intended):** inventory-svc already holds
  `Stub::new("characters", …)` (`cmd/inventory-svc/src/main.rs:35`), so after this
  step its HTTP front on `:8081` also routes `/characters` ops remotely to A — any
  process with a stub becomes front-capable, which is the unified-front-door
  end-state. Acceptable unasserted (B's port is internal in the target topology;
  the proof drives fronts through gateway-svc and the monolith). Note it in the
  Stub doc comment.
- Double-contribution guard: a process holding BOTH `Stub("X")` and the real X
  module would contribute X's routes twice. Current binaries never do that (stubs
  stand in only for absent providers) — assert via the three mains audit in this
  step's review, document the invariant ("a Stub and its provider module are
  mutually exclusive in one process") in the Stub doc comment, and keep
  gateway-svc stub-only.
- Tests: a `Slots`-level test that after `register`, SLOT/BINDING_SLOT carry the
  provider's ops and LOCAL_SLOT does not.

### Step 4 — `core/app`: optional DB + optional player listener `[opus]`

**(a) What:** `core/app/src/lib.rs` (`Config`, `run`), the three existing
`cmd/*/src/main.rs` call sites.

**(b) Why now:** gateway-svc is a pure transport process (Go precedent: no DB, no
bus) and needs the second listener bound by the shared boot path; binaries can only
be assembled after this.

**(c) How:**
- `Config` gains `pub player_edge_addr: String` (env `PLAYER_EDGE_ADDR`, default
  `":9100"`) and `database_url` becomes `Option<String>` — `from_env()` keeps
  `Some(default DSN)`; new `Config::without_db(self) -> Config` sets `None`.
- `run` signature: `run(cfg, modules, edge_server: Option<...>, player_server: Option<Arc<Mutex<edge::PlayerServer>>>)`.
  - DB: `let pool = match &cfg.database_url { Some(dsn) => Some(PgPool::connect(dsn).await?), None => None }`; `Context::with_db(pool)` vs `Context::new()` (`lifecycle/src/context.rs:35-53` already supports both); `/readyz` pings only when a pool exists, else plain 200.
  - After Build (step 7 in `run`, next to the mTLS edge bind, `app/src/lib.rs:175-193`): if `player_server` is `Some`, `mem::take` the same way and `player.listen(to_bind_addr(&cfg.player_edge_addr).parse()?, &ca)`; close it in teardown between HTTP stop and internal-edge close.
- Update `cmd/server`, `cmd/characters-svc`, `cmd/inventory-svc` for the extra
  param (`None` for now; server gets its player front in Step 5).
- Tests: `Config::from_values` coverage for the new field/None-DB; existing app
  tests keep compiling.

### Step 5 — `cmd/gateway-svc` binary + monolith player front `[sonnet]`

**(a) What:** new crate `cmd/gateway-svc/` (member + workspace dep table in root
`Cargo.toml`), edits to `cmd/server/src/main.rs`.

**(b) Why now:** all seams from Steps 1–4 exist; this is pure assembly.

**(c) How:**
- `cmd/gateway-svc/src/main.rs` (mirror `cmd/inventory-svc` shape):
  ```rust
  let player = Arc::new(Mutex::new(edge::PlayerServer::new()));
  let mods: Vec<Box<dyn Module>> = vec![
      Box::new(gateway::Gateway::new().with_player_edge(player.clone())),
      Box::new(remote::Stub::new("characters", &env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"))),
      Box::new(remote::Stub::new("inventory", &env_addr("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"))),
  ];
  app::run(app::Config::from_env().without_db(), mods, None, Some(player)).await
  ```
  (`PORT` default `:8082` for this binary — set in scripts, not code. `env_addr` is
  a new local helper in this main — generalize inventory-svc's bespoke
  `characters_edge_addr()` pattern, `cmd/inventory-svc/src/main.rs:15-20`.)
  Note: gateway-svc runs NO messaging module — `/events` and the async plane bypass
  it entirely (delivered svc→svc, as in Go).
- `cmd/server/src/main.rs`: construct the same `PlayerServer` handle,
  `Gateway::new().with_player_edge(player.clone())`, pass `Some(player)` — the
  monolith serves players over QUIC too (all ops Local).
- Build gate: `cargo build -p gateway-svc` + `cargo test --workspace`.

### Step 6 — inventory-svc edge server `[sonnet]`

**(a) What:** `cmd/inventory-svc/src/main.rs` (3 lines), no module changes.

**(b) Why now:** gateway-svc dispatches `inventory.*` Remote — the provider must
listen. Independent of Step 5 in code but proven together in Step 8.

**(c) How:** exactly the `characters-svc` pattern (`cmd/characters-svc/src/main.rs:19,26,30`):
`let edge_server = Arc::new(Mutex::new(edge::Server::new()));` +
`Inventory::with_edge(edge_server.clone())` (constructor exists,
`modules/inventory/src/lib.rs:543-545`) + `app::run(..., Some(edge_server), None)`.
Update the stale "No edge server" doc comment. Port via `EDGE_ADDR=:9001` in scripts.

### Step 7 — `tools/playercli` `[sonnet]`

**(a) What:** new workspace member `tools/playercli` (mirror `tools/edgeca` shape,
`tools/edgeca/src/main.rs:1-46`).

**(b) Why now:** the split proof (Step 8) needs a way to drive the QUIC player
front from a shell script.

**(c) How:** args `--addr 127.0.0.1:9100 --ca run/edge-ca.crt [--token dev-alice] <method> [json-payload]`;
loads the trust anchor via `DevCA::load_cert_only` (specced + tested in Step 1 —
cert ONLY, a player never holds the CA key), dials `edge::PlayerClient`, prints the
raw response envelope to stdout. **Exit code: 0 iff transport `ok:true` AND the
payload's `status == "Ok"`; 1 otherwise** — per the pinned grammar an auth failure
arrives as `ok:true` + `{status:"Unauthorized"}`, so testing `ok` alone would call
it a success.

### Step 8 — scripts + split proof over the QUIC front `[opus]`

**(a) What:** `experiments/rust-sketch/run.sh`, `run.ps1`, `split-proof.sh`,
`split-proof.ps1`, `verify.sh`/`verify.ps1` untouched (cargo test covers new units).

**(b) Why now:** last — proves the at-risk path live (the
`verify-the-at-risk-path-not-the-safe-one` memory: the NEW topology must be the
thing exercised, committed and repeatable).

**(c) How:** 3-process split: A=characters-svc (`PORT=:8080 EDGE_ADDR=:9000`),
B=inventory-svc (`PORT=:8081 EDGE_ADDR=:9001`), G=gateway-svc (`PORT=:8082
PLAYER_EDGE_ADDR=:9100 CHARACTERS_EDGE_ADDR=127.0.0.1:9000
INVENTORY_EDGE_ADDR=127.0.0.1:9001`), shared `EDGE_CA_CERT/KEY` for all three.
Extend split-proof assertions (keep every existing one):
1. `playercli … --token dev-<uuid> characters.create '{"name":"hero","class":""}'`
   over `:9100` ⇒ exit 0 (player QUIC → gateway-svc → QUIC mTLS → A).
2. `playercli … --token dev-<uuid> inventory.<list-method per inventoryrpc>` over
   `:9100` ⇒ exit 0 — **the newest composition: player QUIC → gateway-svc →
   Remote → B's NEW `:9001` edge** (assertion 1 alone only proves the G→A leg).
3. Same GET-inventory flow as today but through gateway-svc's **HTTP** `:8082`
   (proves the HTTP front still routes cross-provider: `inventory.*` → B remote).
4. `playercli` with NO token on an auth op ⇒ exit 1 with `{status:"Unauthorized"}`
   in the printed envelope; repeat with `--token nope-x` (bad token, same expect).
5. `playercli … characters.ownerOf …` ⇒ exit 1, `{status:"NotFound"}` (wire-only
   method not routable — the allow-list gate, live).
6. Async plane unchanged: starter-sword + delete-wipe assertions stay green
   (events still svc→svc HTTP, not through G).
**Monolith parity proof (concrete, not optional):** split-proof gains a final
stage (or a sibling `monolith-proof` block in the same script): boot `cmd/server`
with `PLAYER_EDGE_ADDR=:9100`, one `playercli … characters.create` ⇒ exit 0, kill.
run.sh has no self-check today — this scripted stage IS the monolith smoke. Also
update `run.sh` microservices mode to boot G and monolith mode to export
`PLAYER_EDGE_ADDR`.

### Step 9 — docs + memory `[inline]`

Status doc `docs/<subdir>/2026-07-08-HHMM-rust-sketch-quic-player-front-status.md`
after the rollout (what was proven, ports, negative results); update
`memory/rust-sketch-split-verified-m1.md` (player QUIC front + gateway-svc now in;
edge-server rule) and `unified-operation-transport.md` (the Rust sketch closes the
`:9100`-unauthenticated gap Go left open); `scripts/memory-sync.sh push`.

---

## Explicitly out of scope (stated, not deferred-by-vagueness)

- Real session verification (accounts module) — M2; the `SessionVerifier` seam is
  the swap point, `DevSessionVerifier` stays.
- Rate limiting / connection limits on the player port (Go's gateway-svc has
  per-IP limits; the sketch does not — note in status doc).
- MessagePack codec, push/streaming, request pipelining per connection beyond
  stream-per-call (JVM sketch's known limits — unchanged here).
- Admin fan-out through gateway-svc (`/admin` reverse-proxy in Go) — the rust
  sketch has no admin module yet (M2).
- Production certs for the player port (players verify the dev CA; real deployment
  would present a WebPKI cert — the `server_tls_public` seam is where it lands).

## Verification gates

- `cargo build --workspace && cargo test --workspace` after every step.
- `./split-proof.sh` green with the 5 assertions above (Step 8) — the committed,
  repeatable proof of the at-risk topology.
- Trailer audit after rollout: `git log --format="%h %B" | grep Co-Authored` matches
  lanes (`[fable]`→Fable 5, `[opus]`→Opus 4.8, `[sonnet]`→Sonnet 4.6).

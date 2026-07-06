# The `gateway` module — external front door (QUIC-RPC router + HTTP reverse-proxy)

Durable reference for the `gateway` added to `experiments/jvm-quarkus-sketch/` (branch `quarkus-per-service`).
It is the single external entry point for the per-service split: it routes the **custom QUIC edge RPC** (game
clients) by method-prefix to the owning service, and reverse-proxies HTTP (`/admin`, `/characters`, `/inventory`).
Plan: `docs/plans/2026-07-06-2014-quarkus-gateway-plan.md`. Proven live end-to-end (see below).

## Why a gateway, and why it must route QUIC
A gateway is unavoidable for the split (single entry point, one origin, auth/TLS termination). The design
conversation's conclusion: **a gateway that can't route our QUIC is pointless** — the external player transport is
QUIC (a bespoke MessagePack RPC over QUIC streams, NOT HTTP/3), so an HTTP-only reverse proxy would leave players
out. Therefore the gateway is dual-protocol.

## Architecture — a recomposition of the `edge` stack (not new native code)
- **QUIC side = an `EdgeServer` (external) whose handlers forward via `EdgeClient`s (to internal services)**,
  routed by method-prefix. `characters.* → characters-service:9100`, `inventory.* → inventory-service:9101`. This is
  the forwarding variant of the existing inventory→characters seam; the FFM binding stays written once in `edge` and
  is reused (`MsQuicServerTransport` for players + `MsQuicClientTransport` per backend). The gateway is the first
  process to be BOTH a QUIC server and client — proven fine (msquic supports multiple registrations/process; Step-1
  smoke).
- **The byte-relay: `EdgeClient.requestRaw(method, payloadBytes)`.** The gateway forwards the inbound payload bytes
  **verbatim** without decoding the method's schema. This required a NEW raw path: `EdgeClient.request(method,
  obj)` msgpack-*encodes* its payload, so relaying an already-encoded `ByteArray` through it would double-encode
  (msgpack `bin` with a length header) and the downstream `typedHandler` decode would fail. `requestRaw` sends
  `Request(cid, method, bytes)` directly. (The reply leg was already raw.)
- **Method-aware prefix routing.** `EdgeRouter.registerPrefix(prefix, MethodForward)` forwards the ORIGINAL inbound
  method (so `characters.list` and `characters.ownerOf` under the `characters.` prefix each reach the right handler);
  exact-match still wins, unknown still errors. cid needs no remapping — each hop's cid space is independent and
  `EdgeRouter.dispatch` re-wraps with the inbound cid.
- **Outbound resilience.** Each backend leg (`RoutedBackend`) mirrors `EdgeRemotePlayerCharacters`: a retained
  `MsQuicClientTransport` + `CachedResource` + bounded one-shot reconnect; a down backend → `ForwardFailedException`
  → clean `Response(ok=false)`, never a hang. Both `ConnectionClosedException` and `TimeoutException` map to
  `ok=false`; the outbound budget is shorter than the inbound timeout so the player sees a clean failure.
- **HTTP side = Vert.x `HttpProxy`** (`io.vertx:vertx-http-proxy:4.5.28` — pinned to Quarkus 3.37.1's Vert.x line;
  5.x would clash) via Quarkus `@io.quarkus.vertx.web.Route` (from `quarkus-reactive-routes`, NOT `quarkus-vertx-http`
  — the latter doesn't carry `@Route`). Origins from plain config; path preserved verbatim (`/admin/foo` → origin
  `/admin/foo`, verified).
- **Discovery = plain `host:port` config, NOT Stork.** The QUIC seam already uses `edge.client.*.target` host:port;
  the gateway reuses that (`gateway.route.<prefix>.target`, `gateway.http.<prefix>.target`) fed by the same
  `CHARACTERS_ADDR`/`CHARACTERS_EDGE_ADDR`/`INVENTORY_ADDR`/`INVENTORY_EDGE_ADDR`/`ADMIN_ADDR` envs install.ps1 sets.
  Programmatic Stork is used nowhere in the repo — it was NOT introduced ("reuse" would have been a fiction).

## Modules + wiring
- `gateway` (impl, plain Kotlin): `GatewayEdgeServer` (role-gated `roles=gateway`, QUIC :9200), `RoutedBackend`,
  `GatewayHttpProxy` (`@Route` bean). **Needs `META-INF/beans.xml`** — without it Quarkus doesn't index the module
  as a bean archive and the `@Observes StartupEvent` never fires (the live smoke caught exactly this: gateway-service
  booted HTTP-only, no QUIC listener).
- `gateway-service` (app-shell, `io.quarkus`): links only `gateway`+`edge`+`platform`+health — NO feature impls
  (enforced by a composition rule in root `build.gradle.kts` forbidding `:characters`/`:inventory`/`:accounts`/
  `:admin`/`:characters-client`).
- Player-facing methods added to demonstrate a REAL player gateway (not the internal `ownerOf` seam):
  `characters.list(playerId)` on `CharactersEdgeServer` (:9100), `inventory.list(owner)` on a new `InventoryEdgeServer`
  (:9101). inventory-service is now QUIC server + client.
- `install.ps1 -Mode microservices` runs 3 processes: characters-service (:8080/:9100), inventory-service
  (:8081/:9101), gateway (:8082 health/:9200 QUIC). Monolith mode unaffected (gateway only in split).

## Proven live (Step-6 marquee, real 3-process split)
- QUIC routing to TWO different backends through the gateway: `characters.list → [Aria]` (characters-service),
  `inventory.list → [starter_sword]` (inventory-service).
- Degradation: killed characters-service → `characters.list` returns `ok=false` in ~3s (no hang); gateway stays up,
  `inventory.list` still works (only the killed prefix fails).
- HTTP: `GET :8082/admin/characters → 200` admin HTML through the proxy.
- The live smoke caught two integration bugs the loopback tests couldn't: the missing `beans.xml` (QUIC router never
  started) and a docker-less teardown fault.

## Scope + honest limits (v1)
- **v1 proves routing CORRECTNESS, not CONCURRENCY.** `EdgeServer` dispatches inline on the per-connection thread and
  the forwarding handler BLOCKS on the outbound round-trip. Different players (different QUIC connections) don't block
  each other, but within one connection there's no pipelining (head-of-line). A real gateway needs a worker-pool
  `EdgeServer` dispatch — OUT of v1.
- **The hop is accepted for v1** (client→gateway→service, +re-crypt) for the single-entry benefit. Future: co-locate,
  or QUIC connection migration (hand the client a direct endpoint post-handshake).
- **Admin fan-out is UNCHANGED** (additive v1). The gateway fronts HTTP, but admin still fans out internally — so in
  the split there's gateway+fan-out both. Removing that redundancy is the **portal-shell** model (gateway routes
  `/admin/<mod>` per-service, admin becomes shell-only) — a separate, bigger, non-additive refactor (duplicate the
  shell per service OR compose in the gateway; solve cross-service nav aggregation). Deliberately deferred.
- **Out of v1:** push/streaming relay (request/response only); worker-pool concurrency; connection migration;
  portal-shell admin. Each a clean additive follow-up.

## Ceiling
This makes the split have a real single front door for BOTH protocols, in-code (no external nginx), reusing the
`edge` stack. It does NOT yet make the gateway a production multiplexer (serial per-connection) — that's the
worker-pool follow-up.

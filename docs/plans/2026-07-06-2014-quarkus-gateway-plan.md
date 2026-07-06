# Plan: external `gateway` module — QUIC-RPC router + HTTP reverse-proxy

> Branch `quarkus-per-service` (`experiments/jvm-quarkus-sketch/`). A single external front door for the split:
> routes the custom **QUIC edge RPC** (players) by method-prefix to the owning service, AND HTTP via a reverse proxy.
> Motivation: a gateway is unavoidable for the split, and one that can't route our QUIC is pointless — QUIC routing
> is the core. Backed by 6 research subagents.
>
> **v2 — reworked after a grumpy-reviewer pass (think-hard) that verified against the code and killed three premises:**
> (1) "byte-relay round-trips unchanged" is FALSE — `EdgeClient.request` msgpack-encodes its payload, so relaying a
> `ByteArray` double-encodes (bin-wraps) it and the downstream `typedHandler` decode fails; needs a new raw path.
> (2) "reuse Stork" is fabricated — the programmatic Stork API is used NOWHERE; the QUIC seam uses plain `host:port`
> config; routing table is now plain config, not Stork. (3) The only QUIC method (`characters.ownerOf`) is an INTERNAL
> module→module seam, not player-facing — routing it demos the wrong thing; v1 now adds genuine player-facing methods.
> Also dropped the MsQuicLibrary hoist (it invalidated Step-1's smoke and wasn't additive). Full resolution at bottom.

## Context — the reuse story (corrected) + the additive verdict

The gateway recomposes the existing `edge` stack, but with TWO honest new edits (not the "zero edits" v1 claimed):
- **QUIC side = an external `EdgeServer` whose handlers forward via cached `EdgeClient`s to internal services**,
  routed by method-prefix. This IS the forwarding variant of the existing inventory→characters pattern.
  - **Byte-relay needs a NEW raw path (reviewer #1).** `EdgeClient.request(method, payloadObj: Any)` does
    `codec.encodePayload(payloadObj)` = `mapper.writeValueAsBytes` — for a `ByteArray` that emits a msgpack **bin with
    a length header**, NOT the raw bytes, so forwarding the inbound payload bytes through `request(...)` double-encodes
    them and the downstream `typedHandler.decodePayload(...)` fails. Fix: add `EdgeClient.requestRaw(method,
    payloadBytes)` (and matching `requestRawWithCid`) that sends `Request(cid, method, payloadBytes)` bypassing
    `encodePayload`. The reply leg is already raw (`Response.payload: ByteArray`, `EdgeCodecTest` proves it round-trips),
    so only the request leg needs the raw path. This is a real, small, backward-compatible `EdgeClient` addition — Step 2.
  - **cid: no remapping, and here's WHY it's safe (reviewer #4):** the forwarding handler calls the cached outbound
    `EdgeClient`, which mints a FRESH cid on its own `cidGen`/`pending` (concurrent-safe); `EdgeRouter.dispatch`
    re-wraps the returned bytes in `Response(inboundReq.cid, …)`. Independent cid spaces per hop + a concurrent-safe
    outbound `pending` map → no collision even when many inbound connections share one cached outbound client.
- **The FFM binding stays written ONCE in `edge`** — the gateway imports and uses `MsQuicServerTransport` +
  `MsQuicClientTransport`. **v1 does NOT hoist a shared `MsQuicLibrary`** (reviewer #6/#7): the per-transport-lib model
  is what Step-1 smokes and what characters/inventory already run; hoisting would ship an un-smoked native init path
  AND edit shared transport ctors. The double-DLL-load is a noted inefficiency for a FUTURE optimization, not v1.
- **Routing table = plain `host:port` config, NOT Stork (reviewer #2).** The QUIC seam already resolves targets via
  `edge.client.characters.target=${CHARACTERS_EDGE_ADDR:localhost:9100}` (a plain host:port), proven by
  `EdgeRemotePlayerCharacters`. The gateway reuses THAT existing seam: a config map `gateway.route.<prefix>.target=
  host:port` fed by the same `CHARACTERS_ADDR`/`CHARACTERS_EDGE_ADDR`/`INVENTORY_ADDR` envs install.ps1 already sets.
  Programmatic Stork is NOT used anywhere in the repo — do not introduce it as "reuse".
- **HTTP side = Vert.x `HttpProxy`** (`io.vertx:vertx-http-proxy:4.5.28` — the exact Vert.x line Quarkus 3.37.1
  bundles; pin it, 5.x clashes), mounted via Quarkus `@Route`, origin from the SAME plain `host:port` config (not
  Stork). Hop-by-hop/WebSocket/streaming handled for free.

**Additive verdict (corrected):** additive at the MODULE level (new `gateway`+`gateway-service`, no edits to
feature-impl modules or the services' wiring). Two honest edits to SHARED `edge`: `EdgeClient.requestRaw` (new method,
backward-compatible) and `EdgeRouter` prefix routing (new method, exact-match path untouched). NOT edits to
characters/inventory/accounts/admin. install.ps1 gains a 3rd process; the composition-rule gate gains one entry.

## Scope — v1, and the two decisions for the user

**v1 = additive external front door that ACTUALLY demonstrates a player gateway:**
1. **A genuine player-facing QUIC method per service** (reviewer #3): the sketch today has only the internal
   `characters.ownerOf`. Add **`characters.list`** (player-facing: given a playerId, return that player's characters)
   on characters-service's EdgeServer, and **`inventory.list`** (a player's holdings) on an inventory EdgeServer.
   These are real read methods a game client would call — NOT internal seams. (inventory has no EdgeServer today; it
   gets a small one, gated like `CharactersEdgeServer`.)
2. **QUIC-RPC routing**: the gateway's external `EdgeServer` prefix-routes `characters.* → characters-service`,
   `inventory.* → inventory-service`, byte-relaying via `requestRaw`. A test **player client** dials the GATEWAY and
   calls both — proving the gateway routes each prefix to the right service. THIS is the marquee (not routing an
   internal seam).
3. **HTTP reverse-proxy**: `/admin/*`, `/characters/*`, `/inventory/*` → owning service via HttpProxy + config origin.
4. Admin fan-out **unchanged** (future (a)).

**Decision 1 (§scope-fork, at approval): portal-shell admin?** Research: making the gateway SUBSUME admin fan-out
(gateway routes `/admin/<mod>` per-service, admin becomes shell-only) is a **separate, bigger, non-additive** refactor
(duplicate the shell template per service OR move composition into the gateway; solve cross-service nav aggregation).
**Recommendation: NO for v1** — keep admin fan-out; the gateway just fronts HTTP. The "gateway subsumes fan-out"
intuition is right in principle but is the (b) refactor, a future plan. (Yes, v1 has gateway+fan-out both; that
redundancy is only removed by (b).)

**Decision 2 (at approval): the hop.** The QUIC edge exists for low-latency player transport; a terminating gateway
adds a hop + re-crypt. v1 accepts it for the single-entry/auth/TLS benefit. Future mitigations (out of v1): co-locate,
QUIC connection migration. **Confirm the hop is acceptable for v1.**

**Out of v1 (noted):** push/streaming relay (real plumbing gap — request/response only); portal-shell admin;
connection-migration; a worker-pool `EdgeServer` (v1 is per-connection serial — see the honest limit below).

## Honest limits (surface, don't bury)
- **v1 proves routing CORRECTNESS, not gateway CONCURRENCY (reviewer #5).** `EdgeServer` dispatches inline on the
  per-connection thread and the forwarding handler BLOCKS on the outbound round-trip. Different players (different QUIC
  connections) don't block each other, but within one connection there's no pipelining (head-of-line). A real gateway
  needs a worker-pool dispatch (an `EdgeServer` change) before "gateway" means anything under load — explicitly OUT of
  v1, and the word "gateway" here means "correct prefix router," not "concurrent multiplexer".
- **Tech-Preview deps** (`vertx-http-proxy`, and Stork is preview) — pin `vertx-http-proxy` to 4.5.28 exactly.
- **Cert** for the gateway's QUIC server (CurrentUser store via `ensure-cert.ps1`), same constraint as the edge server.

---

## Implementation sequence

### Step 0 — persist plan `[inline]`. Stay on `quarkus-per-service`.

### Step 1 — SMOKE the asymmetric server+client-forward in one JVM `[opus]`, think hard
**(a)** Extend `edge`'s msquic tests: one JVM runs an external `MsQuicServerTransport` whose handler calls OUT via a
`MsQuicClientTransport` to a SECOND `MsQuicServerTransport` (stand-in service) and relays the reply — 3 transports,
real QUIC round-trip, each with **its own `MsQuicLibrary`** (the CURRENT per-transport model — no hoist). Assert data
flows and teardown across two registrations doesn't hang.
**(b) Why first:** the ONE unproven native assumption the gateway rests on. **(c)** `assumeTrue`-gate on the cert like
`MsQuicEchoTest`. **HONEST GAP (reviewer #6):** this smoke does NOT cover N concurrent inbound connections, config
resolution, or cached-client reuse under load — it de-risks "can one process forward server→client at all", not
Step-3's concurrency. State that in the test doc. **(d) `[opus]`** — native/concurrency.
**Verify:** the 3-transport forward passes; teardown clean; a few iterations.

### Step 2 — `EdgeClient.requestRaw` + `EdgeRouter` prefix routing + `ForwardingHandler` `[opus]`, think hard
**(a)** `edge/EdgeClient.kt`: add `requestRaw(method, payloadBytes: ByteArray, timeout)` (+ `requestRawWithCid`) that
sends `Request(cid, method, payloadBytes)` WITHOUT `encodePayload` — the byte-relay fix. `edge/EdgeRouter.kt`: add
`registerPrefix(prefix, handler)` + a longest-prefix scan in `dispatch` AFTER the exact-match miss (exact still wins;
unknown still errors). A `ForwardingHandler` (in `edge` or `gateway`): `EdgeHandler { payload -> outboundClient
.requestRaw(method, payload, budget).let { if (!it.ok) throw ...; it.payload } }`, reusing the
`CachedResource`+bounded-retry pattern from `characters-client` for the outbound `EdgeClient`.
**(b) Why now:** the QUIC routing core; independent of HTTP. **(c)** Map BOTH `ConnectionClosedException` AND
`TimeoutException` from the downstream to `Response(ok=false, error=…)` (reviewer #8); give the outbound forward a
SHORTER timeout budget than the inbound expects, so the client sees a clean `ok=false`, not a bare timeout.
**(d) `[opus]`** — dispatch-core + a raw wire path, correctness-bearing.
**Verify (reviewer #1 — must not mask the bug):** the forwarding test's DOWNSTREAM uses a REAL `codec.typedHandler`
(decodes a typed request), NOT a raw echo — proving `requestRaw` delivers bytes the typed decoder accepts. Unit-test
prefix dispatch (exact wins; prefix matches; longest-prefix; no-match errors); `EdgeLoopbackTest` + `EdgeCodecTest`
stay green.

### Step 3 — player-facing methods + the `gateway` module (QUIC side) + `gateway-service` `[opus]`, think hard
**(a)** Add player-facing QUIC methods: `characters.list(playerId) → [characters]` on `CharactersEdgeServer`
(register alongside `ownerOf`); a small `InventoryEdgeServer` (gated like `CharactersEdgeServer`) serving
`inventory.list(owner) → [holdings]`. New `gateway` impl module: a `GatewayEdgeServer` (`@Observes StartupEvent`,
role-gated) building an `EdgeRouter` with prefix forwarders (`characters.*`, `inventory.*`), each owning a cached
outbound `EdgeClient` whose target host:port comes from PLAIN config (`gateway.route.characters.target=
${CHARACTERS_EDGE_ADDR:localhost:9100}`, etc.), on the gateway's external QUIC port. New `gateway-service` app-shell
(`io.quarkus`; deps `gateway`+`edge`+`platform`+health; NO feature impls; `roles=gateway`).
**(b) Why now:** depends on Step 2 (raw path + prefix router) + Step 1 (proven forward). **(c)** Routing table in
config (host:port), resolved at startup/first-call — NO Stork. NO MsQuicLibrary hoist. **(d) `[opus]`** — the crux
module + two new player methods.
**Verify:** in-JVM (or loopback), a test player client → gateway → `characters.list` routed to characters-service,
`inventory.list` to inventory-service, both return correct data; a down backend → clean `ok=false`, not a hang.

### Step 4 — HTTP reverse-proxy side `[sonnet]`, think hard
**(a)** In `gateway`: `vertx-http-proxy:4.5.28` (pinned) + `quarkus-rest`. `@Route(regex="/admin/.*")` etc. delegating
to per-prefix `HttpProxy` instances with a STATIC config origin (host:port from the same env config; no Stork).
**(b)** cheap, completes "one front door". **(c)** Pin 4.5.28 (5.x clashes with Quarkus's Vert.x 4.5). **VERIFY the
actual proxied path (reviewer #9):** confirm the backend serves the path the proxy forwards (e.g. `/admin/...` verbatim
vs a rewrite) — assert on the real proxied request, not just "200". **(d) `[sonnet]`** — wiring; the version pin +
path check are the gotchas.
**Verify:** `GET <gateway>/admin/...` reaches the admin host with the expected path; `/characters/*` reaches
characters-service; hop-by-hop headers correct.

### Step 5 — install.ps1 + composition-rule gate + config `[sonnet]`
**(a)** install.ps1: append `gateway` to `$topology` (jar, external HTTP+QUIC ports), a build line, an env block
(`CHARACTERS_ADDR`/`CHARACTERS_EDGE_ADDR`/`INVENTORY_ADDR`/`EDGE_CERT_THUMBPRINT`); generic loops unchanged; monolith
untouched (gateway only in split). Root `build.gradle.kts` composition rule: `:gateway-service` forbids the feature
impls (additive map entry). **(b)** deploys the 3rd process. **(d) `[sonnet]`**.
**Verify:** `./gradlew build` green (composition rule passes); microservices mode launches 3 processes, all
`/q/health/ready`.

### Step 6 — end-to-end smoke (the marquee) `[opus]` if scripted, else `[inline]`
`install.ps1 -Mode microservices` (3 processes). A test PLAYER client dials the GATEWAY's QUIC and calls
`characters.list` + `inventory.list` → routed to the right services, correct data. Kill characters-service → gateway
returns clean unavailable (not hang). `GET <gateway>/admin` proxies through. This proves "external front door routes
BOTH protocols to the right service" — the real point.
**Verify:** both player calls route correctly; degradation clean; HTTP proxied.

### Step 7 — docs + per-step commits `[inline]`
`docs/reference/`: gateway architecture (QUIC router = EdgeServer-of-EdgeClients via `requestRaw` + HTTP proxy), the
config-based routing (not Stork), the hop/serial-blocking limits, and "portal-shell admin = future (b)". Commit per step.

## Risks / conscious tradeoffs
- **Byte-relay needs `requestRaw`** (the v1 double-encode blocker) — Step 2 adds it; the forward test uses a REAL
  typed downstream so it can't pass while broken.
- **Server+client-forward is unproven** → Step 1 smokes it first (per-transport-lib, no hoist); pauses the plan if it
  deadlocks. The smoke does NOT cover concurrency/load — stated.
- **v1 = routing correctness, NOT concurrency** — per-connection serial blocking; worker-pool `EdgeServer` is future.
- **Hop + latency** — accepted for v1; migration/co-location future. §Decision-2 confirms.
- **No Stork** — routing table is plain host:port config (the existing `edge.client.*.target` seam); do NOT introduce
  programmatic Stork as "reuse" (it isn't used anywhere).
- **No MsQuicLibrary hoist in v1** — keeps Step-1's smoke valid and avoids editing shared transports; double-DLL-load
  noted as a future optimization.
- **Tech-Preview `vertx-http-proxy`** pinned to 4.5.28; **cert** required (assumeTrue-gate the unit smoke).
- **Admin redundancy + portal-shell** — §Decision-1; kept as future (b), surfaced not buried.

## Dispatch summary (for approval)
Step 1 `[opus]` · Step 2 `[opus]` · Step 3 `[opus]` · Step 4 `[sonnet]` · Step 5 `[sonnet]` · Step 6 `[opus]`/`[inline]`
· Step 7 `[inline]`. Commit per step; deliberate-break/verify on each crown behavior. **User decides at approval:**
(1) v1 additive scope (keep admin fan-out) vs portal-shell now; (2) the hop is acceptable for v1.

## Grumpy-reviewer punch-list resolution
- **#1 (BLOCKER) byte-relay double-encodes** → add `EdgeClient.requestRaw` (Step 2); the forward test's downstream is a
  REAL `typedHandler` so it can't pass while broken. Context/Steps corrected.
- **#2 (BLOCKER) Stork-reuse fabricated** → routing table is plain `host:port` config reusing the existing
  `edge.client.*.target` seam; programmatic Stork removed entirely (QUIC + HTTP both use config origins).
- **#3 (BLOCKER) demos an internal seam** → v1 adds genuine player-facing methods (`characters.list`, `inventory.list`)
  + a small inventory EdgeServer; a test PLAYER client routes both through the gateway. The internal `ownerOf` is NOT
  repointed (stays additive).
- **#4 (SHOULD) cid correct-but-unexplained** → Context now states WHY (independent cid spaces + concurrent-safe
  outbound `pending`).
- **#5 (SHOULD) serial blocking** → "Honest limits": v1 proves routing correctness not concurrency; worker-pool = future.
- **#6/#7 (SHOULD) hoist invalidates smoke + edits shared transports** → MsQuicLibrary hoist DROPPED from v1; Step 1
  smokes the per-transport model that ships; double-DLL-load noted as future.
- **#8 (NIT) timeout stacking** → outbound forward gets a shorter budget; both `ConnectionClosedException` and
  `TimeoutException` → `Response(ok=false)` (Step 2).
- **#9 (NIT) HttpProxy path unverified** → Step 4 verifies the actual proxied path, not just a 200.

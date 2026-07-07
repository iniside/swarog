# Status — gateway-svc single front door: the double-layer is CLOSED (Step G2)

**Date:** 2026-07-07 21:45 · **Plan:** `docs/plans/2026-07-07-1815-unified-operation-transport-plan.md` (F1 gateway-svc note)
**Supersedes the "HONEST remaining gap" of** `docs/2026-07-07-2015-unified-operation-transport-status.md`.

The single documented gap from the F1 close-out — `cmd/gateway-svc` HTTP-reverse-proxying player
routes to the backends' own gateway front-handlers (a functional double-layer) — is now **closed**.
`gateway-svc` is the SINGLE gateway: it dispatches each player operation to the owning backend over
the mutually-authenticated QUIC edge via `gateway.RemoteBackend`, one hop. The G1 step (RemoteBackend
round-trips every op shape, proven by the parity tests) made this possible; G2 wires gateway-svc onto it.

---

## What changed

### 1. Impl-free route bindings (rpcgen + opsapi)

`gateway-svc` hosts no module, so it cannot read `ctx.Contributions(opsapi.Slot)` and has no service to
bind a `LocalOp` to. It needs the route table + HTTP↔wire bindings **statically, without an impl**.

- New leaf type `opsapi.RouteBinding{Operation, Binding}` — the impl-free subset of an `OpSet` (the
  static `Operation` + its `OpBinding` Decode/NewResp/Encode), carrying **no** `LocalOp`.
- `tools/rpcgen` now emits, per HTTP-bound interface, `func RouteBindings() []opsapi.RouteBinding`
  alongside the existing `Operations(impl)`. The `Operation`/`OpBinding` literals are byte-identical to
  the `Operations(impl)` entries (both reuse the generated `decodeX`/`encodeX`, which never referenced
  the impl), so the two tables describe the same routes — one source of truth. Regenerated all glue;
  `rpcgen -check` green.

### 2. gateway-svc builds its route table + dispatches over the edge

`cmd/gateway-svc/main.go` (rewritten):

- **Route table:** collects `charactersplayerrpc.RouteBindings()` + `accountsauthrpc.RouteBindings()` +
  `inventoryrpc.RouteBindings()` and builds an op mux via the new
  `gateway.NewOpsMux(routes, backendFor, sessions)` — the SAME `newOpHandler` (decode → auth →
  dispatch → status→HTTP) the in-process gateway module uses, so the two front doors behave identically.
  `leaderboard`/`match` are monolith-only (no split service hosts them) — their rpc packages are **not**
  imported and their ops are **not** routed. Confirmed and skipped cleanly.
- **Auth once, over the edge:** for an `AuthPlayer` op the front-handler verifies the bearer by calling
  `accounts.verifySession` over the edge to the accounts peer — `accountsrpc.NewClient(accRouted)`, where
  `accRouted` is a self-healing `RoutedBackend` to `ACCOUNTS_EDGE_ADDR` (= `CHARACTERS_EDGE_ADDR`, accounts
  is co-hosted in characters-svc). It satisfies `gateway.SessionVerifier` structurally. On failure → 401.
  The resolved `player_id` is injected via `opsapi.WithPlayerID` and rides the op envelope's `Identity`.
- **Peer routing:** one `gateway.RemoteBackend` per peer, keyed by method prefix — `characters.*` /
  `accounts.*` → `CHARACTERS_EDGE_ADDR`; `inventory.*` → `INVENTORY_EDGE_ADDR`. Each RemoteBackend shares
  the peer's self-healing `RoutedBackend` (new `gateway.NewRemoteBackendRelay` + a `Call` method on
  `RoutedBackend` that makes it an `opsapi.Caller`), so op dispatch and auth to A share one edge conn.
  This is a SINGLE hop: gateway-svc → backend edge op. No more HTTP proxy → backend front-handler.

### 3. What stayed HTTP-native passthrough (reverse proxy)

Not operations (HTTP-shaped: HTML, browser redirects), so they stay reverse-proxy to the owning backend:

- `/admin*` → inventory-svc HTTP (admin HTML).
- `/accounts/epic/*` → characters-svc HTTP (Epic OAuth start/callback).

`/characters` and `/inventory` are **removed** from the proxy map — they are edge ops now. gateway-svc
keeps its own `/healthz`+`/readyz`, per-IP rate limiting, and the player-facing `:9100` QUIC prefix
front (native-client scope; the RoutedBackends are shared with it).

### 4. Env + arch-lint

- `run.sh`/`run.ps1`: added `ACCOUNTS_EDGE_ADDR=localhost:9000` to the gateway-svc env. `EDGE_CA_CERT`/
  `EDGE_CA_KEY` and the edge addrs were already passed, so gateway-svc dials the backends' mutually-
  authenticated edge.
- `.go-arch-lint.yml`: `cmdGatewaySvc.mayDependOn` gains `gatewaymod`, `charactersplayerrpc`,
  `accountsauthrpc`, `accountsrpc`, `inventoryrpc` (the impl-free glue) — never a module impl.

---

## The single-hop proof (`scripts/smoke-split-operations.sh`, rewritten, committed)

Boots the full split (A characters-svc, B inventory-svc, scheduler-svc, C gateway-svc) and drives the
player flow entirely through `:8082`, asserting every op is an edge single-hop:

1. **REGISTER over the edge** — `POST :8082/accounts/register` → **201**. `/accounts` is NOT in
   gateway-svc's proxy map (only `/admin`, `/accounts/epic`), so the only way this returns 201 is the op
   table dispatching `accounts.register` over the edge to A. This is the decisive proof: it was **404**
   before (accounts un-proxied); it is 201 now (edge op). The double-layer is gone.
2. **CREATE** — `POST :8082/characters` → 201 + id (`characters.create` edge op on A, auth once at front).
3. **LIST** — `GET :8082/characters` → 200 lists it back.
4. **CROSS-PROCESS mTLS-edge auth + sync op** — `GET :8082/inventory/character/{id}`: gateway-svc verifies
   the bearer over the edge to A, dispatches `inventory.listCharacter` to B's edge, whose op sync-asks
   `characters.OwnerOf` over the edge to A. Starter grant `starter_sword` returns → both hops traversed
   the mTLS edge to DIFFERENT processes, all fronted by the single gateway.
5. **DELETE** — `DELETE :8082/characters/{id}` → 204.

`scripts/smoke-split-messaging.sh` → **PASS** (durable event plane untouched).

## Gate evidence — `verify.ps1 -All`

| Stage | Status | Blocking |
|---|---|---|
| build, vet, golangci-lint, go-arch-lint, test, govulncheck | **PASS** | true |
| test-race | SKIP (no cgo/gcc) | false |
| fuzz (4), apidiff, topiccheck, rpcgen `-check` | **PASS** | false |

`VERIFY OK`. apidiff PASS — the opsapi change (`RouteBinding` type) and the generated `RouteBindings()`
funcs are purely additive.

---

## What is now unified vs still remaining

- **Unified:** the internal operation surface, the generated glue, auth-once-over-mTLS, admin fan-out,
  the gateway front inside each `app.Run` process (monolith + each backend), **and now `cmd/gateway-svc`
  → backend dispatch**: player ops enter the single gateway and cross to the owning peer as one edge hop.
  The double-layer (gateway-svc HTTP proxy → backend front-handler) is removed.
- **Legitimately still HTTP passthrough (by design):** admin HTML (`/admin*`) and Epic OAuth
  (`/accounts/epic/*`) — HTTP-native, cannot be edge ops.
- **Still future scope (unchanged, honest):** the player-facing `:9100` **QUIC** front is a prefix
  relay for native clients and does **not** yet authenticate player-facing QUIC calls (no bearer→
  identity at the QUIC edge). The HTTP front door (:8082) is the authenticated single gateway; native-
  client QUIC auth remains to be designed.

## Files of record

- `opsapi/opsapi.go` (RouteBinding), `tools/rpcgen/main.go` (writeRouteBindings) + all regenerated
  `<module>rpc_gen.go`.
- `modules/gateway/gateway.go` (NewOpsMux, exported SessionVerifier), `modules/gateway/backend.go`
  (NewRemoteBackendRelay), `gateway/routed_backend.go` (Call → opsapi.Caller).
- `cmd/gateway-svc/main.go` (rewritten), `.go-arch-lint.yml`, `run.sh`, `run.ps1`,
  `scripts/smoke-split-operations.sh` (rewritten proof).

# Status — Unified operation transport (Step F1, final)

**Date:** 2026-07-07 20:15 · **Plan:** `docs/plans/2026-07-07-1815-unified-operation-transport-plan.md`
**Phase F / Step F1 — HTTP-native homing, `/events` bypass, final verification.** Phases 0–E done.

This is the honest close-out: what the program actually delivered, the gate evidence, and — precisely
— what is **not** yet unified. It deliberately does not claim "full parity."

---

## What the program delivered

1. **One internal operation surface.** The 12 player operations (characters ×3, inventory ×3,
   accounts ×4, leaderboard, match/report) are declared once as typed `opsapi.Operation`s and
   served through a single gateway front-handler → `OperationBackend` → provider path. A module no
   longer hand-writes a player HTTP handler *and* a QUIC edge handler *and* an admin-data handler for
   the same capability.

2. **Generated glue killed the hand-copy debt.** `tools/rpcgen` synthesizes each `<module>rpc`
   client/server/envelope from the pure `<module>api` interface. The old per-method hand-kept pieces
   (mirrored DTOs in `remote.go`, method-name strings ×2, provider adapter, the two
   `wire_contract_test.go` byte-comparisons) are gone; wire drift is now structurally impossible and
   guarded by the `rpcgen -check` regen-diff (verify advisory stage).

3. **Gateway fronts the port in BOTH topologies via a leaf slot.** The `gateway` module Contributes a
   front-handler to `httpmw.FrontHandlerSlot`; `internal/app` wraps `ctx.Mux` with it without ever
   importing `gateway` (decision D4). So the monolith (`cmd/server`) and every split svc run the same
   front, and `app` stays topology-blind (arch-lint clean).

4. **Auth once, at the front door, over mTLS.** For an `AuthPlayer` op the front-handler verifies the
   bearer via the accounts `Sessions` capability and injects the resolved `player_id` into ctx
   (`opsapi.WithPlayerID`); domain ops read identity only from ctx/envelope, never from the client.
   The triplicated inline auth (`characters.go`/`inventory.go`/`accounts.go`) is deleted. The edge hop
   is mutually authenticated (pinned CA-signed client leaf, `edge.ClientMTLS`), so the injected
   identity is trustworthy across a process boundary.

5. **Admin fan-out folded onto the operation plane.** Each module's admin content is an
   `AdminData` operation over the same generated glue; the `/admin-data/<id>` HTTP handlers,
   `admin.Module.http`/`fetchRemote`, and `PEER_HTTP_ADDR`/`peerAdminURL` are deleted. In the split,
   `/admin` fans out over the SAME mTLS QUIC edge — no separate HTTP admin hop.

6. **HTTP-native routes stay HTTP (passthrough), verified.** OAuth (`POST /accounts/epic/start`,
   `GET /accounts/epic/callback`), admin HTML (`/admin`, `/admin/{slug}`, `/admin/theme.css`), the
   webui SPA (`/`), and infra (`/healthz`/`/readyz`/`/metrics`) are never in the op route table; the
   front-handler passes them through to `ctx.Mux` (health/metrics are owned by `app.Run`). Confirmed
   by driving the monolith (below).

7. **`/events` bypasses the gateway (D7).** The messaging inbound sink `POST /events` is registered on
   `ctx.Mux` (`modules/messaging/messaging.go:194`), NOT contributed to `opsapi.Slot`. No
   `opsapi.Operation` declares path `/events` (checked across all `ctx.Contribute(opsapi.Slot, …)`
   sites). Inter-service messaging POSTs peer→backend `/events` directly (`EVENTS_SUBSCRIBERS` URLs),
   never through the gateway.

---

## Gate evidence

### HTTP-native passthrough (monolith :8080, drive)

| Route | Result |
|---|---|
| `GET /` (webui SPA) | **200** |
| `GET /admin` | **302** (basic-auth gate redirect) |
| `GET /admin/theme.css` | **200** |
| `GET /healthz` / `GET /readyz` / `GET /metrics` | **200** / **200** / **200** |
| `GET /characters` (AuthPlayer op, no bearer) | **401** ← op table owns it |
| `POST /accounts/epic/start` (EPIC unset) | **405** ← `ctx.Mux` webui `GET /` catch-all, wrong method — NOT the op table |

The op route table intercepts only the 12 op routes: an unauth'd op returns **401** (gateway auth),
while the epic route returns a plain mux **405** — proving the op table does not shadow HTTP-native
routes. Epic OAuth handlers are mounted on `ctx.Mux` only when `EPIC_CLIENT_ID`+`EPIC_CLIENT_SECRET`
are set (`accounts.go:170-171`); unset, they are absent and pass through — the op table has no
`/accounts/epic/*` entry either way.

### `verify.ps1 -All`

| Stage | Status | Blocking |
|---|---|---|
| build, vet, golangci-lint, go-arch-lint, test, govulncheck | **PASS** | true |
| test-race | SKIP (no cgo/gcc) | false |
| fuzz (4 targets) | **PASS** | false |
| apidiff | **PASS** | false |
| topiccheck | **PASS** | false |
| rpcgen `-check` | **PASS** | false |

`VERIFY OK`. (apidiff PASSED — its base is HEAD, so the already-committed `adminapi.Item` change from
Phase E is not re-flagged.)

### Smokes (microservices split)

- `scripts/smoke-split-messaging.sh` → **PASS** (durable event plane cross-process; single-owner relay
  holds).
- `scripts/smoke-split-operations.sh` (**new, committed**) → **PASS**: registers on A, then drives
  create → list → inventory → delete THROUGH the gateway-svc front door (:8082). The inventory read
  proves the cross-process **mTLS-edge auth path**: B has no local accounts/characters, so its
  front-handler verifies the bearer over the edge to A and the inventory op sync-asks
  `characters.OwnerOf` over that same edge before returning the starter grant.

---

## The HONEST remaining gap — do not overclaim

**`cmd/gateway-svc` (:8082) still double-layers.** Today, in the split, the dedicated player front-door
process HTTP-reverse-proxies `/characters`, `/inventory`, `/admin` to the backends' HTTP ports
(`cmd/gateway-svc/main.go:113-117`). Those backends then serve the op through their OWN gateway
front-handler (the leaf-slot front from B1). So an external player request travels:

```
player → gateway-svc HTTP proxy → backend:port → backend's front-handler → op (LocalBackend)
```

This **works and is behavior-preserving** (proven by `smoke-split-operations.sh` end-to-end via :8082),
but it is a **functional double-layer**, not the fully-unified single-gateway end-state in which
`gateway-svc` would itself dispatch each op to the owning backend via a `RemoteBackend` over the edge
(`gateway.OperationBackend`), with the backends exposing ops purely as edge servers.

Precisely what is and isn't unified:

- **Unified:** the *internal* operation surface, the generated glue, auth-once-over-mTLS, admin
  fan-out, and the gateway front *inside each app.Run process* (monolith + each backend svc).
- **NOT yet unified:** `cmd/gateway-svc` → backend dispatch. It uses HTTP reverse-proxy + the
  backend's own front-handler, not `gateway.RemoteBackend` op dispatch. The generic-remote wire path
  (`selectBackend`'s remote branch + `peerAddrFor` are currently shape-only stubs —
  `gateway.go:140-161`) is the remaining work to collapse the double-layer into one gateway.
- **Also remaining (future):** `/accounts/*` is NOT in gateway-svc's proxy map, so accounts is
  reachable only on backend A's own port (confirmed: `POST :8082/accounts/register` → 404). The
  player-facing `:9100` QUIC front (native-client auth) is future scope.

### The remaining unification, stated concretely

1. Wire split peer addressing: `peerAddrFor(provider)` resolves the owning peer's edge address from
   the split's ROLES/peer config (today returns `""`).
2. `selectBackend` then returns a `RemoteBackend` for a not-co-hosted provider, and `gateway-svc`
   runs the same `frontmw` front-handler (or a thin equivalent) so it dispatches ops via
   `RemoteBackend` over the mTLS edge instead of HTTP-reverse-proxying to the backend's front.
3. Drop the HTTP reverse-proxy map from `cmd/gateway-svc` once every fronted route is an op; keep the
   HTTP proxy only for the HTTP-native routes (admin HTML, OAuth callback) that legitimately stay HTTP.

Until (1)–(3) land, the split's external entry is the double-layer above — correct, tested, but not
single-gateway.

---

## Files of record

- New: `scripts/smoke-split-operations.sh` (committed repeatable split-op proof).
- This status doc.
- No stale-doc fix was needed: `docs/reference/testing.md`'s `admin-data`/`/admin-data` references
  describe the **Quarkus JVM sketch** (`experiments/jvm-quarkus-sketch/`), a separate experiment that
  still exposes those endpoints — they are accurate there, not stale Go-backend references. The Go
  Phase-E deletions (`/admin-data`, `PEER_HTTP_ADDR`) are correctly absent from the Go reference docs
  (`docs/reference/gateway.md`, `edge-gateway-quic.md`) and `CLAUDE.md`.

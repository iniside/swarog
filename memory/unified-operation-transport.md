---
name: unified-operation-transport
description: Sync module capabilities are typed operations (opsapi/rpcgen-generated glue); the gateway fronts every process and is the client; auth once over mTLS edge
metadata: 
  node_type: memory
  type: project
  originSessionId: 9daf9937-49a2-46ca-88f2-a2c9a48ebd40
---

The sync plane got the same "one topology-transparent surface" treatment as the async
plane (2026-07-07, plan `docs/plans/2026-07-07-1815-unified-operation-transport-plan.md`,
Phases 0–F; status `docs/2026-07-07-2015-unified-operation-transport-status.md`). Do NOT
re-introduce hand-written edge clients/adapters/mirrored-DTOs/`wire_contract_test`, inline
per-handler bearer auth, or per-module player HTTP routes.

**What exists now:**
- **`opsapi` (leaf):** `Caller` transport seam, `Operation{Method,Verb,Path,Auth}` + `Slot`
  (modules Contribute their operations), `LocalOp`/`LocalSlot`, `Status` error taxonomy
  (OK/NotFound/Forbidden/Invalid/Unauthorized/Conflict/Unavailable/Internal → HTTP), and
  `WithPlayerID(ctx)/PlayerID(ctx)` — identity is set ONLY by the gateway, read ONLY from ctx.
- **`tools/rpcgen`:** go/types generator (mirrors topiccheck). A provider declares a pure
  capability interface in `modules/<name>/<name>api/` (codegen input, transport-free, in the
  `contracts` tier); rpcgen emits `modules/<name>/<name>rpc/` (client-over-`Caller` + edge
  server adapter + envelopes). `//go:generate` + a `verify` `rpcgen -check` drift gate
  (gofmt-normalized) replaces the old byte-pinned wire tests. Consumers KEEP their own local
  interfaces (rule 4) — the generated client structurally satisfies them.
- **Gateway = a lifecycle module in EVERY process**, fronting `ctx.Mux` via the leaf
  `httpmw.FrontHandlerSlot` (so `internal/app` never imports `gateway`). `OperationBackend`:
  `LocalBackend` (registry-resolved typed call, no wire marshal — monolith) / `RemoteBackend`
  (edge). Route table built from `opsapi.Slot`. All 12 player ops + admin fan-out go through it.
- **Auth once at the gateway:** verifies the bearer via `accountsapi.Sessions.VerifySession`,
  injects `player_id` into the op envelope/ctx. Safe because the **edge is mutual-TLS** (Phase
  C, `edge.DevCA` shared via `EDGE_CA_CERT/KEY`; unauthenticated dial rejected — proven by
  `edge/mtls_test.go`). Monolith uses nil edge (no mTLS needed).
- **Admin fan-out folded:** `<module>.adminData` is an edge op; `adminapi.Item.RemoteURL`→
  `RemoteFetch func`. `/admin-data/*` HTTP + `PEER_HTTP_ADDR` deleted.
- **`/events` (async plane) BYPASSES the gateway** by design — peer→backend direct, on ctx.Mux.
- Proof: `scripts/smoke-split-operations.sh` (committed, split op path over mTLS edge).

**HONEST remaining gap (do NOT claim "full parity" is done):** `cmd/gateway-svc` (the split
front-door :8082) still HTTP-reverse-proxies `/characters,/inventory,/admin` to backends,
which serve the ops via their OWN gateway front-handler — a functional DOUBLE-LAYER, not the
single-gateway end-state where gateway-svc dispatches ops via `RemoteBackend` over the edge
(`selectBackend`'s remote branch + `peerAddrFor` are shape-only stubs). The generic
gateway-svc→backend-op wire path + player-facing :9100 QUIC auth are the remaining unification.
See [[durable-event-plane-bus-owned]], [[scope-claims-to-what-was-verified]].

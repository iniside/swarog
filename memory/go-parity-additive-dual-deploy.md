---
name: go-parity-additive-dual-deploy
description: "The Go backend reached full dual-deploy parity with the Quarkus sketch additively, with core/ untouched — QUIC via native quic-go (no FFM)"
metadata: 
  node_type: memory
  type: project
  originSessionId: 2dde7081-732d-49f5-b0aa-ce19637ba5f1
---

The Go backend at repo root reached **full parity** with `experiments/jvm-quarkus-sketch/` on branch
**`go-parity`**: one binary, `ROLES` env picks monolith (unset) or the microservices split, transport-transparent
sync seams over **native QUIC** (`quic-go`, no FFM), broker-less async fanout, admin fan-out. Verified live
(monolith + 2-process split) — plan `docs/plans/2026-07-05-1851-go-parity-plan.md`, status
`docs/2026-07-05-2000-go-parity-status.md`.

**The headline finding: parity was ADDITIVE — `core/` was never edited across all 7 steps.** The dual-deploy
machinery lives entirely in new packages + composition-root wiring, because the Go seams already existed:
- **Roles:** `parseRoles` in `cmd/server/main.go` (not core); `hosted` = active roles, `needed` = their unhosted
  DependsOn → register the real module for each hosted, a `modules/remote.Stub` for each needed. The stub is a
  `core.Module` named after the dep that `Provide`s a remote client — slots into the existing registry/topoSort,
  satisfying `DependsOn` and `Require` with zero core change.
- **Sync (ownerOf, verifySession):** the consumer already `Require`s a self-defined interface, so the remote
  client (edge/QUIC) is a drop-in — only the interface signature widened with `error` (network failure → 503,
  not a false 404 — this was a reviewer blocker). One shared `edge.Server` in main() serves both methods on one
  UDP port; monolith takes the local branch (no QUIC/cert).
- **Async:** the in-process `core.Bus` stays for the monolith/co-located path; the split adds a per-schema
  transactional `outbox` (write in the domain tx) + a relay (POSTs with a stable `event_id`, per-subscriber
  stop-on-first-failure ordering) + a **synchronous** sink in the consuming module (inbox `ON CONFLICT` dedup +
  effect in one tx, 200 only after commit — NOT re-emitted on the async bus, which would ack before the handler
  ran). Exactly one delivery per topology.
- **Admin:** `adminapi.Item` keeps the local `Render` closure (in-process, no cross-module import) and gains a
  `RemoteURL`; each module serves `GET /admin-data/<id>`; the stub contributes a remote metadata item; admin
  dispatches local-closure vs remote-HTTP with an error-card on a down peer.

**Follow-on (branch `core-split` off `go-parity`, verified pure refactor):** `core/` was decomposed into four
focused packages — `bus/` (Define/Emit/On), `registry/` (generic `Provide[T]`/`Require[T]`, string-keyed),
`contrib/` (multi-value Slots), `lifecycle/` (Module/Migrator/Starter/Stopper + new optional `Registrar`, Context,
App runner). **topoSort deleted** — replaced by a two-phase Build (Register-pass then Init-pass, registration
order, order-independent). `Module.DependsOn()` was NOT deleted, only renamed **`Requires()`** — it stays a
manifest because `cmd/server/main.go` reads it to plan which remote stubs a split process needs (deleting it
breaks the ROLES split). Insight the user drove: sorting-by-dependency is the smell (contradicts commutative
startup), but declaring-dependencies is legitimate (it IS in-process Kubernetes-Service-discovery shape — the
same named lookup that's a map in the monolith becomes a remote call in the split). rating went value→pointer (its
Provide + On subscription share the service across phases). Runtime behavior byte-identical (monolith + split
re-verified). Plan: `docs/plans/2026-07-05-2047-core-split-plan.md`.

**Gateway front door (branch `feat/go-gateway`, 2026-07-06 — closed the last Quarkus-ahead gap).** A
12-agent delta review found the ONLY functional gap of Go vs `jvm-quarkus-sketch` was the gateway/edge-routing
layer (everything else Go was ahead/parity: match/rating/leaderboard, webui, most of accounts, in-proc `bus`,
generic `outbox.Relay`). Added additively, 5 steps: (1) `edge.Server.HandlePrefix` (exact→longest-prefix-forward,
2 of Kotlin's 3 tiers — payload-only tier skipped as unused) + `edge.Client.CallRaw` (raw byte relay, no
double-encode; `Payload` is already `json.RawMessage`); (2) inventory got an `Edge *edge.Server` + `inventory.list`,
characters got `characters.list` (mirror of `characters.ownerOf`); (3) NEW root pkg `gateway/` (`RoutedBackend` =
`remote.edgeConn` generalized to bytes, identity-guarded reset, 1s per-attempt budget, one retry) + `NewHTTPProxy`
(httputil reverse proxy, verbatim path) + `cmd/gateway-svc` (stateless, NO `internal/app.Run` — no DB); (4)
`run.ps1`/`run.sh` → 3-process split (A chars :8080/:9000, B inv :8081/:9001, C gateway :8082 HTTP + :9100 player
QUIC); (5) in-process hermetic live smoke (routing to 2 backends + graceful degradation ~1s + HTTP proxy).
**Key Go-vs-Kotlin insight:** Go edge = 1 QUIC stream per request (correlation = stream), so Kotlin's whole
cid/pending-map/CompletableFuture machinery is unnecessary and Go gets more concurrency for free. All gates green
(build/test/vet/golangci-lint/go-arch-lint), trailers audited per lane. Plan `docs/plans/2026-07-06-2245-go-gateway-plan.md`.
**Merged to master (`--no-ff` `fa8b5f9`); NOT pushed.** VERIFIED LIVE with real Postgres via `run.ps1 microservices`
(3 procs): HTTP proxy routes /characters→A /inventory→B /admin→B; QUIC :9100 routes characters.list→A + inventory.list→B
(distinct backends, one listener); killing characters-svc → characters.list errors at exactly 2.0s (2×1s budget), inventory.list
still OK, gateway stays up. The live run caught a real bug the hermetic test missed: the HTTP proxy mounted only the
subtree `/characters/`, so a bare `/characters` got 307→`/characters/` and the backend (serving `GET /characters` exact)
404'd — fixed by registering each prefix at both exact + subtree (`fix(gateway)` `3ed71f5`), regression added. Lesson:
hermetic proxy tests that only hit `/prefix/<sub>` miss the bare-`/prefix` trailing-slash case.

**Go vs Kotlin on this exact case (the whole point):** QUIC = `edge/` 6 files + `import quic-go` vs Kotlin's
`edge/msquic/` ~15 FFM files (see [[edge-quic-msquic]]); topology switch = an explicit `if` in main() vs CDI
profiles + the SRMSG00073 channel-sharing wall; deploy = one 22MB static binary × N with different ROLES vs
fast-jar + JVM + `--enable-native-access` + dll; and Go's compile-checked wiring produced ~2 lint nits vs half a
session of build-invisible runtime bugs in Kotlin. Real backend is Go; the Kotlin sketches were the exploration.
Not merged to master (branch `go-parity`).

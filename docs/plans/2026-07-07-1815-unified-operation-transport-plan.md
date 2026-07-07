# Plan — Unified operation transport: one internal surface + gateway-as-client

**Date:** 2026-07-07 18:15 · **Revised:** 2026-07-07 (post ultrathink review — resequenced)
**Status:** APPROVED — executing all phases sequentially, review gate after each phase.
**Sign-offs (resolved 2026-07-07):** (1) consumers KEEP local structural interfaces — rule 4
unchanged, `<module>api` is NOT imported by domain consumers (see D2). (2) Edge mutual auth =
**mTLS** (pinned client cert), Phase C. (3) All phases, gate per phase. (4) msgpack out of scope.
**Product-owner decision (locked):** Full parity — the monolith runs the gateway too;
external requests enter through the gateway in BOTH topologies. End-state: ONE way modules
expose capability (typed operations, in-process or QUIC), gateway is a generic client of it.

> **Revision note.** An independent ultrathink review found the first cut structurally
> unsound: (B1) the `<module>api` contract package imported `edge`, poisoning the
> transport-free contracts tier; (B2) gateway-as-component forced `app.Run`→`gateway`,
> breaking app's topology-blindness; (B3) moving auth to the gateway before edge has mutual
> auth is an impersonation regression in the split; (B4) the phasing deleted the monolith's
> handlers before mounting their replacement. All are fixed by a **strangler resequence**
> and a **pure-contract / generated-glue split**. Review findings tagged `[Rn]` below.

---

## Context

### The debt (unchanged from research)

`characters.go` exposes `POST /characters` (player HTTP) **and** `characters.list` (QUIC edge)
**and** `/admin-data/characters` (HTTP) **and** carries `if m.Edge != nil` (a topology branch
in a module). Three internal-comms channels coexist — sync QUIC edge, async `/events` HTTP
(deliberately separate, **out of scope**), admin-data HTTP — plus player HTTP. Per sync method
there are ~10 hand-kept pieces (consumer iface, registry.Require, client stub + mirrored DTOs
in `remote.go`, method-name strings ×2, provider DTO copy, provider adapter, `Edge.Handle`,
two `wire_contract_test.go` byte-comparisons). Auth is triplicated inline
(`characters.go:288-318`, `inventory.go:363-393`, `accounts.go:214-225`). `list` is
double-exposed (HTTP + edge). Research inventory: **12 RPC-able operations** vs ~18 HTTP-native.

### Overlapping systems (Research-before-planning — MANDATORY)

Same table as researched — `registry` (typed in-process backbone, KEEP), `edge` (QUIC
transport, KEEP; gains one additive envelope field, see `[Rm1]`), `remote.Stub` (client
generated, dial/retry shell kept), `gateway` (becomes generic translator via a **leaf slot**,
not an app dependency — `[RB2]`), `Contribute` slot (the declaration seam), `<module>events`/
`adminapi` (shared contract precedent), `topiccheck` (the go/types codegen precedent). Full
detail in the research artifacts; the corrections below supersede the first cut's D-decisions.

### Locked design decisions (revised)

**D1 — Contract source = the Go interface, not yaml.** rpcgen synthesizes wire envelopes from
method signatures; no hand DTOs. Unchanged.

**D2 — SPLIT the contract from the glue `[RB1]`; consumers KEEP local interfaces (rule 4
unchanged, user sign-off).** `modules/<name>/<name>api/` holds ONLY the provider's canonical
capability interface + method-name constants — transport-free (imports `context`/pure types
only). It is the **codegen input** (rpcgen reads it) and the source of method-name consts. The
GENERATED glue (wire-envelope structs + client-over-`Caller` + server adapter) that imports
`edge` lives in a SEPARATE package `modules/<name>/<name>rpc/` (`mayDependOn: [edge, opsapi,
<name>api]`), NOT in `contracts`. **Domain consumers do NOT import `<name>api`** — they keep
their own local structural interface (e.g. `inventory`'s `charactersSvc`) exactly as today, and
`registry.Require[localIface]` resolves to either the real impl (monolith) or the generated
`<name>rpc.Client` (split), which structurally satisfies it — identical to how `remote.Stub`
works now. So rule 4 is untouched; `<name>api` is reached only by the generated glue + `remote`,
not by every consumer. This is strictly lighter than the first cut (which wrongly made every
consumer import a contract package and dragged edge into the contracts tier).

**D3 — LocalBackend calls the typed method directly; only RemoteBackend marshals `[RM4]`.**
`OperationBackend.Invoke`: `LocalBackend` resolves the provider's interface from `ctx.Registry`
and calls the Go method DIRECTLY (zero serialization — the monolith path), `RemoteBackend`
marshals over the edge `Caller` (split path). Same registry-swap shape as today, just with the
remote client generated. This makes "gateway in the monolith" a direct call + a map lookup with
NO marshal — killing the false "no latency regression" worry rather than asserting it.

**D4 — Gateway fronts the port via a LEAF SLOT, never an app→gateway import `[RB2]`.** Define a
leaf `frontmw` slot (mirroring `httpmw.ReadinessSlot`, already read by `app.Run` at
`app.go:256`). The gateway module `Contribute`s its front-handler to that slot; `app.Run` wraps
`ctx.Mux` with the contributed handler (it imports the leaf slot, not `gateway`). So `app`
stays topology-blind and arch-lint's `app: mayDependOn [lifecycle, edge, metrics]` is untouched.
The gateway is a normal `lifecycle` module present in every process.

**D5 — Auth-once is GATED behind edge mutual auth `[RB3]`.** Moving identity to an injected
envelope field is a security regression until the edge hop is mutually authenticated (today:
server-auth TLS only, `ClientTLS` InsecureSkipVerify — anyone reaching a backend's QUIC port
forges any `player_id`). So edge mTLS (or a shared-secret handshake + network isolation) is a
PREREQUISITE phase; until it lands, backends keep verifying bearers per-request. No trust-
boundary change ships into a split before the boundary is enforceable.

**D6 — Strangler migration `[RB4]`.** Mount the gateway fronting the EXISTING handlers first
(pure passthrough, zero behavior change, both topologies). Migrate operations one at a time;
delete a module's HTTP handler + its inline auth ONLY after its operation is live behind the
already-mounted gateway. Never delete-before-replace.

**D7 — HTTP-native stays HTTP; `/events` bypasses the gateway `[Rm3]`.** OAuth start/callback,
admin HTML, webui SPA, `/healthz`/`/readyz`/`/metrics` stay HTTP (gateway passthrough).
`/events` + all inter-service messaging POST peer→backend directly, NEVER through the gateway,
and stay on the app mux. Stated explicitly so nothing stranded.

**D8 — Operation error taxonomy in the envelope `[RM1]`.** edge's bare-string error can't carry
403-vs-404-vs-400-vs-503. rpcgen's response envelope gains a typed `Status` (an operation error
kind: NotFound/Forbidden/Invalid/Unavailable/Internal + message). Handlers return typed op
errors; the gateway maps them to HTTP status. This is designed in Phase 0, not per-op.

### Open decisions — RESOLVED into concrete Phase-0 steps `[RM3]`

The first cut parked `opsapi.Slot`, the `Caller` seam, and the binding struct as "open
decisions" while three steps consumed them — banned. They are now concrete steps 0.1–0.3.

---

## Phase 0 — Foundations (leaf seams + generator; NO behavior change)

### Step 0.1 — `opsapi` leaf: operation binding + the `Caller` seam `[opus]` `[RM3][RM2]`

**(a) What.** New leaf `opsapi/`:
- `type Caller interface { Call(ctx context.Context, method string, req, resp any) error }` —
  satisfied by both `*edge.Client` AND `remote`'s self-healing `edgeConn` (fixes the first cut's
  broken "wrap the concrete client" claim: the generated client targets `Caller`, so the retry/
  reconnect shell composes).
- `type Operation struct { Method string; Verb string; Path string; Auth AuthReq }` and
  `var Slot = contrib.Define[Operation]("ops.operation")` — the declaration seam the gateway
  reads to build its route table.
- `AuthReq` enum: `AuthNone` / `AuthPlayer` (bearer→player_id) — so `match/report` (auth-none
  today) and login/register/leaderboard declare `AuthNone` explicitly `[RM6]`.

**(b) Why now.** 0.2 (generator) targets `Caller`; B/D (gateway) read `Slot`. Both leaves must
exist first. Leaf, no module import — arch-lint clean.

**(c) How.** `contrib.Define` mirrors `bus.Define`/`adminapi.Slot`. `Caller` is the minimal
interface both edge client shapes already satisfy structurally (verify: `edge.Client.Call` and
`edgeConn.call` have matching signatures).

**(d) Dispatch:** `[opus]` — the seam shapes everything downstream.

### Step 0.2 — `tools/rpcgen`: pure `<module>api` + generated `<module>rpc` `[opus]` `[RB1][RM1][RM2][Rm1][Rm2]`

**(a) What.** `tools/rpcgen/` (go/packages+go/types, topiccheck's toolkit). Input: a capability
interface in `<module>api`. Output:
- Leaves `<module>api` untouched (pure interface + `Method<X>` string consts) — transport-free.
- Generates `<module>rpc/<module>rpc_gen.go`: per method `M(ctx, args…) (rets…, error)` — a
  request envelope (ordered args) + response envelope (ordered rets + a typed `Status` `[RM1]`);
  a `Client` implementing the `<module>api` interface over an injected `opsapi.Caller`; a
  `RegisterServer(reg EdgeRegistrar, impl <module>api.Iface)` installing one
  `func([]byte)([]byte,error)` adapter per method.
- Adds a reserved `Identity` field to the edge request envelope (`edge/wire.go`) — edge gains
  ONE additive field `[Rm1]` (so the plan is honest that edge is not literally unchanged).

**(b) Why now.** Everything consumes generated glue.

**(c) How — the hard parts the review flagged.** Signatures handled: builtin/struct/slice/
pointer types marshal via JSON (all 3 real cases — `OwnerOf (string,bool,error)`,
`VerifySession (string,bool,error)`, `AdminData (adminapi.ItemData,error)` — are JSON-clean);
a param/return that is itself a non-marshalable interface is a **generate-time error**, not a
silent bug. `ctx` is not marshaled — and the generated server adapter uses `context.Background()`
exactly as today's hand adapters do (`characters.go:123`), so the plan claims NO ctx propagation
that doesn't exist `[Rm4]`. Determinism: sorted methods + run `format.Source` + a fixed import
group so the `-check` regen-diff is toolchain-stable `[Rm2]`. The `-check` gate (regen to temp,
gofmt-normalize, diff) replaces the deleted `wire_contract_test.go` and is STRONGER (single
source ⇒ wire drift structurally impossible) once normalized.

**(d) Dispatch:** `[opus]` — generator design + envelope/status/identity + go/types traversal.

### Step 0.3 — arch-lint + verify wiring + CLAUDE.md rule-5 amend `[opus]` `[RB1]`

**(a) What.** `.go-arch-lint.yml`: `<module>api` → `contracts.in` (transport-free, every module
already `mayDependOn [contracts]`); NEW component `rpcglue: { in: modules/**/**rpc }` with
`mayDependOn: [edge, opsapi, <name>api]` — the glue tier that MAY import edge, kept OUT of
contracts. verify.ps1/.sh: add each `<module>api` to `$contractPkgs`; add an `rpcgen -check`
stage (advisory / blocking under `--strict`). Amend CLAUDE.md rule 5 LIGHTLY: `<module>api` (the
provider's pure capability interface, codegen input) is a sanctioned provider-owned contract
package reached by the generated glue + `remote`; `<module>rpc` is generated impl. Domain
consumers still depend on their OWN local interfaces (rule 4 unchanged) — the amendment does NOT
introduce a consumer→provider-package dependency.

**(b) Why now.** Gates must accept the new package shapes before Phase A commits generated files.

**(c) How.** `contracts.in` and `$contractPkgs` are the two hand-lists that must stay in
lock-step (research flag) — edit both, comment the coupling.

**(d) Dispatch:** `[opus]` — the enforcement seam + constitutional change.

---

## Phase A — Prove the generator on the 2 existing sync methods (NO behavior change) `[RM5]`

### Step A1 — Migrate `ownerOf` + `verifySession` to generated glue `[opus]`

**(a) What.** `charactersapi.Ownership` + `accountsapi.Sessions` (pure interfaces); rpcgen →
`charactersrpc`/`accountsrpc`. Consumers `registry.Require[charactersapi.Ownership]`. In the
monolith the registry gives the real service (direct call, D3). In split, `remote.Stub` Provides
the generated `Client` built over its self-healing `edgeConn` (the `Caller` seam, 0.1). Delete
`remote.go`'s hand clients + mirrored DTOs + method strings, the provider adapters + DTO copies,
both `wire_contract_test.go`, and the consumers' re-declared local interfaces.

**(b) Why now.** Proves the generator + the `Caller` composition on the real cases; deletes the
debt. Must be green before anything builds on it.

**(c) How.** `remote.Stub` keeps its dial/retry/reset `edgeConn` and wraps the generated `Client`
(composes because the client targets `Caller`, not `*edge.Client` — the review's M2 fix).
`[opus]` not `[sonnet]` `[RM5]`: this redesigns `remote.Stub`'s guts. Run split smoke + verify.

**(d) Dispatch:** `[opus]`.

---

## Phase B — Mount the gateway fronting EXISTING handlers, both topologies (strangler; NO route deletion) `[RB2][RB4]`

### Step B1 — Gateway as a lifecycle module fronting `ctx.Mux` via the `frontmw` leaf slot `[opus]`

**(a) What.** Define leaf `frontmw` slot (mirroring `httpmw.ReadinessSlot`). The gateway becomes
a `lifecycle` module; in `Init` it `Contribute`s a front-handler to `frontmw`. `app.Run` wraps
`ctx.Mux` with the contributed handler (imports the leaf slot, not `gateway` — D4). Register the
gateway module in EVERY `cmd/*/main.go` including `cmd/server` (monolith). Initially the
front-handler is **pure passthrough** — delegates everything to `ctx.Mux` unchanged. Result:
gateway now fronts the port in both topologies with ZERO behavior change; `cmd/gateway-svc` stays
a valid separate front process for the split (same module).

**(b) Why now.** This is the strangler mount — it must exist BEFORE any op migrates, so no phase
ever ships a port with deleted-but-unreplaced routes (the review's B4 fix).

**(c) How.** `app.Run` already wraps `ctx.Mux` in rate-limit+metrics middleware (`app.go:173-189`)
— the front-handler is one more wrap, sourced from the slot. `/healthz`/`/readyz`/`/metrics` stay
mounted directly by `app.Run` and are NOT wrapped/translated (m5): the front-handler only ever
intercepts routes present in its (initially empty) route table, passing all else through.

**(d) Dispatch:** `[opus]` — resolves B2, the load-bearing ownership question.

### Step B2 — `OperationBackend` + route table (empty) `[opus]`

**(a) What.** `gateway.OperationBackend { Invoke(ctx, op opsapi.Operation, req, resp any) error }`;
`LocalBackend` (resolve provider interface in `ctx.Registry`, call the typed method DIRECTLY, no
marshal — D3) and `RemoteBackend` (marshal via the `opsapi.Caller` to the owning peer). Gateway
builds its route table from `opsapi.Slot` contributions — still empty (no op declares a binding
yet), so the front-handler still passes everything through.

**(b) Why now.** The backend substrate must exist before D migrates ops onto it.

**(c) How.** `LocalBackend` uses the SAME registered service the in-process callers use — one
code path. `RemoteBackend` keeps `routed_backend.go`'s dial/retry/1s-budget. Monolith uses
`LocalBackend` for everything (zero network hop, zero marshal).

**(d) Dispatch:** `[opus]`.

---

## Phase C — Enforce the edge trust boundary (SECURITY PREREQUISITE, before any identity trust) `[RB3]`

### Step C1 — Mutual auth on the edge hop `[opus]`

**(a) What.** Add client authentication to the QUIC edge: **mutual TLS** (client cert pinned to a
local CA — user-chosen over shared-secret), plus documented network-isolation (backends' edge
ports reachable only from the gateway/peers). Until this lands, backends KEEP verifying bearers
per-request (no trust-boundary change yet).

**(b) Why now.** D injects a trusted `player_id` into the envelope; without this, a forged QUIC
envelope on a backend port = full impersonation (the review's B3). This gates D.

**(c) How.** Replace `edge.SelfSignedTLS`/`ClientTLS(InsecureSkipVerify)` with a pinned mutual
config, or add a required `X-Edge-Auth` handshake frame. Verify with a negative test (an
unauthenticated dial is rejected). This also protects the existing `ownerOf`/`verifySession`
calls, an immediate security win independent of the rest.

**(d) Dispatch:** `[opus]` — security-critical.

---

## Phase D — Migrate the 12 operations op-by-op (strangler; delete handler+auth only after its op is live) `[RB4][RM1][RM6]`

### Step D1 — Auth-once at the gateway; identity in the (now-trusted) envelope `[opus]`

**(a) What.** The gateway's front-handler, for a route whose binding is `AuthPlayer`, verifies
the bearer via `accountsapi.Sessions.VerifySession` through its backend, then sets the envelope
`Identity` (player_id). Backends read identity from the envelope only (safe post-C1). Add the
trust-boundary invariant: a domain operation NEVER reads a client-supplied identity.

**(b) Why now.** Depends on B (gateway+backend) + C (trust boundary). Precedes D2 (handlers rely
on injected identity once their inline auth is deleted).

**(c) How.** In the monolith `LocalBackend` sets Identity in-process; in split it rides the edge
envelope (0.2). Note: this makes `/accounts/*` and `match`/`leaderboard` reachable via the
gateway — for accounts this is a behavior EXPANSION in the split (players can log in through the
gateway), stated not hidden `[RM6]`; `match`/`leaderboard` are `LocalBackend`-only (monolith-only
today, no split service) `[RM6]`.

**(d) Dispatch:** `[opus]` — auth + trust boundary.

### Step D2 — Per-operation strangler migration (grouped) `[opus]` for auth/status-bearing, `[sonnet]` for trivial reads

**(a) What.** For each of the 12: declare the interface method in `<module>api`, its
`opsapi.Operation` binding (verb/path/auth), rpcgen the glue; the gateway route table picks it
up; THEN delete that module's `ctx.Mux.HandleFunc` + its inline auth. One op at a time, each a
green commit. Map each handler's differentiated HTTP status onto the typed op `Status` (D8)
`[RM1]`: e.g. `inventory.handleCharacter`'s 503/404/403/200 become `Unavailable/NotFound/
Forbidden/OK`. Delete the double-exposed `list` HTTP handler (now one op). Delete the triplicated
inline auth as each module's last authed op migrates.

**(b) Why now.** Depends on D1. The bulk migration, but incremental — never a broken window.

**(c) How.** Ops with auth or non-trivial status mapping (characters ×3, inventory ×3, accounts
×4) are `[opus]` `[RM5]` — status taxonomy + auth deletion is behavioral. Trivial public reads
(`leaderboard`, `match/report`) are `[sonnet]`. Each op: add binding → verify gateway serves it
(both topologies) → delete handler+auth → commit → verify green.

**(d) Dispatch:** mixed `[opus]`/`[sonnet]` per op as above.

---

## Phase E — Fold admin fan-out `[sonnet]`

### Step E1 — `<module>.adminData` operation; delete `/admin-data` + `PEER_HTTP_ADDR`

**(a) What.** Each module's admin content becomes `AdminData(ctx) (adminapi.ItemData, error)` in
`<module>api`, exposed via generated glue. `admin`'s remote items call it via the backend
(`adminapi.Item.RemoteURL string` → `RemoteFetch func`). Delete every `/admin-data/<id>` handler,
`admin.Module.http`+`fetchRemote`, `PEER_HTTP_ADDR`/`peerAdminURL`.

**(b) Why now.** Depends on the operation machinery (0–D). Independent of player-op migration.

**(c) How.** 404-skip/error-card semantics map onto op errors (unknown-method → skip; other →
"unavailable" card). No user-visible change.

**(d) Dispatch:** `[sonnet]` — mechanical once the machinery exists.

---

## Phase F — HTTP-native homing + final verify `[opus]`

### Step F1 — Gateway passthrough for HTTP-native; confirm `/events` bypass; final green

**(a) What.** The gateway front-handler passes through (unchanged) the HTTP-native routes: OAuth
`start`/`callback` → accounts, admin HTML `/admin*`, webui `/`, and leaves `/healthz`/`/readyz`/
`/metrics` to `app.Run` (m5). Assert `/events` + inter-service messaging never enter the gateway
route table (D7/m3). Full `verify.ps1 --all` + `scripts/smoke-split-messaging.sh` + a new
sync-operation split smoke (create character through the gateway, both topologies).

**(b) Why now.** Last — every external route has a defined home; nothing stranded.

**(c) How.** Passthrough is the initial B1 behavior, now the permanent home for HTTP-native. The
OAuth callback MUST resolve at its exact Epic-registered path through the gateway — verify.

**(d) Dispatch:** `[opus]` — the HTTP-native boundary + end-to-end verification.

---

## Dispatch summary (for approval at ExitPlanMode)

| Phase | Step | Work | Lane |
|---|---|---|---|
| 0 | 0.1 | `opsapi` leaf: `Caller` seam + `Operation` slot + `AuthReq` | `[opus]` |
| 0 | 0.2 | `tools/rpcgen`: pure api / generated rpc, envelope+status+identity | `[opus]` |
| 0 | 0.3 | arch-lint (`rpcglue` tier) + verify `-check` + CLAUDE.md rule 5 | `[opus]` |
| A | A1 | migrate ownerOf+verifySession; `remote.Stub` over `Caller` | `[opus]` |
| B | B1 | gateway module fronts `ctx.Mux` via `frontmw` leaf slot (passthrough) | `[opus]` |
| B | B2 | `OperationBackend` (Local direct-call / Remote marshal) + empty table | `[opus]` |
| C | C1 | edge mutual auth (security prerequisite) | `[opus]` |
| D | D1 | auth-once at gateway; identity envelope | `[opus]` |
| D | D2 | per-op strangler migration (12 ops) | `[opus]`/`[sonnet]` |
| E | E1 | adminData op; delete `/admin-data` + `PEER_HTTP_ADDR` | `[sonnet]` |
| F | F1 | HTTP-native passthrough; `/events` bypass; final verify | `[opus]` |

Every step ends green (`verify.ps1` + split smoke); every PHASE is a user review gate. Trailers
per lane; commit after each step.

## Explicitly out of scope

Async `/events` plane (D7). Codec msgpack swap. Semantic changes to the 12 ops beyond WHERE auth
happens (accounts-via-gateway expansion is noted, `[RM6]`).

## Sign-offs — RESOLVED (2026-07-07)

1. **Consumers KEEP local structural interfaces** — rule 4 unchanged; `<module>api` NOT imported
   by domain consumers, only by the generated glue + `remote` (D2). ✓
2. **CLAUDE.md rule-5 amend is light** — `<module>api` a provider-owned codegen-contract package;
   no consumer→provider-package coupling introduced (0.3). ✓
3. **Edge mutual auth = mTLS**, pinned client cert, Phase C prerequisite before any identity
   trust. ✓
4. **All phases sequentially, review gate after each phase.** ✓

## Review disposition

All ultrathink findings folded: B1→D2 pure/glue split + `rpcglue` tier; B2→`frontmw` leaf slot;
B3→Phase C prerequisite; B4→strangler (B mounts before D deletes); M1→envelope `Status`;
M2→`Caller` seam; M3→concrete 0.1; M4→LocalBackend direct call (no marshal); M5→`[opus]` on
A1/D2-authed; M6→match/leaderboard LocalBackend-only + accounts-expansion + match auth-none
stated; m1→edge additive `Identity` field acknowledged; m2→gofmt-normalized `-check`;
m3→`/events` bypass explicit; m4→ctx uses Background (no false propagation); m5→health stays
app.Run infra.

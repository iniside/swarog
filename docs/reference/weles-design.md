# Weles — orchestrator design (decided shape)

The durable record of what Weles is meant to become and, just as importantly,
what has already been rejected. Until now this design lived only in agent memory,
which meant every fresh context re-derived it — and sometimes re-derived it wrong.
This file is the source of truth; memory points here.

**Status:** M0 shipped (2026-07-15) — supervisor, restart-on-crash, `deploy/`
generations, control endpoint, `rollout.lock` bit-compat. Pre-M1 hardening closed
(2026-07-16). M1 not started. Decisions below were taken 2026-07-09..07-10 by
Lukasz unless dated otherwise; they are settled, not open questions.

## Non-negotiables

- **Native OS processes. No containers, no Docker, no Kubernetes.** Rust ships
  static binaries; a container would wrap one file in ceremony. Deploy = copy
  binary + supervise. Resource limits, if ever needed, are direct cgroups/Job
  Objects. Reference point: Guild Wars 1 (2005) ran a custom native-process
  orchestrator with live no-downtime updates, small team, no container tooling.
- **Zero-sharing, both directions.** Weles never imports a workspace crate; the
  workspace never imports a Weles crate. The only coupling is a **wire-only JSON
  contract with its own types on each side**. Weles is not a `lifecycle::Module`
  and never lives in `core/` — it embodies topology while modules are
  topology-blind, and it outlives the processes it spawns.
- **Weles never builds.** No cargo invocation, ever. It executes artifacts staged
  in `<root>/deploy/` by `weles deploy <src-dir>`. (A linker failure during M0
  exposed the creep that produced this rule.)
- **Cross-platform: Windows, Linux, macOS.** All platform abstraction lives in the
  agent behind a small trait (spawn_supervised / kill_tree / alive): process
  groups on unix, Job Objects on Windows. The master and the wire contract are
  platform-free. This is why drain is a **wire command, not SIGTERM** — SIGTERM
  does not exist on Windows. Signals are only the fallback for a process that
  stopped answering.

## Two disjoint boot modes (resolved 2026-07-10)

The managed contract is **opt-in per process**, and the switch is one
deterministic decision at process start — never config layering or precedence
(config-as-code: no magic, no layering):

| `ORCHESTRATOR_URL` | Mode | Config source |
|---|---|---|
| set | **managed** | pull from the centrala (hello + resolve) |
| unset | **standalone** | classic `*_EDGE_ADDR` env — today's path |

Standalone is a **first-class supported mode**, like monolith-vs-split — not a
fallback and not a deprecation target. A process that does not implement the
client simply gets no management: no restarts, no replicas, no rolling deploy.
**No other penalty.** It boots and serves exactly as it does today.

This is the answer to "should Weles keep a fallback": the question dissolves,
because the two modes are disjoint rather than layered. Nothing falls back to
anything.

## The managed convention (four points)

A tiny, language-neutral contract. Any-language service implements it itself;
Weles supports nothing per-service.

1. Read `ORCHESTRATOR_URL` + service/instance identity.
2. `hello` (registration) + `resolve` the peers its stubs need.
3. Expose `GET /readyz`.
4. Drain on a **wire command from the centrala**.

`cmd/server` (the monolith) satisfies this trivially — it has no peers to resolve.
Registration proper only matters for processes the centrala did not spawn (future
game servers).

## The client lives in `core/remote` — backend-owned

The Rust client is **backend code**, not a Weles crate. This is what keeps
zero-sharing real rather than nominal: no crate crosses the boundary in either
direction, only JSON on the wire, own types on each side.

It belongs in `core/remote` specifically so `Stub` can **re-resolve** — replicas
and address changes without restarting the consumer. The seam is already in the
right place: `Stub` holds `peer_addr` as an unparsed `String` and parses it
**lazily at dial time** (`core/remote/src/lib.rs`, `EdgeDialer`), so the
"fetch address → dial" path already runs per call rather than once at boot.
Swapping a fixed string for a resolver is a field change, not surgery.

Integration point is the **`cmd/*-svc` main** — exactly where `std::env::var`
reads `RATING_EDGE_ADDR` today and hands it to `remote::Stub`. Modules are never
touched and stay topology-blind. This is also why "a service without the client
still works" is nearly free: it is a two-line difference in one file.

## Discovery, and who knows what

**Knowledge of dependencies stays in the consumer** — its stubs already declare
them. There is no env-name mapping in any Weles manifest; the manifest shrinks to
services / binaries / replicas.

Today's env push already enforces this least-knowledge shape and M1 must preserve
it: `match-svc` receives only `RATING_EDGE_ADDR` and has no idea leaderboard
exists (`weles/src/manifest.rs`, `split_fleet`/`compose_env`). So `resolve` is
scoped per-consumer — never "give me the fleet map".

`resolve` returns **all live instances**, and **round-robin load balancing is
client-side** in `core/remote`.

### Manual vs automatic

- **Automatic (runtime state Weles owns):** ports (agent-minted), addresses
  (agent IP + port), discovery, client-side LB, process identity (injected at
  spawn).
- **Manual (the manifest, and only the manifest):** services, binary/version,
  replicas, placement, and the static env Weles does not own (`DATABASE_URL`,
  secrets, feature flags) as literals.

Source of truth is the **git-versioned Rust manifest** + `plan`/`apply`
(Terraform-style diff) — **not** a mutating admin panel, which would recreate the
where-did-this-value-come-from drift. Panel/CLI is read-only observability plus
imperative ops (status/restart/deploy); CLI first, a tiny own web UI later at
most — **never** a page in the backend's `admin` module (zero-sharing both ways).
The only runtime-mutable desired state: future game-server `replicas`, via API
from the server-management module — game-server management is a domain fortress
under `modules/`, never generic Consul/etcd-style discovery infra (the services
topology is static; the only dynamic discovery-shaped problem is allocating
stateful session instances, which is a domain concern). It commands Weles over a
wire API, address injected by `cmd/*`, module stays topology-blind.

## Gateway route discovery — routing-as-data

The hand-listed `remote::Stub` set in `cmd/gateway-svc/src/lib.rs` is to be
replaced by data:

- Each svc serves a reserved `describe()` op over its edge, returning a manifest
  of its `#[http]` ops (verb, path pattern, method id, param mapping — data the
  `#[rpc]` macro already holds at glue-gen time).
- The gateway builds its route table **at runtime** from peers' manifests; the
  peer list comes from the centrala's `resolve`. Same loud collision-fail at
  manifest merge as today.
- Adding a module then requires **zero gateway changes**.

The gateway's own typed capability deps (`accountsapi::Sessions`,
`apikeysapi::Keys`) stay compiled in — they are its sync deps, not routing.

**Research this before writing it:** whether every existing `OpBinding`
decode/encode is declaratively describable (e.g. match's Go-parity
`Winner`/`Loser` mapping), or whether HTTP decode moves svc-side and the gateway
becomes a pure reverse proxy.

Until this lands, the interim safeguard is a checker tripwire diffing the stub
list against `api/*/rpc` crates on disk — any tool resting on a hand-maintained
list must diff it against the real source of truth and die pre-work with a
per-entry drift log (same discipline as `weles-fleet-parity`).

## State: SQLite, runtime only

Weles's own state is SQLite (`rusqlite`, `bundled` — embedded, no server,
cross-platform, preserves the one-binary deploy). Scope is deliberately narrow:

- **Desired state is the git manifest**, never the database.
- **Most runtime state is soft** — reconcilable from agent reports after a master
  restart (kubelet-style).
- SQLite persists what live processes cannot tell you: deploy history, port
  assignments of dead instances, API-mutated desired state.

No state replication. Master migration = move the `.db` file, or rebuild from
manifest + reconciliation.

The real driver for SQLite is **write concurrency**, not storage: today a single
writer (the supervisor) makes JSON safe; `hello`/`resolve` + port minting
introduce N writers.

## Multi-machine: master + per-machine agents

The Nomad server/client shape. **No overlay network, no master election.**

- Overlay exists in k8s only for IP-per-pod; native processes share host
  networking, so `resolve` returns real `host:port` — plain LAN/VPC routing. The
  mTLS QUIC edge already assumes an untrusted network. Cross-internet fleets get
  a flat host-level WireGuard at most, never per-process overlay.
- Master holds manifest / desired state / resolve API. Agents are dumb
  spawn/kill/status executors connecting **outbound** to the master. Processes
  still see only `ORCHESTRATOR_URL` — the contract is unchanged by topology.
- Single master, local-disk state, no Raft. Replicated consensus is the threshold
  where you start rewriting Consul. **Master down = running processes keep
  running** (addresses already resolved, agents supervise locally); only
  management degrades until restart.
- Placement is a manifest annotation, not scheduling.

Names: Weles is the Slavic god of herds — it shepherds the process flock, and the
backend is Swaróg. If master and per-machine agent ever separate: master =
`weles`, agent = `rarog` (Swaróg's fire falcon). Reserved, not yet decided.

**The multi-machine proof is planned against real hardware** (recorded
2026-07-10): master on the Windows dev box + agent on the MacBook beside it, over
a real LAN — non-loopback resolve, mTLS across a physical network, mixed-platform
fleet. macOS is therefore a live test platform, not a theoretical target.
Verification gates: Windows and Linux blocking.

## Rejected — do not re-propose

- **Containers / Docker / k8s / containerd** — see Non-negotiables.
- **Weles using the backend's Postgres** (registry table + LISTEN/NOTIFY) —
  superseded; violates zero-sharing.
- **An env-template manifest** (env-name mapping in the manifest) — superseded by
  control-plane pull; dependency knowledge belongs to the consumer.
- **Link-time auto-registration (linkme / inventory distributed slices) for
  gateway routes** — only moves the hand-maintained list from `lib.rs` to
  `Cargo.toml`; identical forget-risk.
- **A Weles page in the backend's `admin` module** — zero-sharing both ways.
- **A mutating admin panel as topology source of truth** — config drift.
- **Folding Weles into `core/` or the module system.**

## Open design points

- **`hello`/`resolve` transport.** Today's control endpoint is a serial,
  single-connection operator IPC (named pipe / UDS), not a registry serving N
  services. Concurrency model and reachability are real work. Weles is
  **std-only, no tokio** — `axum` would break that invariant; a blocking mini-HTTP
  or an extended pipe/UDS keeps it.
- **Remote authorization.** `status`/`down` are safe today because the OS
  confirms the caller (`SO_PEERCRED`, pid+uid). Once `hello`/`resolve` admits a
  remote caller, that backstop is gone and authorization must be designed, not
  inherited.
- **Port minting vs the parity gate.** The blocking `weles-fleet-parity` stage
  asserts static ports equal to processctl's. Minting requires reframing "static
  ports" as "requested defaults" and redefining what the gate asserts — otherwise
  minting fails `--fast`. See [weles-fleet-parity](weles-fleet-parity.md).
- **Replica-safety is a module prerequisite**, not a Weles feature: before any
  `replicas: 2`, rating's MMR must be DB-backed and the relay needs an advisory
  lock per `EVENTS_ORIGIN`.

## M1 scope

`weles rollback` (generations + `current` + sha256 already exist — one CLI verb),
`hello`/`resolve`, SQLite, port minting.

Carried into M1 from the readiness review
([status](../status/2026-07-16-1342-weles-m1-readiness-review-2-status.md)):
bind the control endpoint before the slow prep helpers (today a 30–60s window
where `weles down` cannot stop the fleet, and M1 lengthens it); take the
readiness probe off the monitor thread before replicas; derive `DOWN_TIMEOUT`
from fleet size; verify sha256 on read (the integrity half of rollback); treat
retention's live pins as a set once overlapping supervisors are possible.

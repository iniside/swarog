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
- Master holds manifest / desired state / resolve API; agents connect **outbound**
  to it. Processes still see only `ORCHESTRATOR_URL` — the contract is unchanged
  by topology. (An earlier sketch called agents "dumb spawn/kill/status
  executors". That undersells them — see the role split below: the restart policy
  and local supervision are the agent's, and they are weles's whole
  differentiator.)
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

## Master and agent — the role split (decided 2026-07-16)

Nomad is the reference, and Nomad ships **one binary with roles**
(`nomad agent -server` / `-client` / both). Today's weles — master and agent in
one process — is therefore not a stopgap to be fixed; it is Nomad's own dev
shape. The job is to draw the boundary internally, and flip roles on later.

**Master** — manifest (desired state, from git), SQLite, the HTTP API, the
resolve registry. It never touches a process, so it is platform-free.

**Agent** — spawns, supervises, restarts, mints ports, proxies resolve. ALL
platform code lives here: Job Objects, process groups, pdeathsig, the FFI.
Today's `supervisor.rs` and `platform/` are agent code. Note this is a heavier
role than the earlier sketch's "dumb spawn/kill/status executor": the restart
policy and local supervision — weles's actual differentiator vs devctl — are the
agent's.

### The agent dies with its fleet — no re-attach

A Nomad client that restarts recreates handles to still-running tasks from a
local state store. It names the price: if that state is lost while the client is
down, it cannot recreate the handles, so it **can neither stop nor restart those
tasks** — stale versions keep running
([#9512](https://github.com/hashicorp/nomad/issues/9512)). Siblings of the same
class: jobs restarting on client reconnect despite `restart { attempts = 0 }`
([#6212](https://github.com/hashicorp/nomad/issues/6212)); macOS clients not
resuming after a server restart
([#26083](https://github.com/hashicorp/nomad/issues/26083)).

**We take the other side of that trade.** `KILL_ON_JOB_CLOSE` / `pdeathsig` stay:
the agent takes its fleet down with it, and the recovery path IS the normal boot
path — so it cannot rot, and the orphan class never exists. Nomad's own reboot
guidance is to *drain* the node first, so even with re-attach it is not the
answer for a planned restart; draining is, and that is a rolling-deploy feature,
not a supervision one.

Consequences, accepted deliberately:
- **The OS service manager restarts the agent** (systemd / Windows service /
  launchd) — we delegate, we do not write it. This requires the agent binary to
  be installable as a system service, which it is not today.
- **The master never resurrects an agent.** Cross-machine it structurally cannot:
  starting a process on machine B requires something already running on B, which
  is what the agent *is*. Giving the master remote exec (SSH) would make it a
  different product. The master's job on agent death is to notice the missing
  heartbeats, mark the node down, and report. (Nomad servers never start clients
  either.)
- **Fleet survivability across a master restart comes from the two roles being
  separate processes** — the agent stays up holding its job objects — **not from
  re-attach.** Re-attach would only buy "the agent itself can restart without
  downtime", a much narrower prize.
- **The agent is stateless.** It mints ports locally and reports them up (the
  master persists them for the record); it need not remember them, because an
  agent restart takes the fleet with it and everything is re-minted on the way
  back.

### Master unavailable = orchestration is down. Accepted.

Running processes keep running (the agent is alive and supervising); everything
that *changes* state stops until the master returns. The agent caches nothing and
remembers no assignment, so a machine whose agent restarts during a master outage
stays dark. At this scale (a handful of machines, ~12 services) that is a fair
trade; the machinery Nomad builds to avoid it solves a problem we do not have.

Corollary: caching in the agent is **not foreclosed** — the service-facing
contract below is identical whether the agent knows the answer or forwards the
question, so a cache stays a pure optimization, addable when a second machine
makes it hurt.

## The service-facing contract: services only ever talk to their local agent

A service asks its **local agent**; the agent asks the master. `ORCHESTRATOR_URL`
therefore points at the agent, not the master.

Why this shape:
- **The service→agent hop is local**, so it can use the OS to confirm its caller
  and needs no certificates at all. **mTLS is then confined to the agent↔master
  hop — between two binaries we ship ourselves.** A future Go service implements
  plain localhost HTTP + JSON, no TLS. (Pointing services at the master directly
  would mean issuing a client cert to every service.)
- It keeps the master off the re-dial path — `Stub` re-resolves at dial time
  (that is why the client lives in `core/remote`), and that path should not cross
  the network to another machine.
- The contract is stable under every implementation of the agent's side (proxy
  today, cache later), so it never has to change.

On one machine master and agent are the same process, so this choice costs
nothing today and only starts to matter at the second machine.

### Honest note: this contract goes FURTHER than Nomad

Nomad tasks do not phone home. Services are stored in the server's state store,
but tasks discover addresses **through template stanzas or the API** — and the
dominant pattern is the first: the **client renders addresses into the task's
env/files, and the task implements nothing**. On dynamic ports, explicitly: "your
service will have to read an environment variable to know which port to bind to
at startup" ([service discovery](https://developer.hashicorp.com/nomad/docs/networking/service-discovery)).

So Nomad buys language-neutrality by making the task dumb. **Our standalone mode
(env vars) is closer to Nomad than our managed mode is**, and our pull contract is
more invasive than the architecture we cite. We go further on purpose — we want
`Stub` to re-resolve without restarting the consumer, where Nomad's answer is
re-render + signal/restart the task. That is a defensible call, but it is OURS,
not inherited. Do not justify the pull contract by pointing at Nomad.

## Transport: HTTPS + JSON + mTLS, not QUIC (decided 2026-07-16)

The internal edge is QUIC, so QUIC is the tempting default. It is the wrong tool
here:
- **QUIC buys nothing at this volume.** Its wins — stream multiplexing without
  head-of-line blocking, 0-RTT, connection migration — matter on the backend's hot
  path (player traffic, inter-service RPC). The control plane is `hello` once at
  boot, `resolve` for a handful of peers, an occasional drain command, agent
  status reports.
- **The contract must be language-neutral.** A future Go service implements it
  itself; HTTP+JSON is an hour's work in any language, a custom QUIC framing is
  not.
- **Reuse is illusory.** For QUIC to buy anything, weles would have to speak
  `core/edge`'s protocol — hand-copying a non-trivial wire protocol across the
  zero-sharing boundary, bit-compatible forever, with no gate. We already run a
  blocking parity stage merely because weles hand-copied `fleet.rs` (ports and
  env). Raw quinn with our own framing would share only the transport's name.

Authorization on the agent↔master hop is mTLS with client certs from the CA weles
already mints (by shelling out to the deployed `edgeca` binary — a process
contract, not an import). `reqwest` is already in the workspace for the backend
side.

## Tokio: the runtime arrives WITH the HTTPS server (decided 2026-07-16)

The M0 plan recorded "no tokio" as *finding #13*, with this rationale: "all sync
std threads; the devctl/processctl patterns we copy are synchronous; timing
decisions via injected Instant". That rationale is **scoped to M0** — supervising
processes and a local operator endpoint. It says nothing about a network server
serving N clients, which did not exist. M1 does not violate the decision; it
outgrows its premise. (An earlier revision of this file called std-only an
*invariant*. That was an overstatement, copied from a readiness review.)

The runtime arrives when it becomes unavoidable — with M1's HTTPS server — as a
**contained I/O island on its own thread**, not as a whole-crate migration and
not as a separate no-feature refactor first. Weighed against and rejected:
unifying the operator pipe onto HTTPS (see below), and a tokio-first refactor
whose honest prize is ~60 lines plus concurrent probes.

**The line the runtime may not cross.** It may own the probe and network I/O and
hand results back as plain values. It must never own:
- **`platform/*`.** `tokio::process` would actively destroy invariants: its
  reaper reaps children out from under you, breaking the "never reap the root
  before sweeping the group" rule that keeps `kill(-pid)` off a reused pgid;
  `try_wait` must report whole-containment-unit exit (`job_active_processes == 0`),
  not root exit, or a `-svc` with a grandchild is reported dead while holding its
  port; `kill_on_drop` kills the root only, with no job object.
- **`spawn`** — `SPAWN_LOCK` is a `std::sync::Mutex` held across `CreateProcessW`.
- **`lock.rs`** — `RolloutLock` stays an RAII local on the supervisor thread;
  flock/LockFileEx ownership is per-fd/per-handle, and "the lock drops last" is an
  ordering guarantee a task would break.
- **`state.rs`** (atomic tmp→rename), **`prep.rs`**, and the pure clock-injected
  decision functions.
- **The signal handler**, which may touch only a static atomic. `tokio::signal`
  would fight the existing raw `libc::signal(SIGINT)` — last writer wins.

Also note: `Reporter` uses `Cell`/`RefCell` and is `!Sync` by design; `Drop`
cannot await, yet teardown ordering (poller dropped before teardown, control
after — the "P6" invariant — and `_lock` last) is built on `thread.join()` in
`Drop`. And there is **no prior art in this repo** for a runtime on a dedicated
thread beside sync code: everything is either whole-process async with
`spawn_blocking` escapes, or whole-process sync with one `block_on` in `main`.

**Hard rule for the refactor:** nothing on the async side may ever manufacture
`Observed::Exited`. That stays the sole authority of `OwnedProc::try_wait`. Under
async, "connection refused" and "the process is gone" look alike; unifying
`ConnectFailed => Exited` would turn a Postgres blip into a fleet-wide restart
storm. See the invariant note below.

### Where "readiness never restarts" actually lives

Not in the poller thread — that thread exists purely for latency (an ~800ms
blocking probe must not delay the 100ms monitor tick). The invariant is held by:
a **type boundary** (`readiness_for` returns `Readiness`, which has no
constructor into `Observed`/`Directive`; `fold_readiness` writes only
`svc.readiness` and returns `bool`); a **pure match arm**
(`Phase::Healthy => match observed { Exited => crash, _ => Stay(phase) }`); and
**`Observed::Exited` being unforgeable from a probe** (`ConnectFailed` maps to
`NotReady`). `supervisor.rs`'s own doc comment claiming "observe/step never see a
probe" is FALSE — `observe()` probes in `WaitingHealthy` and feeds the result to
`step()`. The two most load-bearing guards are the least tested.

## Not unifying the operator control plane (decided 2026-07-16)

`weles status` / `down` stay on the local pipe/UDS. Moving them onto HTTPS would
delete ~165 lines of hand-rolled accept loops and frame codec, but it conflates
two trust domains: the operator path authenticates a **local OS caller**
(UID / owner DACL), the service path authenticates a **certificate identity**.
Unifying does not merge them — it forces operator cert issuance, storage, and
revocation, and costs `weles down` its OS-verified caller just to stop a local
fleet.

**Known gap surfaced by that review:** the operator control endpoint supports
Windows and Linux only, despite weles claiming macOS support — and macOS is the
planned second machine. The fix is a native macOS UDS peer-cred implementation,
not weakening local auth everywhere.

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
- **QUIC for the control plane** — buys nothing at this volume, is hostile to a
  language-neutral contract, and its only real prize (reuse) would mean
  hand-copying `core/edge`'s wire protocol across the zero-sharing boundary.
- **Agent re-attach to running processes after its own restart** — buys a narrow
  prize and imports a whole bug class; the fleet survives a *master* restart
  because the roles are separate processes, which is the property we actually
  wanted.
- **The master (re)starting a dead agent** — structurally impossible
  cross-machine without giving the master remote exec, which is a different
  product. The OS service manager does it.
- **Unifying the operator pipe onto HTTPS** — conflates two trust domains.
- **A tokio-first refactor as its own step, before M1** — honest prize is ~60
  lines and concurrent probes; the runtime arrives with the HTTPS server instead.
- **A whole-crate tokio migration** — `platform/*` under `tokio::process` would
  destroy the containment and reap-ordering invariants outright.

## Open design points

- **How binaries reach a second machine.** `weles deploy` stages into a local
  `deploy/`. Nomad clients fetch artifacts themselves. Ours does not address this
  at all. No idea yet — revisit when the second machine is real.
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

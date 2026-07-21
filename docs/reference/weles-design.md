# Weles — orchestrator design (decided shape)

The durable record of what Weles is meant to become and, just as importantly,
what has already been rejected. Until now this design lived only in agent memory,
which meant every fresh context re-derived it — and sometimes re-derived it wrong.
This file is the source of truth; memory points here.

**Status:** M0 shipped (2026-07-15) — supervisor, restart-on-crash, `deploy/`
generations, control endpoint, `rollout.lock` bit-compat. Pre-M1 hardening closed
(2026-07-16). M1 partially shipped — the agent hello/resolve endpoint (2026-07-17)
and the `weles-managed-gateway` verify stage are live; the fleet definition moved
to `fleet.toml` (2026-07-21). Single-host authorities hardened 2026-07-21 (runtime
root resolution, placement annotation, Told/Asks replica validator) — see the
two dated errata below. Decisions below were taken 2026-07-09..07-10 by Lukasz
unless dated otherwise; they are settled, not open questions.

## Errata (2026-07-21) — fleet definition moved to `fleet.toml`

The fleet's DATA (service names, ports, peers, per-process env) moved out of the
hardcoded `weles/src/manifest.rs::split_fleet()`/`monolith()` Rust literals and
into an operator-authored, strict `fleet.toml` (`weles/src/fleet_toml.rs`,
`#[serde(deny_unknown_fields)]`, no layering, no templating — the anti-magic
rule's one recorded exception, because the fleet must be readable at a deploy
site that does not compile weles). `weles deploy <src-dir> --fleet <path>`
stamps the chosen file into the generation; `weles up` (no topology argument —
`enum Topology` and `up [split|monolith]` are both deleted) reads it back and
boots whatever fleet was deployed. `weles up --dry-run` runs
`fleet_toml::validate` without acquiring the rollout lock, running a prepare
hook, or spawning anything.

This is a source-of-truth change, not a widening of what "the manifest" means
below: the **git-versioned manifest** claim (`## Discovery, and who knows
what`) still holds — `fleet.toml` is checked in and diffed the same way the
Rust literals were; it is a deployed, closed-world file `resolve` derives from,
just no longer compiled into the weles binary.

Composition/wiring logic (`compose_env_with_fleet`, `PeerAddrs`, `peer_addr`,
`service_addr`, `Addrs::Told`/`Asks`) stayed in Rust, now operating on owned
types parsed from the TOML rather than `&'static` literals — only the source of
the DATA changed.

weles lost DOMAIN KNOWLEDGE it should never have carried in the first place —
the Postgres session-budget machinery, the hardcoded `edgeca`/`adminctl`
binary names and argv, the DB/CA env-injection special-case — while KEEPING the
generic capability underneath each: `[[prepare]]` is a fleet-declared list of
opaque commands (name/run/args/env/passthrough/timeout) run once before the
fleet boots, so "mint the CA" and "seed the admin" still happen, just as data
instead of a hardcoded call. The blocking `weles-fleet-parity` verify stage
(referenced several times below as the live parity gate) was DELETED — its
premise, guarding weles's hand-copy of `processctl`'s fleet table, evaporated
once that hand-copy was replaced by `fleet.toml`; see
[weles-fleet-parity.md](weles-fleet-parity.md) for the removal note and the
historical record of what it checked. Below, every mention of
`weles-fleet-parity`, `split_fleet()`, or `weles up split|monolith` as CURRENT
is historical — read against this errata, not deleted, per this repo's
"historical docs are archives" convention.

## Errata (2026-07-21b) — single-host authorities fixed; remaining single-host known-gaps

Two compile-time/hardcoded *authorities* were removed while the crate is small, so
later milestones read from data instead of inheriting a single-host `if`. What
changed, and — more importantly — the single-host assumptions that DELIBERATELY
remain, consolidated here because they were previously assemblable only from four
scattered places.

**Fixed now.**
- **Root is a runtime authority, not a compile-time literal.** The two duplicated
  `env!("CARGO_MANIFEST_DIR").parent()` derivations (`main::state_path`,
  `supervisor::workspace_root`) are gone; one `prep::resolve_root(flag)` decides the
  fleet root: `--root` → `WELES_ROOT` → walk cwd up to the repo marker (`Cargo.toml`
  + `tools/processctl/`, byte-matching `verifyctl`'s `workspace_root` so
  `<root>/run/rollout.lock` stays identical and the one-Postgres mutual exclusion
  holds) → else fail closed. This is what makes "run the weles binary somewhere
  other than the build checkout" honest: pass `--root`/`WELES_ROOT`. `current_exe`
  is deliberately NOT consulted (a deployed weles installs separately from the
  fleet's `deploy/`).
- **Placement, not a raw host, is the manifest datum.** `fleet.toml` services may
  carry `placement: Option<String>` (the design-sanctioned annotation, "Placement is
  a manifest annotation, not scheduling" below). Legal single-machine values are
  absent or the sentinel `"local"`; any real node name fails validation closed (no
  node registry exists yet). A raw `host`/address field was deliberately NOT added —
  see the next point.

**The address authority is the agent, not the manifest (do not add a `host` field).**
The `## Discovery` section classifies **addresses as AUTOMATIC — runtime state the
agent owns**; at machine two `resolve` returns real `host:port` derived from the
agent's observed IP, never an operator-authored literal. So `manifest::service_addr`'s
`127.0.0.1:{port}` is **correct for one machine** and its multi-machine successor is
the agent's resolve answer — NOT a TOML field. A per-service `host` in `fleet.toml`
would be a *second, conflicting* address authority the day resolve grows real
answers. `placement` is the seam host-derivation will hang off (node → the agent
running it → that agent's observed address); the manifest annotates WHERE a service
runs, the agent discovers its ADDRESS.

**What still assumes one machine (known-gaps, not defects).** These become failing
assertions the moment the planned real-hardware multi-machine proof runs (see "The
multi-machine proof is planned against real hardware" below — master on the Windows
box + agent on the MacBook over a real LAN). Until then they are the correct M1
shape, deliberately deferred:
- **weles's own agent endpoint binds loopback + plaintext** (`agentapi.rs`) — by
  design; the service→agent hop stays local even multi-machine (the OS confirms the
  caller, no certs). The new hop at machine two is **agent↔master mTLS, which does
  not exist yet**. The present-tense master/agent-split prose in "## Multi-machine"
  describes that DESIGNED split; read it as "designed, not yet built" per this errata
  — today master and agent are one process (the section's own "role split" text says
  so).
- **The backend edge's mTLS identity is a `localhost` fiction.** `core/edge`'s
  `DevCA::leaf` mints every server leaf with fixed SANs `localhost`/`127.0.0.1`/`::1`
  and `Client::dial` always presents `ServerName="localhost"`. A real LAN dial passes
  verification only because both sides collude on the same fake identity — zero real
  per-host identity; it will break silently if ever hardened. (`core/remote`'s
  `EdgeDialer` itself dials any numeric `host:port` fine — but `SocketAddr::parse`
  only, so a hostname/DNS answer has no path yet.)
- **Port allocation is one global namespace.** All fleet ports are `fleet.toml`
  literals validated for global uniqueness against a single `AGENT_PORT` (8300);
  "port-minting" is design vocabulary, unbuilt. Multi-machine needs per-(host,port)
  uniqueness or an explicit host-local statement.
- **The operator control plane assumes one local disk + one OS account.**
  `rollout.lock` (`flock`/`LockFileEx` + stdin-pipe lease inheritance) and the
  loopback control endpoint cannot cross a machine. This is correct and stays — it
  coordinates operator invocations against one shared local dev Postgres — but it is
  NOT a distribution mechanism, and nothing here states the **root/privilege**
  requirement for installing the agent as a system service (the one gap the
  "## Not unifying the operator control plane" section leaves unstated).

**Process contract, named honestly.** weles is generic over *domains* (it knows
nothing of accounts/config/admin) but NOT over *process shape*: it assumes the Swaróg
process contract — `PORT`/`EDGE_ADDR` env, a single HTTP listener plus optional
edge/player planes, `GET /readyz` on the HTTP port, `Edge|Http` peer kinds, loopback.
An arbitrary (e.g. C#) service must adopt this contract to be supervised. This is a
stated platform contract, not a bug — but it bounds the "drop in any binary" claim.

Cross-references (these gaps are already reasoned about elsewhere — this errata
consolidates, it does not supersede): loopback-only resolve and the local-only
service→agent hop ("## The service-facing contract: services only ever talk to their
local agent"); the mTLS/CA transport plan ("## Transport: HTTPS + JSON + mTLS, not
QUIC"); port-minting × master-down ("## Open design points"); M1's no-network-hop
scope ("## Tokio: the runtime arrives WITH the HTTPS server").

## Non-negotiables

- **Native OS processes. No containers, no Docker, no Kubernetes.** Rust ships
  static binaries; a container would wrap one file in ceremony. Deploy = copy
  binary + supervise. Resource limits, if ever needed, are direct cgroups/Job
  Objects. Reference point: Guild Wars 1 (2005) ran a custom native-process
  orchestrator with live no-downtime updates, small team, no container tooling.
- **Zero-sharing, both directions — stated precisely (corrected 2026-07-17).**
  Weles never imports a workspace crate, in any direction, ever. The reverse arrow
  is narrower than earlier revisions of this file claimed: **the shipping graph
  (`core/`, `api/`, `modules/`, `cmd/`, `demos/`) never imports Weles** — *verify
  tooling may*, and does: `tools/verifyctl/Cargo.toml:13` has
  `weles = { path = "../../weles" }`, which is what makes the (since-deleted)
  `weles-fleet-parity` stage and the still-live `weles-async-island` stage
  possible at all. A cross-cutting claim ABOUT weles cannot be checked from
  inside weles.

  This distinction is load-bearing, not pedantry: **the shipping-graph half is why
  `core/remote` cannot dev-dep weles**, hence why each side of the wire is tested
  only against its own fake, hence why the managed-mode proof is the ONLY thing
  gating interop. Read it as "one-directional" and that whole chain collapses.

  The only coupling between weles and the shipping graph is a **wire-only JSON
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
exists (`weles/src/fleet_toml.rs` parses the per-service peer list,
`weles/src/manifest.rs::compose_env_with_fleet` composes the env from it — the
2026-07-21 errata's `fleet.toml` move; the machinery was `split_fleet()`/
`compose_env` before that). So `resolve` is scoped per-consumer — never "give me
the fleet map".

`resolve` returns **all live instances**, and **round-robin load balancing is
client-side** in `core/remote`.

### Manual vs automatic

- **Automatic (runtime state Weles owns):** ports (agent-minted), addresses
  (agent IP + port), discovery, client-side LB, process identity (injected at
  spawn).
- **Manual (the manifest, and only the manifest):** services, binary/version,
  replicas, placement, and the static env Weles does not own (`DATABASE_URL`,
  secrets, feature flags) as literals.

Source of truth is the **git-versioned manifest** (the operator-authored
`fleet.toml`, 2026-07-21 errata — was a Rust literal before that) + `plan`/`apply`
(Terraform-style diff, not yet built) — **not** a mutating admin panel, which would recreate the
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
per-entry drift log (the discipline the now-deleted `weles-fleet-parity` stage
used to exemplify; `fleet_toml::validate`'s peer-name-must-be-a-declared-provider
check is today's instance of the same discipline).

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
- **The agent is stateless about the WORKLOAD.** It mints ports locally and
  reports them up (the master persists them for the record); it need not remember
  them, because an agent restart takes the fleet with it and everything is
  re-minted on the way back. **"Stateless" scopes to the workload — processes,
  ports, assignments — and NOT to the agent's own configuration.** A daemon has
  config. In particular the agent persists *who its master is* (see master
  migration below); that is config, not workload state, and the two must not be
  conflated when reading the word "stateless" here.

### Master unavailable = orchestration is down. Accepted.

Running processes keep running (the agent is alive and supervising); everything
that *changes* state stops until the master returns. The agent caches nothing and
remembers no assignment, so a machine whose agent restarts during a master outage
stays dark. At this scale (a handful of machines, ~12 services) that is a fair
trade; the machinery Nomad builds to avoid it solves a problem we do not have.

**Why the SPOF is affordable: a cold start of the orchestrator plus the fleet is
seconds.** Rust binaries, native processes, no image pulls. Proper HA — Raft,
re-attach, caches — is a large problem we are deliberately not solving, and the
boot path is what makes not solving it survivable.

The consequence worth naming: **boot simplicity and boot speed are not UX, they
are disaster recovery.** That is an independent argument for keeping the boot path
short and uncomplicated, and for M2 (binding control before the slow prep
helpers). Two honest qualifications:
- Fleet recovery time is **Σ of per-service healthy times, not max** — boot is
  deliberately sequential (one service spawned and gated at a time). If this ever
  hurts, the lever is a **parallel boot**, not HA.
- The 30–60s prep window (`edgeca` + `seed_admin`) is a **fresh-install** cost,
  not a recovery cost: `mint_ca` short-circuits when both CA files already exist.
  So "prep is eating the recovery budget" is NOT a valid argument for M2; the
  Σ-boot one is.

Corollary: caching in the agent is **not foreclosed** — the service-facing
contract below is identical whether the agent knows the answer or forwards the
question, so a cache stays a pure optimization, addable when a second machine
makes it hurt.

**On one machine the question does not even arise:** the agent minted the ports
and spawned the processes, so it IS the source of truth for everything local. The
master is only needed for peers on *other* machines. Local resolve therefore never
touches the master, cache or no cache.

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

## Who deploys Weles itself (decided 2026-07-16)

A human, with a script in this repo. `weles deploy` stages the *fleet's* binaries;
nothing stages weles. The upgrade is: **stop weles → the fleet dies with it (by
design) → swap the binary → start → the fleet comes back up the normal boot path.**

Self-upgrade therefore inherits "the recovery path IS the normal boot path" for
free — there is no separate upgrade machinery to rot. On a slave the shape is
identical, just driven through the system service manager. The script must
**self-check and refuse to run while weles is alive** (swapping a binary under a
live fleet is the one way to get this wrong).

## Master migration: live-repoint the agents (decided 2026-07-16)

Moving the master to another machine is a **repoint of the agents**, not a DNS
flip. A flip assumes control of a resolver we do not have on a home LAN; a repoint
avoids the question entirely and reuses the master-restart path we already have.

- **The repoint persists.** The agent keeps a small file with its last-known
  master address; the startup flag is **bootstrap only**, consulted when the file
  is absent. Without persistence an agent restart would march back to a
  decommissioned master. (This is config, not workload state — see "stateless"
  above.)
- **Runbook order prevents split-brain:** stop the old master → move the `.db` and
  the CA → start the new master → repoint the agents. There is never a window with
  two masters.
- **Identity is the certificate, not the address** — which is what makes the
  repoint safe: the agent does not trust a host, it trusts a cert. Hence the CA
  travels with the `.db`.
- The repoint command lands on the agent's **local** control endpoint (operator
  trust domain). N agents = a loop in the same script as the self-deploy.

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

### The orchestrator needs its OWN CA — not the backend's edge CA

Authorization on the agent↔master hop is mTLS. The tempting shortcut — reuse the
CA weles already mints via `edgeca` — is **wrong, and it follows from
zero-sharing** (a case the rule catches that no checker would: it hides in a file
path and an env var, never in a `Cargo.toml`).

Concretely: `manifest::compose_env` hands **`EDGE_CA_KEY` — the CA private key —
to every DB-backed service and to the gateway** (they mint their own edge certs
from it). If the control plane trusted that same CA, any service on the box could
issue itself an agent certificate and speak to the master as an agent. The mTLS
would be encryption, not an authorization boundary — the same two-trust-domains
mistake as unifying the operator pipe.

So weles mints **two** CAs, for two purposes:
1. **Its own**, for agent↔master. Minted by weles itself (`rcgen` is an external
   crate — allowed). **Not** via `edgeca`: that is a *backend artifact* staged in
   `deploy/`, and the orchestrator's identity must not depend on the workload's
   binaries being deployed first.
2. **The backend's edge CA**, which it keeps provisioning via `edgeca` as part of
   preparing the fleet — that is workload setup, not orchestrator identity.

Master migration moves the **orchestrator's** CA with the `.db`, not the backend's.

`reqwest` is already in the workspace for the backend side.

## Tokio: the runtime arrives WITH the HTTPS server (decided 2026-07-16)

The M0 plan recorded "no tokio" as *finding #13*, with this rationale: "all sync
std threads; the devctl/processctl patterns we copy are synchronous; timing
decisions via injected Instant". That rationale is **scoped to M0** — supervising
processes and a local operator endpoint. It says nothing about a network server
serving N clients, which did not exist. M1 does not violate the decision; it
outgrows its premise. (An earlier revision of this file called std-only an
*invariant*. That was an overstatement, copied from a readiness review.)

The runtime arrives when it becomes unavoidable — with the first server weles has
to host — as a **contained I/O island on its own thread**, not as a whole-crate
migration and not as a separate no-feature refactor first. Weighed against and
rejected: unifying the operator pipe onto HTTPS (see below), and a tokio-first
refactor whose honest prize is ~60 lines plus concurrent probes.

**Precision (corrected 2026-07-17):** an earlier revision said "with M1's HTTPS
server". That contradicts "M1 scope" below — M1 has **no network hop**, so the
server it actually brings is a **plaintext localhost** one (services → their local
agent). HTTPS and mTLS arrive with the agent↔master hop at machine two. The
runtime lands in M1 regardless; only the transport it carries was misstated.

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

**Gap surfaced by that review — closed by the macOS port (2026-07-17):** the
operator control endpoint originally supported Windows and Linux only, despite
weles claiming macOS support. The fix was a native macOS UDS peer-cred backend
(`LOCAL_PEERCRED` + `LOCAL_PEERPID`, `weles/src/control.rs` under
`#[cfg(target_os = "macos")]`), not weakening local auth everywhere. `weles up`
and the `weles-managed-gateway` verify stage now run on macOS.

## Rejected — do not re-propose

- **Adopting Nomad (or Consul/etcd/Agones/k8s) instead of building this.** Nomad
  is the **reference architecture, not a candidate**. The owner wants his own
  orchestrator; that is a sufficient and non-negotiable reason on its own, and it
  is not open to re-litigation by a fresh context trying to be helpful. (For the
  record, adoption would also collide with decisions above — no containers, native
  processes, zero-sharing, Windows-first, one small binary — but those are
  supporting facts, not the reason.) Do not propose adoption. Do mine Nomad for
  shape, and say when we deviate.
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
- **Port minting vs the parity gate — the first M1 design task, not a footnote.**
  *(Superseded framing, 2026-07-21 errata: `weles-fleet-parity` no longer exists
  at all — it was deleted when the fleet definition moved to `fleet.toml`, not
  eroded service-by-service as managed mode landed. The reasoning below is kept
  for the still-open question it argues — how minted/managed peers interact
  with whatever proves fleet correctness — but its anchor is now
  `fleet_toml::validate`, not a live parity stage.)* The now-deleted, formerly
  blocking `weles-fleet-parity` stage asserted weles's static ports equal
  processctl's. A minted port has no static value to compare, and a managed
  service's peer env (`*_EDGE_ADDR`) does not exist at all — it resolves. So the
  gate loses exactly the comparison it was built for (peer wiring), and weles has
  no other gate. Note the replacement is NOT a weaker static assertion "modulo
  ports" — it is a **live test of managed mode**.

  **The ordering constraint that makes this tractable:** *a service's port cannot
  be minted until every consumer of it resolves.* Give inventory a random port and
  gateway/admin still hold `INVENTORY_EDGE_ADDR` in env. So managed mode spreads
  along dependency edges, consumer-first, and minting is the LAST step, not the
  first. Consequence: the gate need not fall all at once — a service leaves the
  port assertion exactly when it goes managed, each departure paid for by a live
  proof of that service, and everything still standalone stays fully asserted.
  Natural first managed service: **gateway** — it dials six peers and nothing dials
  its edge, so it can start resolving without forcing anyone else to move (its own
  port stays static regardless; it is the public front door and must be
  predictable). See [weles-fleet-parity](weles-fleet-parity.md).
- **Minting × master-down.** With minted ports, a crash during a master outage
  means the agent restarts the service on a NEW port that nobody can discover
  until the master returns — so "running processes keep running" quietly becomes
  "…until one crashes". This is the first real argument for an agent-side cache:
  with minting, the cache stops being a pure optimization and becomes a
  *consequence* of the minting decision. **Not a day-one problem:** on one machine
  the agent minted the ports itself and is the local source of truth, so this only
  bites for *remote* peers at machine two. Record it so M1 does not design it away
  by accident.
- **Replica-safety is a module prerequisite**, not a Weles feature: before any
  `replicas: 2`, rating's MMR must be DB-backed and the relay needs an advisory
  lock per `EVENTS_ORIGIN`.
- **Round-robin LB is not "a field change".** Re-resolution is cheap — `Stub`
  holds `peer_addr` as an unparsed `String` and parses at dial, so swapping the
  string for a resolver call is small. **Load balancing is not:** N live instances
  means a connection pool per instance, a selection policy, a notion of instance
  health, and an interaction with `RetryMode` when an instance dies mid-request.
  Estimate them separately or the estimate is wrong.

## M1 scope — what it actually is

**M1 is a shape-proving milestone, not a feature milestone. Say so out loud.** At
one machine with twelve services on static ports, minting solves nothing that is
broken today (it exists for replicas, which are out of scope pending module
prerequisites) and `resolve` answers what env already answers. The point is to
build the master/agent seam and the managed contract **while the stakes are low** —
that is a legitimate reason, but it is the reason, and nobody should later hunt for
the feature that minting delivered.

**M1 has no network hop at all.** Master and agent are one process, so there is
nothing between them to secure; services reach the agent over localhost, where the
contract deliberately uses no certificates. So M1 is: a **local** HTTP+JSON
contract, no TLS, no orchestrator CA. The decisions above (network transport,
HTTPS, mTLS, own CA) stand — they simply have no hop to run on until machine two.

In M1:
- The master/agent role split, internally — one binary, two roles (Nomad's shape).
- The local service-facing contract (`hello`/`resolve`) served by the agent, and
  the client in `core/remote` — with tokio arriving as the contained I/O island
  behind that server.

  **`resolve`'s two "no address" answers are different, and the line is drawn
  now** (agent side shipped 2026-07-17, `weles/src/agentapi.rs`) — because
  `resolve` returns *all live instances*, "nothing is live" is a natural value
  in that shape and must not be conflated with "no such thing":

  | answer | means | who produces it |
  |---|---|---|
  | `404 {"code":"unknown_peer"}` | this `(provider, kind)` is **not a thing in this topology** — closed-world, derived from the manifest, and not coming | M1 (the only one it can produce) |
  | `200 {"addrs":[]}` | it **is** a thing; **nothing is live right now** | M2, once liveness exists |

  A client may treat 404 as fatal-and-final; it may not treat `[]` that way. M2
  puts zero instances in the list, never in the 404.

  Every non-2xx also carries `{"code":…,"error":…}` with `code` a closed enum
  (`unknown_route` / `unknown_peer` / `bad_request` / `internal`). `code` is the
  only thing a client may branch on — `unknown_route` (an agent that does not
  speak the contract) and `unknown_peer` (a fact about the fleet) share status
  404 and must never be confused, since the first is fatal for the caller and
  the second may be a legitimately absent passthrough origin.
- **Port minting**, agent-side — ordered by the consumers-resolve-first constraint
  above, so it lands per-service rather than fleet-wide.
- **SQLite** for master runtime state (minted ports reported up, deploy history,
  instance records).
- **`weles rollback`** — independent of everything else and cheap (generations,
  `current` and sha256 already exist); it needs **sha-verify-on-read**, which is
  its integrity half, not a separate feature.
- **The managed-mode proof** — weles has no other gate, and every service that goes
  managed leaves the parity gate's port assertion.
- Along the way: **M2** (bind control before the slow prep helpers) and
  `DOWN_TIMEOUT` derived from fleet size.

Deferred to machine two (recorded, not forgotten): the network hop and mTLS, the
orchestrator's own CA, agent-side caching for remote peers, master migration by
repoint, and how binaries reach a second machine.

Not in M1: replicas (module prerequisites), round-robin LB, gateway
routing-as-data via `describe()` (which still has an open research question), and
re-attach (rejected outright).

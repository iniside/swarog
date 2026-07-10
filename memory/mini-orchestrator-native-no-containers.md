---
name: mini-orchestrator-native-no-containers
description: "Future mini-orchestrator runs NATIVE processes, no containers/Docker/k8s; GW1-2005 is the reference point that this is feasible"
metadata: 
  node_type: memory
  type: project
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Decision (2026-07-09): the future mini-orchestrator for this backend will manage
**native OS processes — explicitly NO containers, no Docker, no Kubernetes**.
Scope sketch (not started): supervisor (spawn/restart/backoff off `/readyz`) +
multi-peer round-robin in `remote::Stub` + rolling deploy with QUIC drain.
Prerequisite before any `replicas: 2`: module replica-safety (rating MMR to DB,
advisory lock on relay per EVENTS_ORIGIN).

**Why:** Rust ships static binaries — containers would wrap one file for
ceremony; native supervision is simpler (no containerd, no PID-1 traps); resource
limits, if ever needed, are direct cgroups. Reference point: Guild Wars 1 (2005)
ran a custom native-process orchestrator with live no-downtime updates — small
team, no container tooling. Sized at ~10 agent iterations over a few days.

**Separate application with ZERO sharing** (decided 2026-07-09, Lukasz explicit:
"zero współdzielenia poza folderem głównym"): own crate + binary (likely top-level
`orchestrator/`, not `core/`, not a lifecycle::Module — it embodies topology while
modules are topology-blind, and it outlives the processes it spawns). No shared
crates, **no use of the backend's Postgres** (the earlier registry-table +
LISTEN/NOTIFY idea is SUPERSEDED — don't re-propose it). It knows the backend only
via the external process contract: spawn binary with env vars (`*_EDGE_ADDR`,
`EVENTS_SUBSCRIBERS`, `EVENTS_ORIGIN`, `DATABASE_URL`), poll `GET /readyz`,
signal/kill + exit codes. **Config/discovery model: control-plane PULL, Lukasz's preferred shape
(2026-07-10, supersedes the env-template-manifest sketch):** each process is
spawned with ONE bootstrap env (`ORCHESTRATOR_URL` + service/instance identity)
and phones home ("centrala") with a small wire client — hello/registration +
`resolve(peer:plane)` for the addresses its stubs need (the kubelet/Envoy-xDS
pattern). Knowledge of dependencies stays in the consumer (its stubs already
declare them) — no env-name mapping in any manifest; manifest shrinks to
services/binaries/replicas. The client is backend-owned code in `core/`
(likely `core/remote`, so Stub can re-resolve → replicas/address changes without
consumer restarts); wire-only JSON contract, own types on each side — zero shared
crates preserved. Registration proper only matters for processes the centrala
didn't spawn (future game servers). **Convention over configuration — RESOLVED
(2026-07-10, Lukasz): the contract is OPT-IN per process, backend-side optional.**
Two disjoint boot modes with a deterministic switch — `ORCHESTRATOR_URL` set ⇒
managed mode (pull config from centrala); unset ⇒ standalone mode (classic
`*_EDGE_ADDR` env, exactly today's path, stays a supported first-class mode like
monolith-vs-split). NOT config layering/precedence — one decision at process
start ([[config-as-code-anti-magic]]). An unmanaged process simply gets no
management (no restarts/replicas/rolling deploy) — no other penalty. The managed
convention is a tiny language-neutral contract: (1) read `ORCHESTRATOR_URL` +
identity, (2) hello + resolve peers, (3) expose `/readyz`, (4) drain on a
**wire command from the centrala** (NOT SIGTERM — platform-neutral, see
cross-platform below; signal/TerminateProcess is only the unresponsive-process
fallback);
any-language service (e.g. future Go svc) implements it itself — the orchestrator
supports nothing per-service; `core/remote` is merely our Rust client.
`cmd/server` (monolith) satisfies it trivially (no peers to resolve). Its own
state lives in **SQLite** (decided 2026-07-10; rusqlite `bundled` — embedded,
no server, cross-platform, keeps the one-binary deploy): runtime state only
(minted ports, instances/PIDs/versions, deploy generations) — desired state is
the git manifest, and most runtime state is soft (reconcilable from agent
reports after master restart, kubelet-style); SQLite persists what live
processes can't tell you (deploy history, port assignments of dead instances,
API-mutated desired state). No state replication — master migration = move the
.db file or rebuild from manifest + reconciliation. Open design
point: env is read at spawn, so a peer address change = consumer restart (or
later stub re-resolve). Platform-side pieces still land at existing seams:
multi-peer round-robin in `core/remote`, drain in `core/edge`/`httpmw`,
replica-safety in modules.

**Manual-vs-automatic split (2026-07-10):** automatic runtime state = ports
(agent-minted), addresses (agent IP + port), discovery (consumer's stubs declare
needs, resolve answers), client-side round-robin LB in `core/remote` (resolve
returns all live instances), process identity (injected at spawn). Manual = the
MANIFEST only: services, binary/version, replicas, placement, and static env the
orchestrator doesn't own (`DATABASE_URL`, secrets, feature flags) as literals.
Source of truth is the git-versioned Rust manifest + `orchestrator plan/apply`
(Terraform-style diff) — NOT a mutating admin panel (UI-edited topology recreates
the where-did-this-value-come-from drift, [[config-as-code-anti-magic]]).
Panel/CLI is read-only observability + imperative ops (status/restart/deploy);
CLI first, own tiny web UI later at most — never a page in the backend's `admin`
module (zero-sharing both ways). Only runtime-mutable desired state: future game
-server `replicas` via API from the server-management module.

**Cross-platform REQUIREMENT (2026-07-10, Lukasz): the orchestrator must run on
Windows/Linux/macOS.** All platform abstraction lives ONLY in the agent — a small
trait (`spawn_supervised`/`kill_tree`/`alive`) with cfg impls: process
groups (unix) vs Job Objects (windows); master and the wire contract are
platform-free. Graceful drain goes through the wire contract (not signals —
SIGTERM doesn't exist on Windows); resource limits are an optional capability
(cgroups/Job Objects, mac none) the agent applies best-effort and reports
unsupported. Verify: Windows (dev box) + Linux blocking; a real MacBook sits next to the dev
box (2026-07-10) — mac is a live test platform AND the second physical machine
for the multi-machine proof (master on Windows + agent on mac over real LAN:
non-loopback resolve, mTLS across a physical network, mixed-platform fleet).
Bonus argument: one Rust binary replaces the duplicated `.sh`/`.ps1` operational
scripts — it reduces the repo's platform surface.

**Multi-machine (2026-07-10): master + per-machine agents (the Nomad
server/client shape), NO overlay network, NO master election.** Overlay exists in
k8s only for IP-per-pod; our native processes share host networking, so resolve
just returns real `host:port` — plain LAN/VPC routing, and the mTLS QUIC edge
already assumes an untrusted network (cross-internet fleets at most get a flat
host-level WireGuard, never per-process overlay). Master holds
manifest/desired-state/resolve API; agents are dumb spawn/kill/status executors
connecting outbound to the master; processes still see only `ORCHESTRATOR_URL`
(contract unchanged). Single master, local-disk state, no Raft — replicated
consensus is the threshold where you start rewriting Consul; master down =
running processes keep running (addresses resolved, agents supervise locally),
only management degrades until restart. Placement = manifest annotation, not
scheduling.

**How to apply:** when the orchestrator work starts, don't propose
Docker/k8s/containerd anywhere in the design and don't fold the orchestrator into
`core/` or the module system; deploy = copy binary + supervisor.
Related: [[server-management-is-a-domain-module]], [[team-is-solo-plus-agents-forever]].

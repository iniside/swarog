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
signal/kill + exit codes. Discovery collapses into port assignment: the
orchestrator mints ports and injects addresses at spawn (what split-proof.ps1 does
by hand today) — no registration mechanism, zero backend code change. Env-name
knowledge lives in the MANIFEST, not orchestrator code (2026-07-09, Lukasz's
"how would it know the module's config surface" objection): the operator-authored
manifest maps opaque env names to placeholders (`{port:self:edge}`,
`{addr:characters:edge}`); the orchestrator only substitutes values it owns
(ports/addrs) — the docker-compose/k8s-yaml division of knowledge. Its own
state (manifest, PIDs, ports) lives locally (file/sqlite/in-memory). Open design
point: env is read at spawn, so a peer address change = consumer restart (or
later stub re-resolve). Platform-side pieces still land at existing seams:
multi-peer round-robin in `core/remote`, drain in `core/edge`/`httpmw`,
replica-safety in modules.

**How to apply:** when the orchestrator work starts, don't propose
Docker/k8s/containerd anywhere in the design and don't fold the orchestrator into
`core/` or the module system; deploy = copy binary + supervisor.
Related: [[server-management-is-a-domain-module]], [[team-is-solo-plus-agents-forever]].

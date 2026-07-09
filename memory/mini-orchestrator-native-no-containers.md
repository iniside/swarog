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
Postgres-table service registry (heartbeat + LISTEN/NOTIFY, `CachedConfig` pattern)
+ multi-peer round-robin in `remote::Stub` + rolling deploy with QUIC drain.
Prerequisite before any `replicas: 2`: module replica-safety (rating MMR to DB,
advisory lock on relay per EVENTS_ORIGIN).

**Why:** Rust ships static binaries — containers would wrap one file for
ceremony; native supervision is simpler (no containerd, no PID-1 traps); resource
limits, if ever needed, are direct cgroups. Reference point: Guild Wars 1 (2005)
ran a custom native-process orchestrator with live no-downtime updates — small
team, no container tooling. Sized at ~10 agent iterations over a few days.

**Separate application, NOT part of the core/ backplane** (decided 2026-07-09):
own crate + binary (likely top-level `orchestrator/`, not `core/`, not a
lifecycle::Module — it embodies topology while modules are topology-blind, and it
outlives the processes it spawns). Shares only Postgres (registry) + the `/readyz`
convention. Platform-side pieces still land at existing seams: multi-peer
round-robin in `core/remote`, drain in `core/edge`/`httpmw`, replica-safety in
modules.

**How to apply:** when the orchestrator work starts, don't propose
Docker/k8s/containerd anywhere in the design and don't fold the orchestrator into
`core/` or the module system; deploy = copy binary + supervisor.
Related: [[server-management-is-a-domain-module]], [[team-is-solo-plus-agents-forever]].

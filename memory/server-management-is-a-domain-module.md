---
name: server-management-is-a-domain-module
description: "Game-server management will be a specialized domain module (fortress), NOT generic service discovery / not Consul-style infra"
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

Decision (2026-07-09): dedicated game-server management (session allocation, instance
lifecycle, matchmaker→instance routing) will be a **specialized domain module** in
`modules/` — a fortress with its own contract crates — NOT a generic service-discovery layer
and NOT Consul/etcd/Agones-style infra.

**Why:** the services topology is static (discovery = env/DNS at `cmd/*`); the only *dynamic*
discovery-shaped problem in a game backend is allocating stateful session instances, and
that's a domain problem (players_count, allocation state, heartbeats) — a "table + heartbeat +
LISTEN/NOTIFY" pattern owned by a module.

**Boundary to the orchestrator:** the module commands the orchestrator over a wire API
(external-system client pattern like accounts→Epic), address injected by `cmd/*`, module stays
topology-blind; orchestrator knows only process verbs (spawn/kill/status), the module owns all
game semantics — full design in [[mini-orchestrator-native-no-containers]].

**How to apply:** don't propose generic service-discovery infra or a registry in `core/`; plan
it as `modules/<name>` + `api/<name>/`. Related: [[never-monolith-only-features]].

---
name: server-management-is-a-domain-module
description: "Game-server management will be a specialized domain module (fortress), NOT generic service discovery / not Consul-style infra"
metadata: 
  node_type: memory
  type: project
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Decision (2026-07-09): when the project gets dedicated game-server management
(session allocation, instance lifecycle, matchmaker→instance routing), it will be
built as a **specialized domain module** in `modules/` — a fortress like any other,
with its own contract crates — NOT as a generic service-discovery layer and NOT by
adopting Consul/etcd/Agones-style generic infra.

**Why:** the services topology of this backend is static (discovery = env/DNS at
the `cmd/*` composition roots); the only *dynamic* discovery-shaped problem in a
game backend is allocating stateful session game-server instances, and that is a
domain problem (players_count, allocation state, heartbeats) — the
"table + heartbeat + LISTEN/NOTIFY" pattern, owned by a module.

**Boundary to the orchestrator** (2026-07-09): the module commands the
orchestrator over a wire API (HTTP/JSON) — no shared crates (zero-sharing rule,
[[mini-orchestrator-native-no-containers]]), external-system client pattern like
accounts→Epic; orchestrator address injected by `cmd/*` (the
`Gateway::with_passthrough` pattern), module stays topology-blind. Semantics
split: orchestrator knows only process verbs (spawn/kill/status →
instance_id+addr), the module owns ALL game semantics (sessions, players,
allocation policy) in its own schema. Sync request/response (not the bus).
Open points for the plan: dead-instance feedback (poll status wins over webhook),
API auth (localhost+token vs edgeca mTLS).

**How to apply:** don't propose generic service-discovery infrastructure or a
registry in `core/`; when server management comes up, plan it as
`modules/<name>` + `api/<name>/` contracts. Related: [[never-monolith-only-features]].

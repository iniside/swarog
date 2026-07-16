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

**Why:** the services topology is static (in standalone mode discovery is env at `cmd/*`; in
Weles-managed mode it is `resolve` — either way it is *static wiring*, not a dynamic registry);
the only *dynamic* discovery-shaped problem in a game backend is allocating stateful session
instances, and that's a domain problem (players_count, allocation state, heartbeats) — a
"table + heartbeat + LISTEN/NOTIFY" pattern owned by a module.

**Boundary to the orchestrator:** the module commands Weles over a wire API (external-system
client pattern like accounts→Epic), address injected by `cmd/*`, module stays topology-blind;
the module owns all game semantics. **Weles design is in the repo: `docs/reference/weles-design.md`**
— read it, do not reconstruct it from memory.

Correction (2026-07-16): "the orchestrator knows only process verbs (spawn/kill/status)" was
too narrow and is now wrong in two ways. Weles has a master/agent role split: the **agent** does
the process verbs plus the restart policy and local supervision; the **master** owns the
manifest, SQLite and a `resolve` registry — which IS service discovery, just at the orchestrator
layer. This does not weaken the decision here: no discovery infra in `core/`, and game-server
management stays a `modules/` fortress. It only means Weles is a richer counterpart than the
"dumb spawn/kill executor" this note assumed.

**How to apply:** don't propose generic service-discovery infra or a registry in `core/`; plan
it as `modules/<name>` + `api/<name>/`. Related: [[never-monolith-only-features]].

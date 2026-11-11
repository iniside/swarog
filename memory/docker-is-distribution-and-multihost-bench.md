---
name: docker-is-distribution-and-multihost-bench
description: "DECIDED 2026-11-11: Docker/compose support is wanted — as a distribution path (\"compose up and it works\") and as a multi-host test bench (containers = simulated machines); Weles stays native"
metadata:
  node_type: memory
  type: project
  originSessionId: fb0529fe-4b60-43b5-a11a-9d60ec9ddba8
  modified: 2026-11-11T16:55:34.960Z
---

Lukasz reversed the blanket "no Docker" stance on 2026-11-11: many users expect
`docker compose up` to just work, and "docs can always be fixed, reality I can't
change alone". Prior docs saying "no containers" (README, weles-design.md
Non-negotiables, testing.md) are to be UPDATED to match, not cited as blockers.

Scope of the decision:
- **Distribution:** a Dockerfile + compose (monolith first; split later).
- **Multi-host test bench:** each container plays a MACHINE (own IP/netns), running
  native processes (eventually under a Weles agent) — exposes hidden 127.0.0.1 /
  `localhost`-SNI assumptions, allows kill / network-partition / netem fault drills.
  Limits: shared kernel+clock (no clock skew), gentler MTU/NAT — final check on real VMs.
- **Unchanged:** Weles itself supervises native processes (Docker is not its runtime);
  integration tests still hit local Postgres directly ([[reference_local_postgres]]);
  a compose fleet counts as a rollout under "one test rollout at a time".

Known blockers found 2026-11-11 (research only, nothing planned yet):
- `core/remote/src/lib.rs` parses peer addrs as literal `SocketAddr` → compose
  service names fail (static IPs work as a stopgap; real discovery = Weles `resolve`).
- `core/edge` gives every process `EDGE_CA_KEY` and self-mints a `localhost`-SAN leaf
  → no per-service identity; multi-host needs CA-key-off-node + per-service leaves
  issued by an INSTALLER (typed Rust tool, [[config-as-code-anti-magic]]).

**How to apply:** don't refuse or hedge Docker work by citing old "no containers"
prose; fold the docs updates into the plan. Related: [[mini-orchestrator-native-no-containers]].

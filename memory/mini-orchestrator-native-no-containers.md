---
name: mini-orchestrator-native-no-containers
description: "WELES — the mini-orchestrator (top-level weles/): design now lives in docs/reference/weles-design.md; M0+pre-M1 shipped, M1 next"
metadata: 
  node_type: memory
  type: project
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

**The design is in the repo, not here: `docs/reference/weles-design.md`** (written
2026-07-16). It carries the settled shape — native processes/no containers,
zero-sharing via a wire-only JSON contract, the two disjoint boot modes
(`ORCHESTRATOR_URL` set ⇒ managed, unset ⇒ standalone first-class), the client in
`core/remote` (NOT a weles crate — so `Stub` can re-resolve), resolve scoped to the
consumer's declared deps, client-side round-robin LB, gateway routing-as-data via
`describe()`, SQLite for runtime state only, master+agents with no overlay/no
election, the rejected list, and the open points. **Read that file before proposing
any Weles shape** — this memory is a pointer, not a second copy.

Why the file exists: the design lived only in memory for a week, so a fresh context
proposed a client crate in `weles/` imported by the backend — contradicting the
decided wire-only contract. Memory is per-machine and invisible to whoever writes
the plan. Durable design belongs in `docs/reference/`.

**Status:** M0 shipped 2026-07-15 (`docs/plans/2026-07-15-1055-weles-m0-plan.md`);
pre-M1 backlog #2-#5 + hardening P1-P6 + review punch-list A1-A3 all shipped by
2026-07-16 (`docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md` is
the current readiness verdict + carried-in items). M1 = rollback / hello+resolve /
SQLite / port minting — not started.

Two design corrections made mid-M0 by Lukasz, now binding and easy to re-violate:
(a) **the orchestrator NEVER builds** (a linker failure exposed the creep);
(b) **own artifact dir `deploy/`**, never `target/debug` — no Cargo-isms.

**Open, tracked, NOT weles-core:** devctl test flake under full-workspace
parallelism (`down_waits_for_stopped…`, passes in isolation, uninvestigated); the
one-line `processctl` BUILD_ENV_ALLOWLIST gap (SYSTEMDRIVE/ProgramData). Deferred
by decision: deploy-scoped lock for concurrent `weles deploy` (one-deploy-at-a-time
is operator discipline for now).

**How to apply:** never propose Docker/k8s/containerd, never fold Weles into
`core/` or the module system, never let a crate cross the Weles↔backend boundary.
Related: [[server-management-is-a-domain-module]], [[config-as-code-anti-magic]],
[[didnt-forget-scripts-must-self-check]], [[team-is-solo-plus-agents-forever]].

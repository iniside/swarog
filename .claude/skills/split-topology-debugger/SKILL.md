---
name: split-topology-debugger
description: Diagnose "works in the monolith, fails/hangs/404s in the split" problems in this repo — registry key mismatches, missing stubs, edge registration, mTLS certs, PEER_SLOT wiring, gateway routing, event delivery across processes. Use whenever split-proof fails, a *-svc process fails startup, a request 404s/401s only through gateway-svc, an event consumer never fires cross-process, or the user says "w splicie nie działa", "split-proof failed", "działa tylko w monolicie". Diagnose by tracing — do not fix blind.
---

Read and follow the canonical skill at `../../../.agents/skills/split-topology-debugger/SKILL.md`. Do not maintain a second copy.

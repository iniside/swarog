---
name: edge-stub-no-reconnect-after-peer-restart
description: "CLOSED as not-reproducible (2026-07-15): B1 'stub never re-dials, permanent 404' REFUTED — transport recovers on both teardown shapes; regression tests pin it; if permanent 404 returns, suspect data/environment, never core/remote"
metadata: 
  node_type: memory
  type: project
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Originally found 2026-07-15 by the Weles M0 Step 7 chaos test (kill + auto-restart
characters-svc under live traffic) as "permanent silent 404 on GET /inventory/{cid},
module→module stub never re-dials". **Same-day diagnosis rollout REFUTED the
transport hypothesis** (plan + errata:
`docs/plans/2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md`; closure:
`docs/status/2026-07-15-1745-b1-stub-redial-diagnosis-status.md`).

**What is actually true:**
- `Reconnecting`'s reset gate (core/remote) only resets on ConnectionFatal — but
  BOTH teardown shapes (graceful close AND hard kill, same-port rebind) surface as
  ConnectionFatal and self-heal. StreamLocal pinning does not fire on this path.
  Pinned by regression tests (commit `cde5282`): `core/remote/src/redial_tests.rs`
  + `core/remote/tests/abrupt_kill_redial.rs` (child-process peer, TerminateProcess).
- Live Weles split re-run of the chaos scenario: kill → ~30s hanging call ended
  408 (idle-detection window × HTTP_REQUEST_TIMEOUT_MS) → 200 permanently. One
  503 on the first post-restart create (gateway evicting its own dead client) →
  201. **No 404 ever appeared.** Product code was unchanged since the original
  observation.
- A pinned dead transport CANNOT produce 404 on this path anyway: inventory maps
  transport `Err` → 503; 404 requires a decoded `Ok(None)` from a LIVE
  characters-svc (`modules/inventory/src/service.rs:61-77`).

**How to apply:** if a permanent 404 on the inventory→characters path shows up
again, do NOT go to core/remote/core/edge — investigate the data/environment
layer of that session (character id mismatch, respawn env, DB state; decision
table branch (D) in the plan). Known cost, not a bug: ~30s detection window
after a hard peer kill during which one call hangs (then 408/503); shortening it
(per-call edge deadline / shorter idle) is a separate deliberate decision.
Deferred, not implemented: `[B1-REDIAL]` splitproof recovery assertion and the
gateway-parity evict-without-close gate change (would be semantics alignment,
not a fix). [[verify-the-at-risk-path-not-the-safe-one]]

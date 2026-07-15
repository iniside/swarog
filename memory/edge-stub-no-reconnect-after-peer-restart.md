---
name: edge-stub-no-reconnect-after-peer-restart
description: "OPEN BUG (backend, not weles): module→module edge stub (inventory→characters Ownership) never re-dials after the peer process restarts — permanent silent 404s"
metadata: 
  node_type: memory
  type: project
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Found 2026-07-15 by Weles M0 Step 7 chaos test (kill + auto-restart characters-svc
under live traffic — a topology state nothing ever produced before: devctl tears
the whole fleet down on any crash, and splitproof's rdy_dead only asserts the
GATEWAY's readyz recovery, never a module→module call after a peer restart).

**Symptom:** after characters-svc is killed and restarted (same port :9000),
`GET /inventory/{cid}` returns 404 forever — also for characters created AFTER
the restart — while the durable write path works (starter grant lands in
`inventory.holdings`). inventory-svc readyz stays green, no edge errors logged.

**Evidence-backed diagnosis:** the sync `Ownership` authz call
(inventory-svc → characters-svc over the internal mTLS QUIC edge) fails on the
stale connection and is mapped to 404 silently; the gateway's client DOES recover
(create character 201 through gateway→characters works post-restart) — asymmetry:
gateway stub path re-dials, module→module consumer stub path does not.

**Why:** the fix belongs at the authority in `core/remote`/`core/edge` client
(reconnect-on-dead-connection for consumer stubs, matching the gateway path), and
MUST ship with a committed splitproof assertion "module→module call succeeds
after peer restart" ([[verify-the-at-risk-path-not-the-safe-one]]).

**How to apply:** don't treat single-svc-restart flows as proven by existing
splitproof; when this fix lands, also re-run the Weles chaos scenario
(kill characters-svc → restart → create character → poll starter_sword via
GET /inventory). Weles M0 acceptance was completed WITH this known-open bug.

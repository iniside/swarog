---
name: durable-event-plane-bus-owned
description: The async transport (outbox/inbox/relay/HTTP) is now hidden behind the bus via modules/messaging; modules never branch on topology
metadata: 
  node_type: memory
  type: project
  originSessionId: 9daf9937-49a2-46ca-88f2-a2c9a48ebd40
---

The async dual-path debt (a module carrying BOTH `bus.On` AND a hand-wired
`POST /events/*` HTTP sink, "one path per topology") was RESOLVED 2026-07-07
(plan `docs/plans/2026-07-07-1527-bus-owned-transport-plan.md`, Steps 1-9, verified
live). Do NOT re-propose or re-introduce the old per-module outbox/inbox/sink pattern.

**Two planes, chosen by durability intent, never by topology:**
- Best-effort: `bus.Emit` / `bus.On` — in-process fanout, zero DB (unchanged). For
  in-memory/idempotent reactions (rating, `inventory.onConfigChanged`).
- Durable: `bus.EmitTx(tx, ...)` / `bus.OnTx[T](et, subscriber, h)` /
  `bus.OnTxRaw(topic, subscriber, h)` (untyped, for audit-style verbatim logging).
  Exactly-once, transactional, topology-transparent.

**`modules/messaging`** owns schema `messaging` (outbox + per-`(event_id,subscriber)`
inbox), implements `bus.Transport`, installs it via `ctx.Bus.SetTransport` in `Register`
(phase 1). It is the ONLY module importing `outbox`; the leaf `bus/` still imports no
module (the Transport interface is defined in `bus/`, implemented by messaging).

**Single-owner relay (the load-bearing correctness rule):** every outbox row is stamped
`origin` (env `MESSAGING_ORIGIN`, stable per process); a process's relay drains ONLY
`WHERE origin=$self ... FOR UPDATE SKIP LOCKED`, so a foreign-origin relay can never
swallow another process's event. This fixed a BLOCKER the plan review caught. Regression:
`outbox.TestRelayDrainsOnlyOwnOrigin` + `scripts/smoke-split-messaging.sh` (evidence:
`docs/2026-07-07-1654-messaging-split-verified.md`).

**When adding a new cross-process event:** declare `bus.Define`, emit via `EmitTx(tx,...)`
in the producer's domain tx, subscribe via `OnTx`; add `Requires("messaging")` to both
producer and consumer; every hosting process must register `&messaging.Module{}` (last in
`mods`) and set `MESSAGING_ORIGIN` + `EVENTS_SUBSCRIBERS` (topic=`<peer>/events`). No
per-topic route, no hand-written inbox. See [[never-monolith-only]], [[verify-the-at-risk-path-not-the-safe-one]].

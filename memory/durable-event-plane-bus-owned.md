---
name: durable-event-plane-bus-owned
description: "Durable events = app-owned PULL plane (core/asyncevents); keeps only what's NOT in CLAUDE.md — xid8/generation ordering + PITR fencing, and the delivery-history lineage of superseded designs (don't re-propose them)"
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

The current model (publisher owns the event, consumer owns the subscription;
`on_tx(SubscriptionSpec{id,start})`; no inbox, exactly-once for TransactionalPg; poison
backs off + pauses) is fully in CLAUDE.md seam #3. This memory keeps only what ISN'T there:

**Ordering / fencing mechanics:** position = `(generation, producer_xid xid8, tie_breaker)`,
NEVER bigserial (INSERT vs COMMIT order differ). Readers gate current-generation rows by
`producer_xid < pg_snapshot_xmin(pg_current_snapshot())` — slow commits delay the frontier,
never get skipped. `generation` + `plane_meta.system_identifier` fence restores/PITR
(`eventctl bump-generation`). ONE writer: SQL `asyncevents.append_event`; module SQL may call
plane FUNCTIONS, never plane tables (archcheck).

**Delivery-history lineage (each supersedes the previous — do NOT re-propose the older):**
per-module outbox/inbox/sink → module `messaging` → app-owned push plane
(outbox→relay→`POST /events`→inbox, 2026-07-09) → **pull event log (2026-07-09/10)**.
`EVENTS_SUBSCRIBERS`/`EVENTS_ORIGIN`/`POST /events`/relay/inbox/`core/outbox` are DELETED and
archcheck-banned — don't re-propose them, messaging-as-a-module, or per-module sinks.

**Replica-local caches are NOT durable subscriptions** (consumer group ⇒ only one replica
would refresh): use `core/invalidation` (LISTEN/NOTIFY broadcast + authoritative refresh,
first-refresh-or-fail at start). See
[[asyncevents-single-invocation-parallelism-deadlocks]] for the one-invocation test gotcha,
and [[never-monolith-only-features]].

---
name: durable-event-plane-bus-owned
description: Durable events = APP-OWNED plane (core/asyncevents, DB ⇒ plane, injected at Context construction), since 2026-07-10 a PULL model — XID-ordered shared Postgres log + consumer-owned subscriptions with transactional checkpoints. Push relay/POST /events/inbox/EVENTS_* are GONE (don't re-propose). core/bus stays sqlx-free (AnyTx/Delivery); replica-local caches use core/invalidation, never durable subs
metadata: 
  node_type: memory
  type: project
  originSessionId: 9daf9937-49a2-46ca-88f2-a2c9a48ebd40
---

Delivery history (each supersedes the previous — do NOT re-propose the older):
per-module outbox/inbox/sink (debt, resolved 2026-07-07) → module `messaging` →
app-owned push plane (outbox → relay → `POST /events` → inbox, 2026-07-09) →
**pull event log (2026-07-09/10, plan
`docs/plans/2026-07-09-2234-durable-event-log-fresh-plan.md`, Steps 1–12,
commits `e5194d3`…; DB wiped at cutover — user accepted fresh start).**

**Current model — "publisher owns the event, consumer owns the subscription":**
- One shared XID-ordered log `asyncevents.events`; position =
  `(generation, producer_xid xid8, tie_breaker)`, NEVER bigserial (INSERT vs
  COMMIT order). Readers gate current-generation rows by
  `producer_xid < pg_snapshot_xmin(pg_current_snapshot())` — slow commits delay
  the frontier, never get skipped. `generation` + `plane_meta.system_identifier`
  fence restores/PITR (`eventctl bump-generation`).
- ONE writer implementation: SQL function `asyncevents.append_event` (shared
  advisory lock → read generation → INSERT with `pg_current_xact_id()`). Rust
  `store::append` and config's row trigger both call it. Module SQL may call
  plane FUNCTIONS (`append_event`, `ensure_history_contract`), never plane
  tables — archcheck tripwire.
- Consumers: `on_tx(SubscriptionSpec { id: "<module>.<topic-kebab>.v1", start },
  …)` — globally unique versioned id, explicit StartPosition (no default).
  Worker: `FOR UPDATE SKIP LOCKED` on the subscription row (replicas = consumer
  group by construction) → handler on the delivery connection → effect +
  checkpoint commit in ONE tx. No inbox, no dedup, exactly-once for
  TransactionalPg. Poison: backoff 1s→5m, pause@20, NEVER auto-skip
  (`eventctl skip --reason` is the audited operator path).
- Retention is checkpoint-coupled and conservative (paused sub blocks GC; topic
  without a `history_contracts` row is never deleted from). Producer owns
  `HistoryPolicy` in `bus::define(topic, version, policy)`.
- NO routing config of any kind: `EVENTS_SUBSCRIBERS`/`EVENTS_ORIGIN`/
  `POST /events`/relay/inbox/`core/outbox` are DELETED and archcheck-banned.
  Monolith and split run identical code; adding a consumer = code in the
  consuming module only.

**Replica-local caches are NOT durable subscriptions** (consumer group ⇒ only
one replica would refresh): `core/invalidation` (second app-owned plane,
LISTEN/NOTIFY broadcast + authoritative-refresh callbacks, first-refresh-or-fail
at start, freshness not delivery). Config/CachedConfig/inventory-starter use it;
config has a monotonic `config.revision` and its trigger emits both NOTIFY and
the durable event.

**Still true (unchanged invariants):** plane is app-owned (`app::run`, DB ⇒
plane, injected at `Context` construction — `set_transport` does not exist);
modules declare NOTHING for it in `requires()`; `core/bus` is sqlx-free
(`AnyTx`/`Delivery` seam; engine only in the transport); plain `emit`/`on` is
in-process best-effort only. Rating is now a persistent projection
(`rating.ratings`) — "restart resets MMR" is no longer true.

**Gotcha:** run `cargo test --workspace` ONE invocation at a time — concurrent
runs deadlock on the plane's migrate advisory lock (looks like a hang; bit two
subagents on 2026-07-10).

See [[never-monolith-only]], [[verify-the-at-risk-path-not-the-safe-one]].

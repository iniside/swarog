---
name: durable-event-plane-bus-owned
description: The durable async transport (outbox/inbox/relay/HTTP) is an APP-OWNED plane (core/asyncevents::Plane, DB ⇒ plane) injected into the bus at Context construction — NOT a module; modules never branch on topology nor declare it in requires(). core/bus is sqlx-free (AnyTx/Delivery seam): the events API names no store engine, handlers get event_id for cross-engine idempotency
metadata: 
  node_type: memory
  type: project
  originSessionId: 9daf9937-49a2-46ca-88f2-a2c9a48ebd40
---

The async dual-path debt (a module carrying BOTH `bus.On` AND a hand-wired
`POST /events/*` HTTP sink, "one path per topology") was RESOLVED 2026-07-07
(plan `docs/plans/2026-07-07-1527-bus-owned-transport-plan.md`, Steps 1-9, verified
live). Then 2026-07-09 the plane was **de-modulized** into an app-owned plane
(plan `docs/plans/2026-07-09-1118-asyncevents-plane-plan.md`, commits `b3a6ce7`,
`584d9eb`). Do NOT re-propose or re-introduce the old per-module outbox/inbox/sink
pattern, and do NOT re-propose messaging-as-a-module or `requires("messaging")` —
the plane is process infrastructure, not a peer module.

**Two planes, chosen by durability intent, never by topology:**
- Best-effort: `bus.Emit` / `bus.On` — in-process fanout, zero DB (unchanged). For
  in-memory/idempotent reactions (rating, `inventory.onConfigChanged`).
- Durable: `bus.EmitTx(tx, ...)` / `bus.OnTx[T](et, subscriber, h)` /
  `bus.OnTxRaw(topic, subscriber, h)` (untyped, for audit-style verbatim logging).
  Exactly-once, transactional, topology-transparent.

**`core/bus` is sqlx-free (AnyTx/Delivery seam, 2026-07-09, plan
`docs/plans/2026-07-09-1422-anytx-seam-plan.md`, commits `7418320`, `790e388`).**
The events API names NO store engine: `emit_tx(AnyTx::new(&mut *tx), et, v)` takes a
type-erased `bus::AnyTx<'_>`; durable handlers receive `bus::Delivery { event_id, tx }`.
The engine lives only in the concrete transport (`asyncevents::enqueue_tx` downcasts
`AnyTx → &mut sqlx::PgConnection`) and in each module's own store layer. **Generalized
contract:** *delivery is at-least-once with a stable `event_id`; effects are
exactly-once iff the dedup-check and the effect are atomic in the consumer's own store
— via the handed delivery tx when engines match, via an idempotent `event_id`-keyed
write otherwise.* A non-Postgres-store consumer ignores `Delivery::tx` and keys an
idempotent write on `event_id` in its own store (inbox stays the redelivery gate). A
foreign-store PRODUCER fails loud with `Error::TxEngineMismatch` at first emit — a
second Transport impl in that engine is the day-two path (backplane stays Postgres).
Don't reintroduce sqlx into `core/bus` or name a store engine in a module's events API.

**`core/asyncevents`** (renamed from `core/messaging`) owns DB schema `asyncevents`
(outbox + per-`(event_id,subscriber)` inbox) and exposes a **`Plane`** — NOT a
`lifecycle::Module`. `core/app::run` owns its lifecycle: it constructs the `Plane`
when the process has a DB, injects its `Transport` into the `Bus` **at `Context`
construction** (`Context::with_db_and_transport`), migrates its schema before module
migrations, starts relay/LISTEN/housekeeping after modules start, and stops delivery
before any module stops. `Bus::set_transport`/`SetTransport` NO LONGER EXISTS — the
transport is a constructor argument (the double-set panic class is gone
structurally). It is the ONLY crate importing `outbox`; the leaf `bus/` still imports
no module (the Transport interface is defined in `bus/`, implemented by asyncevents).
A DB-less process (gateway-svc, admin-svc) hosts no plane; an `on_tx` there fails
loud at init.

**Modules declare NOTHING for it.** No `requires("messaging")`, no handle
acquisition — `requires()` is reserved for domain capabilities from `modules/`.
Modules just call `ctx.bus().emit_tx` / `on_tx` / `on_tx_raw` unchanged.

**Single-owner relay (the load-bearing correctness rule):** every outbox row is stamped
`origin` (env `EVENTS_ORIGIN`, stable per process); a process's relay drains ONLY
`WHERE origin=$self ... FOR UPDATE SKIP LOCKED`, so a foreign-origin relay can never
swallow another process's event. This fixed a BLOCKER the plan review caught. Regression:
`outbox.TestRelayDrainsOnlyOwnOrigin` + `scripts/smoke-split-asyncevents.sh` (evidence:
`docs/2026-07-07-1654-messaging-split-verified.md`).

**When adding a new cross-process event:** declare `bus.Define`, emit via `EmitTx(tx,...)`
in the producer's domain tx, subscribe via `OnTx`; NO `requires()` entry for the plane
(it is app-owned). Every hosting process just needs a DB (⇒ the plane is present) and
`EVENTS_ORIGIN` + `EVENTS_SUBSCRIBERS` (topic=`<peer>/events`) set. No per-topic route,
no hand-written inbox. See [[never-monolith-only]], [[verify-the-at-risk-path-not-the-safe-one]].

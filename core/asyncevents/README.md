# asyncevents — the durable events plane

The async half of this backend's two communication seams: **sync** = registry
capabilities (ask now, get an answer), **async** = durable fire-and-forget events.
This crate is the async half's machinery: an XID-ordered shared event log in
Postgres plus consumer-owned pull subscriptions with transactional checkpoints.
It is owned by `core/app::run` as a *process plane* (DB ⇒ plane), never listed as
a module, never declared in `requires()`. Modules only ever see
`ctx.bus().emit_tx(...)` / `on_tx(spec, ...)`.

**Publisher owns the event. Consumer owns the subscription.** A producer appends
once inside its domain transaction and never knows who consumes; a consumer
declares a durable, versioned `SubscriptionSpec` and pulls at its own pace from
its own checkpoint. There is no per-process routing configuration: adding a
consumer is a code change in the consuming module only, identical in the monolith
and the split.

## How broker backends actually work (the part that's easy to misremember)

A broker is a separate stateful process: its own log, offsets, retention,
fan-out, wire protocol. But **every service that talks to it still compiles in a
client library** (`kafka-clients`, `rdkafka`, `nats.rs`, `lapin`) plus its HTTP
framework, metrics, middleware, serialization. So the design question is never
*"code or service"* — both worlds compile shared code into every process. The
question is **which capabilities arrive as code (linked in) and which as a
protocol (dialed)**:

- **Must share your process / transaction / memory → library.** Broker clients,
  middleware, metric registries — and, here, the `emit_tx` transport, because it
  rides the caller's open Postgres transaction. An open transaction cannot cross
  a wire; that is the dual-write problem, and it is why *even Kafka shops* keep a
  transactional-outbox library compiled into every exactly-once producer.
- **Own state + lifecycle that many processes coordinate through → service.**
  Databases, brokers, caches.

## Where this backend sits: the broker-as-a-service already exists — it is Postgres

| | Kafka-style stack | this stack |
|---|---|---|
| Stateful service (log, offsets, retention, durability) | broker cluster | **Postgres** (`asyncevents.events` + `subscriptions`, LISTEN/NOTIFY, WAL) |
| Compiled into every service | broker client + framework + outbox lib | `core/*` + this crate |
| Delivery model | consumers pull from offsets | consumers pull from checkpoints (this crate's worker) |
| Consumer group | broker-coordinated | `FOR UPDATE SKIP LOCKED` on the subscription row |

Unlike a broker, the log lives in the SAME Postgres as the domain data, which
buys the one thing a broker structurally cannot offer: **the domain effect and
the consumer checkpoint commit in one transaction.** There is no inbox, no dedup
table, no idempotency reasoning for a `TransactionalPg` consumer — the handler
runs on the delivery connection, and either both the effect and the cursor
advance commit, or neither does.

The guarantee, stated generally: *delivery is at-least-once per subscription with
a stable `event_id`; effects are exactly-once for a consumer whose effect commits
in the handed delivery transaction; ordering is per-subscription in XID-allocation
order.* A foreign-store consumer ignores the handed tx and keys an idempotent
write on `event_id` — supported by the types, not used by any current module.

## The correctness model (why the log is not just a bigserial table)

`bigserial` is allocated at INSERT but becomes visible at COMMIT, in a different
order — a naive cursor skips rows that commit late, forever. A V2 position is
`(generation, producer_xid, tie_breaker)`:

- `producer_xid` = `pg_current_xact_id()` of the producing top-level transaction;
  readers only observe current-generation rows with
  `producer_xid < pg_snapshot_xmin(pg_current_snapshot())` — the frontier. A slow
  commit delays the frontier (alarmed via the safe-frontier-age metric); it can
  never be skipped.
- `generation` fences cluster identity: after a restore/PITR onto a new cluster,
  XIDs stop being monotonic, so an operator bumps the generation
  (`eventctl bump-generation`) and every older generation becomes fully eligible
  while the new one starts a fresh XID ordering. `plane_meta` pins the cluster's
  `system_identifier`; startup guards refuse a changed cluster without a bump
  (plus `max_prepared_transactions = 0` and an empty `pg_prepared_xacts` —
  a prepared transaction would sit outside the snapshot indefinitely).
- `tie_breaker` orders events written by the same transaction.

**Exactly one writer implementation exists:** the SQL function
`asyncevents.append_event(topic, version, payload)` — transaction-scoped
**shared** advisory lock on one fixed key → read `plane_meta.generation` → INSERT
stamped with `pg_current_xact_id()`. The Rust producer (`store::append`, behind
`Transport::enqueue_tx`) calls it; module-owned SQL (config's trigger) calls it;
`store::bump_generation` takes the **exclusive** form of the same lock and waits
every in-flight writer out. Module SQL may call the plane's functions
(`append_event`, `ensure_history_contract`) but never touch plane tables —
archcheck enforces this.

sqlx has no `xid8` codec: every bind/decode crosses as text (`$n::xid8` in,
`producer_xid::text` out) and comparisons stay in SQL.

## Delivery (the worker)

Per process, one worker pool over the subscriptions its modules registered
(`catalog.rs` reconciles `SubscriptionSpec`s into `asyncevents.subscriptions` at
start; an existing row with a different immutable `spec_hash` fails startup; the
cursor is materialized from `StartPosition` at creation and is never NULL).
One delivery: pick a due subscription `FOR UPDATE SKIP LOCKED` (replicas form a
consumer group by construction) → frontier-bounded next-event select → SAVEPOINT
→ handler on the same connection → cursor advance → COMMIT.

Failure state machine: handler **error** → rollback to the savepoint, record
exponential backoff (1s → 5m), pause after 20 consecutive failures — **no
automatic skip, ever** (skipping is an audited `eventctl skip --reason` operator
action). Handler **timeout** (`ASYNCEVENTS_HANDLER_TIMEOUT`, default 10s) → the
connection has an in-flight statement and is unusable: drop it, terminate the
wedged backend (releasing the row lock), record backoff on a fresh connection.
Workers never sleep holding the row lock. Wake-up: one `PgListener` on
`asyncevents_events` + a 1s poll floor. A dead worker fails `/readyz`.

Replica-local caches are **not** durable subscriptions (under consumer-group
semantics only one replica would refresh): they use `core/invalidation`
(LISTEN/NOTIFY broadcast + authoritative refresh), which promises freshness, not
delivery.

## Retention

Coupled to checkpoints, conservative by construction: per topic, the GC floor is
the MIN cursor over active **and paused** subscriptions (a paused consumer blocks
GC and raises `asyncevents_retention_blocked_age_seconds`); rows below the floor
are deleted only past the topic's `MinRetention` days; `KeepForever` never
deletes; **a topic with no `history_contracts` row is never deleted from**.
`history_contracts` is seeded by the emitting transport, by typed-subscription
reconcile, and — for SQL producers — by the producer's migrate calling
`asyncevents.ensure_history_contract(...)`.

Operator surface: `tools/eventctl` — `list`, `lag`, `retry`, `pause`, `resume`,
`skip <id> --reason`, `retire`, `bump-generation`. No command advances a
checkpoint silently.

## What adopting a real broker would change (and what it wouldn't)

A broker earns its place only past this project's stated boundary (one shared
Postgres, moderate throughput): separate per-service databases, thousands of
partitions, multi-region. The change is local to this crate — the log and worker
move behind the broker's client — while `emit_tx`, the event contracts, and every
`SubscriptionSpec` stay exactly as they are. Modules never knew what delivery
looks like, which is the point of the plane. Deliberate scale choices at the
current boundary: one event per delivery transaction (no batching knob), one
checkpoint per subscription (no partition parallelism, no `partition_key`
column), generation bumps are offline operator actions.

## First-migrate prerequisite (one-time superuser bootstrap)

The migrate seeds `plane_meta` with the cluster's
`pg_control_system().system_identifier`; the function is not executable by
ordinary roles by default:

```
GRANT EXECUTE ON FUNCTION pg_control_system() TO gamebackend;
```

## Mechanics (quick reference)

- **Lifecycle** (all driven by `app::run`, in this order): `Plane::new(pool, dsn)`
  → transport injected at `Context` construction (so every module's wiring-time
  `on_tx` finds it) → `migrate()` before any module migrate (DDL under an
  exclusive migrate advisory lock; drops the legacy push-era `outbox`/`inbox` if
  present) → `start()` after module starts (subscription reconcile, worker pool,
  LISTEN wake-up, retention) → `stop()` before any module stops (delivery halts
  first).
- **Schema** `asyncevents`: `events` (the log; PK `(generation, producer_xid,
  tie_breaker)`, unique `event_id`, `AFTER INSERT` → `pg_notify
  ('asyncevents_events')`), `subscriptions` (checkpoint + failure state),
  `plane_meta` (generation + cluster identity singleton), `history_contracts`
  (per-topic retention policy).
- **Env:** `ASYNCEVENTS_HANDLER_TIMEOUT` (default `10s`),
  `EVENTS_HOUSEKEEP_INTERVAL` (default `1h`).
- **Tests:** `asyncevents::testing` — `events_count`/`cleanup_events` +
  `transport(pool)` returning a `TestTransport` whose `deliver_all()` runs a real
  reconcile-and-drain worker pass, so module tests exercise emit → deliver
  round-trips without booting a process. Run `cargo test --workspace` ONE
  invocation at a time — concurrent runs contend on the migrate advisory lock.

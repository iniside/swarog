# asyncevents — the durable events plane

The async half of this backend's two communication seams: **sync** = registry
capabilities (ask now, get an answer), **async** = durable fire-and-forget events.
This crate is the async half's machinery: transactional outbox, relay,
`POST /events` wire, inbox dedup. It is owned by `core/app::run` as a *process
plane* (DB ⇒ plane), never listed as a module, never declared in `requires()`.
Modules only ever see `ctx.bus().emit_tx(...)` / `on_tx(...)`.

This README answers the question every new reader asks: *"backends normally run
NATS/Kafka/RabbitMQ as a service and each service has a client to it — why is
this compiled into every process instead?"*

## How broker backends actually work (the part that's easy to misremember)

A broker is a separate stateful process: its own log, offsets, retention,
fan-out, wire protocol. But **every service that talks to it still compiles in a
client library** (`kafka-clients`, `rdkafka`, `nats.rs`, `lapin`) plus its HTTP
framework, metrics, middleware, serialization. A "microservice" binary is ~90%
shared infrastructure code. So the design question is never *"code or service"*
— both worlds compile shared code into every process. The question is **which
capabilities arrive as code (linked in) and which as a protocol (dialed)**, and
the rule is physical, not aesthetic:

- **Must share your process / transaction / memory → library.** Broker clients,
  middleware, metric registries — and, here, the `emit_tx` transport, because it
  rides the caller's open Postgres transaction. An open transaction cannot cross
  a wire; that is the dual-write problem, and it is why *even Kafka shops* keep a
  transactional-outbox library compiled into every exactly-once producer.
- **Own state + lifecycle that many processes coordinate through → service.**
  Databases, brokers, caches.

## Where this backend sits: the broker-as-a-service already exists — it is Postgres

The stateful half of a broker is here, and it is a separate service every process
already dials:

| | Kafka-style stack | this stack |
|---|---|---|
| Stateful service (log, coordination, durability) | broker cluster | **Postgres** (`asyncevents.outbox`, `FOR UPDATE SKIP LOCKED`, LISTEN/NOTIFY, WAL) |
| Compiled into every service | broker client + framework + outbox lib | `core/*` + this crate |
| Who pushes delivery | broker (consumers pull) | the **relay** in each producer process (POSTs to subscribers) |

The only piece that is code here but a service there is the *delivery loop* (the
relay). And a broker would not remove the rest: the producer-side outbox and the
consumer-side inbox dedup stay in-process in both worlds, because exactly-once
semantics live exactly at the transaction boundary the broker cannot enter.

`emit_tx` is "send to the broker": the broker's ingest is a SQL `INSERT`, which —
unlike a network POST — can join the producer's domain transaction, because the
broker's storage lives in the same Postgres as the domain data. Everything after
commit (relay → `POST /events` → inbox) is at-least-once delivery with
per-subscriber dedup, i.e. exactly-once effects.

## Why `core/*` compiles into every process

1. **Some seams are in-process by definition.** `registry` is a hashmap of
   `Arc<dyn Trait>`; `lifecycle` is call ordering inside your process; the
   in-process bus is tokio channels. "Registry over the network" is not a thing,
   the same way `std` is not a service.
2. **The rest is the process's membrane.** `edge` (your QUIC listener), `httpmw`
   (your middleware), `metrics` (your scrape registry on your port), this crate
   (your broker client + your transactionality). Every process needs its own —
   exactly like every Java microservice carries its own Netty and its own Kafka
   client. A remote "metrics service" you call per-request is the known
   anti-pattern; Prometheus scrapes each process for the same reason.
3. **Monorepo + workspace turns the cost into a feature.** The classic pain of
   compiled-in infrastructure is version skew across a polyrepo fleet (five
   `kafka-clients` versions, a quarter-long migration). Here `core` has exactly
   one version, upgrades are atomic in one commit, and cargo + archcheck police
   the dependency edges. The price is recompiling the workspace when core
   changes — seconds at this scale.

## What adopting a real broker would change (and what it wouldn't)

A dedicated broker earns its place only for semantics deliberately not offered
here: replay/history, late consumers reading from an offset, cross-partition
ordering, throughput beyond relay-HTTP. If that day comes, the change is local
to this crate: the relay POSTs to the broker instead of to peers, and consumers
gain a consume loop instead of the `POST /events` sink. The outbox, the inbox,
and the module-facing API (`emit_tx`/`on_tx`) stay exactly as they are — modules
never knew what delivery looks like, which is the point of the plane.

## Mechanics (quick reference)

- **Lifecycle** (all driven by `app::run`, in this order): `Plane::new(pool, dsn)`
  → transport injected at `Context` construction (so every module's wiring-time
  `on_tx` finds it) → `router()` mounted (`POST /events`) → `migrate()` before
  any module migrate → `start()` after module starts (origin-collision guard,
  local-target snapshot, relay + LISTEN + housekeeping) → `stop()` before any
  module stops (delivery halts first).
- **Schema** `asyncevents`: `outbox` (origin-stamped log; `AFTER INSERT` trigger
  fires `pg_notify('asyncevents_outbox')`), `inbox` (PK `(event_id, subscriber)`
  — per-subscriber exactly-once). The DDL includes a guarded
  `ALTER SCHEMA messaging RENAME TO asyncevents` + inbox dedup-prefix rewrite
  for databases created before the 2026-07-09 rename.
- **Single-owner drain:** each relay drains ONLY its own `EVENTS_ORIGIN`'s rows
  (`FOR UPDATE SKIP LOCKED`), so many processes share one table without stealing
  each other's deliveries. The generic drain/deliver engine lives in
  [`core/outbox`](../outbox/); this crate owns the schema, the `bus::Transport`
  impl, the inbound sink, the LISTEN wakeup, and retention pruning.
- **Env:** `EVENTS_ORIGIN` (distinct per process in the split; default
  `monolith`), `EVENTS_SUBSCRIBERS` (`topic=url,url2;topic2=url` — remote peer
  `/events` sinks), `EVENTS_RETENTION` (default `168h`),
  `EVENTS_HOUSEKEEP_INTERVAL` (default `1h`).
- **Tests:** `asyncevents::transport(pool, origin)` gives module integration
  tests a bare transport without booting the plane.

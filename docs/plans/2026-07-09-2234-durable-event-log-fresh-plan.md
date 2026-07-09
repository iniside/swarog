# Durable event log: pull subscriptions, fresh-start edition

**Date:** 2026-07-09 22:34
**Status:** reviewed — grumpy-review punch list (16 items, 2 blockers) addressed in this revision
**Supersedes:** [`2026-07-09-2202-durable-event-log-plan.md`](2026-07-09-2202-durable-event-log-plan.md)
(same target architecture, migration machinery removed) and the push/outbox/HTTP/inbox
delivery of [`2026-07-09-1118-asyncevents-plane-plan.md`](2026-07-09-1118-asyncevents-plane-plan.md).

## Context and the one decision that reshaped this plan

The 2202 plan's target architecture (XID-ordered shared Postgres event log,
consumer-owned pull subscriptions with transactional checkpoints) survives intact.
Its Steps 11–17 — legacy delivery gates, claim ranges, mirror triggers, protocol
epochs, consumer-rollback state machine, cleanup manifest — were a zero-downtime
rolling-fleet migration protocol. **User decision (2026-07-09): this project is
local; wiping the database and starting fresh is 100% acceptable.** Therefore there
is no migration. We build the target state, drop the old `asyncevents` tables, and
boot. Deleted relative to 2202: `legacy_topic_map`, `legacy_claim_ranges`,
`legacy_delivery_gates`, `migration_boundaries`, `cleanup_manifest`,
`process_leases`, `subscription_hosts`, `plane_meta.{legacy_gc_frozen,
consumer_rollback, config_emission_epoch}`, the `asyncevents:<outbox_id>` event-id
format, `StartPosition::ProjectionSnapshot`, `ArchiveFromEpoch`, the "Rollback
boundaries" protocol, and the entire per-subscription cutover choreography.
Multi-host safety no longer needs lease tables: `FOR UPDATE SKIP LOCKED` on the
subscription row makes concurrent hosts *safe*; the static one-host-per-profile
check (Step 11) makes accidental double-hosting *visible*.

**Why not extend the existing plane:** the current model
(`core/asyncevents::Plane` + `core/outbox::Relay`, verified 2026-07-09 by three
research subagents) is producer-push: the producer's process must know every
consumer's URL (`EVENTS_SUBSCRIBERS`), each new module edits every producing
process's env graph in `run.*`/`split-proof.*`, and delivery is
outbox → HTTP `POST /events` → per-process inbox dedup. That is O(producers ×
consumers) configuration and an unauthenticated HTTP sink. The replacement inverts
ownership — the consumer declares its subscription; the producer appends once —
which is new plane code, not an extension of the relay.

### Research basis (verified against source, not the 2202 plan's claims)

- `core/bus/src/lib.rs`: `EventType<T>` holds a bare topic string; `define(topic)`;
  `Transport { enqueue_tx(tx, topic, payload), subscribe_tx(topic, subscriber,
  handler) }`; `TxHandler`; `Delivery { event_id: &str, tx: AnyTx }`;
  `Bus::subscribed_topics()` feeds topiccheck. Topics are unversioned.
- Contract crates: `accountsevents` (player.registered), `charactersevents`
  (character.created/deleted), `configevents` (config.changed), `matchevents`
  (match.finished), `schedulerevents` (scheduler.fired). All
  `LazyLock<EventType<T>>` + serde payload struct.
- Producers (`emit_tx` in a real domain tx): accounts (lib.rs:174), characters
  (219, 262), match (109), scheduler (179, under advisory lock). config emits from
  its NOTIFY-listener path in a purpose-opened tx (lib.rs:271) — deliberately, to
  catch psql writes.
- Consumers: inventory (`"inventory"` × created/deleted/config.changed; the
  config.changed handler only rebuilds an in-memory `Starter` cache), leaderboard
  (`"leaderboard"` × match.finished, DB upsert), rating (`"rating"` ×
  match.finished, **in-memory only**), audit (`"audit"` shared across 5 raw topics
  + scheduler.fired). `configrpc`'s remote `Stub` registers a durable
  `"config-cache"` subscriber in `register` phase to refresh `CachedConfig`.
- Plane: `Plane::new` is the only `EVENTS_ORIGIN`/`EVENTS_SUBSCRIBERS` read site;
  `Plane::router()` mounts `POST /events` (no auth) via `app::run`;
  `outbox`/`inbox` tables + `asyncevents_outbox` NOTIFY trigger; relay drains
  `FOR UPDATE SKIP LOCKED` ordered by id, per-(topic,target) blocking, `sent_at`
  marks. `core/outbox` is the generic relay lib underneath.
- `app::run` ordering: DB ⇒ plane; transport injected at `Context` construction;
  plane migrate → module migrate → module start → plane start; plane stop before
  module stop.
- config module hand-rolls a `PgListener` on channel `config_changed` (trigger
  covers INSERT/UPDATE only — **not DELETE**), with reconnect replay-and-heal.
- Checkers run real code: `topiccheck` boots `checkmodules::monolith_modules()`
  with a `RecordingTransport`; `defined_topics()` is a hand list of the 6 statics;
  `checkmodules` manually mirrors `cmd/server`'s list. `cmd/*` are `main.rs`-only.
- split-proof event assertions: `[AU1-3]` audit rows, `[SC0-1]` scheduler fired via
  `outbox.sent_at`, `[MT1-4]` leaderboard/audit, `[C0-3]` config live-reload,
  starter-grant/wipe DB asserts. `scripts/smoke-split-asyncevents.sh` asserts
  outbox origin/`sent_at` and inbox `subscriber` rows directly — full rewrite needed.

## Correctness model (unchanged from 2202 where it was right)

**Position & visibility.** `bigserial` is never a cursor. A position is
`(generation bigint, producer_xid xid8, tie_breaker bigint)`; `producer_xid` =
`pg_current_xact_id()` of the producing top-level transaction; `tie_breaker`
orders events within one transaction. A reader may only observe rows of the
current generation satisfying `producer_xid < pg_snapshot_xmin(pg_current_snapshot())`;
rows of completed older generations are all eligible. A long-running transaction
delays the frontier (alarmed via metric), never causes a skip. Every writer —
native Rust and the config SQL trigger — takes the transaction-scoped **shared**
advisory lock on one fixed key, then reads `plane_meta.generation`, then inserts.
A generation bump (offline event, e.g. after restoring onto a new cluster) takes
the **exclusive** form, waits out shared holders, and updates generation +
`system_identifier` atomically. Startup fails on: changed
`pg_control_system().system_identifier`, `max_prepared_transactions != 0`, or any
row in `pg_prepared_xacts` (a prepared event-producing tx would sit outside the
snapshot indefinitely).

**Delivery.** One durable handler class: `TransactionalPg` — worker selects one due
subscription `FOR UPDATE SKIP LOCKED`, computes the frontier, selects one next
event, runs the handler on the same connection under a savepoint, advances the
cursor, commits effect + checkpoint atomically. On error: roll back to the
savepoint, record `consecutive_failures`/`last_error`/`next_attempt_at`
(exponential backoff 1s → 5m), commit; after 20 consecutive failures the
subscription pauses. No automatic skip, ever. Replicas of one service share the
subscription row and form a consumer group by construction. Replica-local caches
(CachedConfig, inventory starter) are **not** durable subscriptions — under
consumer-group semantics only one replica would ever refresh; they move to the
broadcast invalidation plane (Step 6). External-system effects (none exist today)
would be a module-owned command outbox, not a worker concern — documented, not built.

**Cursor discipline.** The cursor is NOT NULL from the moment a subscription row
is created: `StartPosition` is materialized into an initial cursor at reconcile
time — `Genesis` → `(0, xid8 '0', 0)` (sorts before every real position, whose
generation is ≥ 1); `AfterRegistration` → `(current_generation,
pg_current_xact_id(), i64::MAX)` (excludes every event of the registering
transaction, exclusive floor by construction); `Explicit(p)` → `p`. There are no
separate `floor_*` columns — the initial cursor IS the floor. The worker's
next-event select filters `contract_version = subscription.contract_version`
exactly; events of the same topic at another version are passed over (the cursor
is the position of the last *delivered* event, so passed-over rows stay retained
until this subscription's cursor moves beyond them — conservative and correct).

**History & start.** Publisher declares `HistoryPolicy::MinRetention{days}` (all
six topics start at 7 days) or `KeepForever` (none today; required before any
future replay-from-genesis consumer). Consumer declares `StartPosition` with no
default: `Genesis` | `AfterRegistration` | `Explicit(position)`. GC floor per topic =
min(effective position of every active **and paused** subscription); a paused
subscription blocks retention and raises an age alarm. Retirement is an explicit
`eventctl` operation; deleting code is not retirement (readiness reports orphaned
DB rows).

## Subscription matrix (fresh DB, all start `Genesis`)

| Topic (v1) | Producer | Durable subscriptions |
|---|---|---|
| `player.registered` | accounts | `audit.player-registered.v1` |
| `character.created` | characters | `inventory.character-created.v1`, `audit.character-created.v1` |
| `character.deleted` | characters | `inventory.character-deleted.v1`, `audit.character-deleted.v1` |
| `match.finished` | match | `rating.match-finished.v1`, `leaderboard.match-finished.v1`, `audit.match-finished.v1` |
| `config.changed` | config (SQL trigger after Step 7) | `audit.config-changed.v1` |
| `scheduler.fired` | scheduler | `audit.prune-on-scheduler.v1` |

Removed outright: inventory's `config.changed` subscription and `configrpc`'s
`"config-cache"` subscriber (both become invalidation-plane callbacks); audit's
single shared `"audit"` name (now six independent checkpoints).

## Target schema (`asyncevents`, created by `Plane::migrate`)

```sql
-- Step 2 creates these ALONGSIDE the legacy outbox/inbox; the Step 3 cutover
-- migrate then drops the legacy tables + notify trigger (wipe-acceptable decision).
plane_meta(singleton bool PRIMARY KEY CHECK (singleton),
           generation bigint NOT NULL,
           system_identifier numeric NOT NULL);
events(generation bigint NOT NULL,
       producer_xid xid8 NOT NULL,
       tie_breaker bigint GENERATED ALWAYS AS IDENTITY,
       event_id text NOT NULL UNIQUE DEFAULT gen_random_uuid()::text,
       topic text NOT NULL,
       contract_version integer NOT NULL CHECK (contract_version > 0),
       payload jsonb NOT NULL,
       created_at timestamptz NOT NULL DEFAULT now(),
       PRIMARY KEY (generation, producer_xid, tie_breaker));
CREATE INDEX events_scan ON events (topic, generation, producer_xid, tie_breaker);
subscriptions(subscription_id text PRIMARY KEY,
              topic text NOT NULL,
              contract_version integer NOT NULL,
              state text NOT NULL CHECK (state IN ('active','paused','retired')),
              -- NOT NULL from creation: StartPosition materialized at reconcile
              cursor_generation bigint NOT NULL,
              cursor_xid xid8 NOT NULL,
              cursor_tie bigint NOT NULL,
              next_attempt_at timestamptz,
              consecutive_failures integer NOT NULL DEFAULT 0,
              last_error text,
              spec_hash text NOT NULL,
              start_kind text NOT NULL,
              updated_at timestamptz NOT NULL);
history_contracts(topic text NOT NULL,
                  contract_version integer NOT NULL,
                  policy text NOT NULL CHECK (policy IN ('min_retention','keep_forever')),
                  min_retention_days integer NOT NULL DEFAULT 7,
                  PRIMARY KEY (topic, contract_version));
```

The migration also installs `asyncevents.append_event(topic text, version int,
payload jsonb) RETURNS text` — an SQL function owning the whole writer protocol
(shared advisory lock → read `plane_meta.generation` → insert with
`pg_current_xact_id()`, returning the `event_id`). The native Rust writer calls
this function too, so there is exactly ONE writer implementation; module-owned SQL
(config's trigger, Step 7) may call the function but never touches plane tables
(archcheck-enforced, Step 11). `Plane::migrate` seeds the `plane_meta` singleton
(`generation = 1`, current `pg_control_system().system_identifier`) with
`ON CONFLICT DO NOTHING` under the global migration advisory lock; the
`GRANT EXECUTE ON FUNCTION pg_control_system() TO gamebackend` superuser bootstrap
is therefore a prerequisite of the FIRST migrate, not just the startup guard.

**sqlx has no `xid8` codec.** Every bind/decode crosses the boundary as text:
`$n::xid8` on the way in, `producer_xid::text` on the way out; `EventPosition.xid`
is a `u64` parsed from that text; comparisons happen in SQL (row comparison), never
in Rust. A round-trip unit test pins this convention (Step 2).

Wake-up: `AFTER INSERT ON events` trigger → `pg_notify('asyncevents_events', topic)`;
one `PgListener` per process wakes the worker pool; a single global 1s poll is the
lost-NOTIFY fallback. No `partition_key` column until checkpoint-per-partition
semantics exist (one subscription = one serial stream, a deliberate ordering choice).

## Steps

Commit per step, and every commit leaves `cargo build/test --workspace` green,
with ONE declared exception: **Steps 3 and 4 are a single commit** (the cutover
commit) — swapping the delivery mechanism and rewriting the split-proof/smoke
assertions that observe it cannot be separated without a red BLOCKING verify
stage in between. Step 1 ships a shim so the old push plane keeps delivering
until that cutover; Step 2 is purely additive.

The `public-api` verify stage (advisory; additive-only diff of contract crates vs
HEAD) goes red at Step 1 — `define()`'s signature and later config's payload
(Step 7) are deliberate one-time breaks of the fresh-world reset. Acknowledged
here once; do not "fix" it mid-rollout, it self-heals as commits become HEAD.

### Step 1 — versioned contracts and subscription descriptors in `core/bus` `[fable]`

**What:** `core/bus/src/lib.rs` + `tests.rs`; all five `api/*/events` crates;
every `on_tx`/`on_tx_raw` registration in `modules/{inventory,leaderboard,rating,
audit}`; the `"config-cache"` registration in `api/config/rpc`;
`core/asyncevents/src/lib.rs` (signature shim only); `tools/topiccheck` (compile fix).

**Why now:** every later step compiles against this vocabulary; changing it later
would ripple through storage, worker, checkers and scripts.

**How:** add `EventContract { topic: &'static str, version: u32, history:
HistoryPolicy }`, `HistoryPolicy::{MinRetention{days: u32}, KeepForever}`,
`SubscriptionSpec { id: &'static str, start: StartPosition }`,
`StartPosition::{Genesis, AfterRegistration, Explicit(EventPosition)}`,
`EventPosition { generation: i64, xid: u64, tie: i64 }`. `define<T>(topic, version,
history)` builds `EventType<T>` carrying the contract; update all five events
crates (`define("character.created", 1, HistoryPolicy::MinRetention{days: 7})` …).
Change `Bus::on_tx(spec: SubscriptionSpec, et: &EventType<T>, handler)` and
`on_tx_raw(spec, topic, handler)`; `Transport::subscribe_tx(&self, spec:
SubscriptionSpec, topic: &str, version: u32, handler)`; `Transport::enqueue_tx`
gains `contract: &EventContract` instead of bare `topic`. Bus panics on duplicate
`spec.id` at registration. Rewrite module registrations per the matrix (audit's
loop zips `DURABLE_TOPICS` with per-topic spec IDs). Extend `subscribed_topics()`
with `subscriptions() -> Vec<(String, String)>` (id, topic) for topiccheck.
Duplicate-`spec.id` detection lives in `Bus` (a `Mutex<HashSet<&'static str>>`
checked before forwarding to the transport) so it holds under ANY transport,
including topiccheck's `RecordingTransport`. Shim: the existing
`Inner::subscribe_tx` maps `spec.id` → old subscriber string so the push plane
still delivers (inbox dedup keys change; irrelevant, DB gets wiped). `AnyTx`,
`Delivery`, `TxHandler`, plain `emit`/`on` unchanged.

### Step 2 — commit-safe append protocol and V2 storage (additive) `[fable]`

**What:** `core/asyncevents/src/{lib,store,producer}.rs` (split the single file),
`src/tests.rs`. **Purely additive:** the legacy `outbox`/`inbox` tables, the
relay, and the old `enqueue_tx` path remain the live delivery mechanism — the
whole workspace stays green with old behavior.

**Why now:** worker (Step 3) and the config SQL trigger (Step 7) must share one
proven writer/position semantics before anything cuts over.

**How:** `Plane::migrate` additionally creates the target schema above (legacy
tables untouched until Step 3), seeds `plane_meta` as specified, and installs
`asyncevents.append_event(...)` — the single writer implementation. Add the
internal Rust producer (`store::append(conn, contract, payload)` calling the SQL
function) plus the xid8-as-text codec convention with its round-trip test.
`Transport::enqueue_tx` still writes the OLD outbox. Add generation-bump code
(`eventctl` wires it in Step 5): exclusive advisory lock, update generation +
`system_identifier`. Startup guards: `pg_control_system()` identity match (fail
with the exact `GRANT EXECUTE ON FUNCTION pg_control_system() TO gamebackend`
message — a documented one-time superuser bootstrap; superuser creds are in local
agent memory; required already at first migrate), `max_prepared_transactions = 0`,
empty `pg_prepared_xacts`. Live-Postgres tests (house `test_pool()` pattern): two
controlled transactions prove XID-inversion safety (earlier xid commits later, is
not skippable once frontier passes); `tie_breaker` orders same-tx events;
exclusive bump waits for an in-flight shared writer; identity mismatch fails
startup.

### Step 3 — pull worker, failure state machine, delivery cutover `[fable]`

*(Steps 3+4 are one commit — see the red-window declaration above.)*

**What:** `core/asyncevents/src/{catalog,worker,wakeup}.rs` + tests;
`Plane::{new,migrate,start,stop,transport}`; delete `Plane::router`/
`handle_inbound`, the relay/`LISTEN asyncevents_outbox`/inbox `consume` paths and
the `EVENTS_ORIGIN`/`EVENTS_SUBSCRIBERS` reads; `Plane::migrate` now also drops
the legacy `outbox`/`inbox` tables + trigger; `core/app/src/lib.rs` (drop
`ctx.mount(p.router())`, add worker readiness); **the `asyncevents::testing`
helpers and every module test that uses them** — `testing::{outbox_count,
cleanup_outbox}` and the `transport(pool, origin)` test helper are replaced by V2
equivalents (`events_count(topic)`, `cleanup_events`, `transport(pool)` backed by
a test worker), with the call sites in `modules/{accounts,characters,config,
inventory,match,leaderboard,rating,audit}/src/tests.rs` swept in the same change;
`core/outbox` becomes unused (crate deleted in Step 12).

**Why now:** storage and contracts exist; with no migration constraint the cutover
is a single step — after it, delivery is pull-only in both topologies.

**How:** `Transport::enqueue_tx` switches to `store::append` (Step 2's writer).
At `Plane::start`, reconcile registered `SubscriptionSpec`s into `subscriptions`:
insert missing rows with the cursor **materialized from `StartPosition` at
creation** (see Cursor discipline — never NULL); an existing row with a different
immutable `spec_hash` (hash of topic+version+start) fails startup. Worker loop
per process: `SELECT … FROM asyncevents.subscriptions WHERE subscription_id =
ANY($local) AND state='active' AND (next_attempt_at IS NULL OR next_attempt_at <=
now()) FOR UPDATE SKIP LOCKED LIMIT 1`; frontier-bounded next-event select
(row-compare `(generation, producer_xid, tie_breaker)` > cursor, exact
`contract_version` match, current-generation rows gated by `producer_xid <
pg_snapshot_xmin(pg_current_snapshot())`); `SAVEPOINT`; invoke the `TxHandler`
with `Delivery { event_id, tx }` on the same connection (existing handlers
downcast to `PgConnection` unchanged); advance cursor; commit. Handler **error**:
`ROLLBACK TO SAVEPOINT`, write backoff/pause state, commit immediately. Handler
**timeout** (10s default, env `ASYNCEVENTS_HANDLER_TIMEOUT`): the delivery
connection has an in-flight statement and is unusable — close/drop it (server
aborts the transaction, releasing the row lock), then record backoff state on a
fresh pool connection; never attempt savepoint rollback on a timed-out
connection. Workers never sleep holding the row lock. Wake-up: `PgListener` on
`asyncevents_events` + global 1s poll; drain a subscription until it has no
eligible events before yielding. Metrics: per-subscription lag (events + age),
safe-frontier age, consecutive failures, paused count. Readiness
(`httpmw::ReadyCheck`): worker task alive; a panicked worker fails `/readyz`.
In-memory rating keeps its handler as-is until Step 9 (same honesty level as
today). Integration tests: two workers on one subscription (SKIP LOCKED single
owner, failover resumes from checkpoint), crash-before/after-commit idempotence,
poison → backoff → pause → no skip, timeout poisons only the one delivery
connection, NOTIFY loss covered by poll.

### Step 4 — rewrite topology scripts and split-proof assertions `[opus]`

*(Lands in the same commit as Step 3.)*

**What:** `run.sh`, `run.ps1`, `split-proof.sh`, `split-proof.ps1`,
`scripts/smoke-split-asyncevents.sh`; the `EVENTS_*`/outbox/relay/`POST /events`
doc headers in the seven cmd mains (`cmd/{accounts,scheduler,config,rating,match,
audit,apikeys}-svc/src/main.rs`) rewritten to describe pull delivery.

**Why now:** the split-proof BLOCKING stage observes the delivery mechanism
Step 3 just replaced; the cutover commit is complete only with these rewritten.

**How:** delete every `EVENTS_ORIGIN`/`EVENTS_SUBSCRIBERS` assignment and comment.
Replace `[SC1]`'s `sent_at` poll with: poll `asyncevents.subscriptions` until
`audit.prune-on-scheduler.v1` cursor is non-null and advancing, plus the existing
`audit.log` row poll. Keep `[AU*]`, `[MT*]`, `[C0-3]`, starter-grant/wipe
end-to-end assertions as-is (they assert domain effects, which survive). Rewrite
`smoke-split-asyncevents.sh`: create character on A → poll inventory grant on B;
assert `asyncevents.events` row exists for `character.created`; assert both
`inventory.character-created.v1` and `audit.character-created.v1` cursors ≥ that
row's position (replaces the BLOCKER-1 origin/inbox assertions — foreign-relay
swallowing is structurally impossible in pull). Monolith parity re-run unchanged
in shape. Keep the psql-missing graceful-skip pattern.

### Step 5 — retention, retirement, `tools/eventctl` `[opus]`

**What:** `core/asyncevents/src/retention.rs` + tests; new workspace crate
`tools/eventctl`; workspace `Cargo.toml`.

**Why now:** the log now grows unboundedly; retention must be checkpoint-coupled
before anyone relies on V2 in anger, and pause/skip need an operator surface.

**How:** `history_contracts` is populated by two paths and read by GC: (a) the
native writer upserts its `EventContract` once per process (a `OnceLock`-guarded
`INSERT … ON CONFLICT (topic, contract_version) DO NOTHING` on first emit; a
conflicting existing row with a DIFFERENT policy fails the emit loudly), and (b)
typed subscription reconciliation (Step 3) upserts the contract carried by each
`EventType` (raw `on_tx_raw` subs carry no contract — producers own the row).
**GC is conservative: a topic with no `history_contracts` row is never deleted
from.** Housekeeping task (reuse `EVENTS_HOUSEKEEP_INTERVAL`, default 1h; drop
`EVENTS_RETENTION` in favor of per-topic policy): per topic, floor =
min(cursor over active+paused subscriptions of that topic; a never-run `Genesis`
subscription pins everything via its `(0,0,0)` cursor); delete only rows below
the floor AND older than `MinRetention` days — hourly bounded-batch deletes
(`ctid IN (… LIMIT 1000)`, today's prune pattern); no `created_at` index, the
seq scan is accepted at this scale and noted in the README. `KeepForever`
topics never delete. Paused subscription blocking GC raises
`asyncevents_retention_blocked_age_seconds`. `eventctl` (sqlx CLI, same
`DATABASE_URL`): `list`, `lag`, `retry <id>`, `pause <id>`, `resume <id>`,
`skip <id> --reason` (advances cursor past the failing event, logs event payload +
reason to stderr and a row note in `last_error`), `retire <id>`,
`bump-generation`. It never silently advances a checkpoint. Tests: floor math with
paused/Genesis/retired mixes; MinRetention lower bound honored.

### Step 6 — broadcast invalidation plane `[opus]`

**What:** new crate `core/invalidation`; `core/lifecycle/src/context.rs` (handle
accessor); `core/app/src/lib.rs` (lifecycle ownership); root `Cargo.toml`; tests.

**Why now:** the durable cache subscriptions removed on paper in Step 1's matrix
still run as real handlers; config (Step 7) and inventory (Step 8) need the
replacement first.

**How:** `Invalidation::register(channel: &str, name: &str, callback)` where
callback is an async authoritative refresh. One `PgListener` per DB process
LISTENs to all registered channels; each committed NOTIFY invokes matching
callbacks independently; reconnect triggers a full refresh of every callback; a
30s poll is the lost-NOTIFY fallback. `app::run` constructs it iff DB (like the
plane), starts it after module `start` completing each callback's first refresh
before readiness goes green (callbacks are registered during `init`); readiness
fails if a callback hasn't refreshed successfully for 60s. Processes without a DB
(gateway-svc) host no invalidation plane — and have no cache consumers.
Fold config's hand-rolled `listen()`/`listen_once` semantics (boot-vs-reconnect
heal) into this crate's tests as the reference behavior; config switches in Step 7.

### Step 7 — config: monotonic revision, trigger emission, callback caches `[opus]`

**What:** `modules/config/src/lib.rs` + tests + its schema DDL;
`api/config/api/src/lib.rs` (snapshot op gains `revision`);
`api/config/events/src/lib.rs` (payload gains `revision`, `operation`, nullable
`value`); `api/config/rpc/src/lib.rs` (drop `"config-cache"` durable sub, register
invalidation callback).

**Why now:** invalidation plane exists; config is its first real consumer and the
only SQL-trigger producer, which needs the Step 2 writer protocol.

**How:** add `config.revision` singleton. Replace the INSERT/UPDATE trigger with
an INSERT/UPDATE/**DELETE** trigger that branches on `TG_OP` (DELETE reads `OLD`,
not `NEW`) and (a) locks and increments the revision, (b)
`pg_notify('config_changed', …)`, (c) calls `asyncevents.append_event(
'config.changed', 1, payload)` — the plane-owned function from Step 2, so config's
DDL never touches plane tables and the writer protocol has exactly one
implementation; a contract test drives the native writer and the trigger and
asserts identical position/locking behavior. Delete the module's `PgListener`
loop and the listener-path `emit_changed` (psql writes now audit via the trigger —
the current behavior's whole point, preserved with less machinery). `snapshot()`
returns `{revision, settings}` from one SQL statement. Local `Service` and remote
`CachedConfig` register invalidation callbacks on `config_changed` for REFRESH
only: atomic full-map swap, apply only if `revision` is newer (CachedConfig
refreshes over the existing snapshot RPC — never reads the `config` schema
directly). **The boot guarantee does not degrade:** `CachedConfig` keeps its
current boot-fill-during-`start`-or-fail-startup behavior (`api/config/rpc`'s
`RemoteBoot`), and the local `Service` keeps loading its snapshot in `start`; the
invalidation callback replaces only the durable `"config-cache"` subscription's
refresh role, so "config-svc down at boot" remains a loud startup failure, not a
degraded-ready. DELETE events carry `value: null`.

### Step 8 — inventory: drop the second config cache `[sonnet]`

**What:** `modules/inventory/src/lib.rs`, `modules/inventory/src/tests.rs`.

**Why now:** config refresh is authoritative and replica-local after Steps 6–7;
the `Starter` `RwLock` and its durable subscription are now dead weight.

**How:** delete `Starter`, `on_config_changed`, and the `config.changed`
subscription; `grant_starter` reads both starter keys through the injected
`dyn configapi::Config` (already fresh via invalidation) inside the handler
transaction. Keep only the created/deleted subscriptions from the matrix. Tests:
update config, wait for revision application, prove the next grant uses the new
starter item.

### Step 9 — rating becomes a persistent projection `[opus]`

**What:** `modules/rating/src/lib.rs` + tests + new schema DDL (`rating.ratings
(player text PRIMARY KEY, mmr integer NOT NULL)`); `cmd/rating-svc` doc comment;
CLAUDE.md blurb updated in Step 12.

**Why now:** under V2 a durable checkpoint over an in-memory effect is dishonest —
the checkpoint advances past events whose effect a restart erases. The worker
exists (Step 3), so the fix is a plain `TransactionalPg` handler.

**How:** `migrate` creates the table; the `match.finished` handler downcasts the
handed `PgConnection` and upserts both players (`INSERT … ON CONFLICT … UPDATE`,
default 1000, ±15) in the delivery transaction; `MmrReader::get` reads the DB.
Delete the in-memory map. "Restart resets to 1000" stops being true. Tests: two
reports accumulate; restart (new Service instance) preserves MMR.

### Step 10 — per-process module lists become a shared source `[sonnet]`

**What:** add `src/lib.rs` beside each of the 13 `cmd/*/src/main.rs` exporting
`pub fn modules() -> Vec<Box<dyn lifecycle::Module>>` (and the stub wiring that
needs no runtime handles); mains call their own `lib::modules()`;
`tools/checkmodules` re-exports per-process lists + `DeploymentProfile::{Monolith,
Split}` instead of the manually mirrored `monolith_modules()`.

**Why now:** Step 11's one-host-per-subscription-per-profile validation needs the
real per-process composition, not a hand-copied catalog; doing this earlier would
have churned against Steps 3–9's module changes.

**How:** each lib exports `pub fn modules(wiring: &ProcessWiring) ->
Vec<Box<dyn Module>>` where `ProcessWiring` carries the runtime-parameterized
inputs (peer addresses for `remote::Stub`s, passthrough origins) — `main.rs`
builds it from env; checkers pass dummies (`register`/`init` do no I/O, so dummy
peer addresses are safe — the same trick topiccheck's lazy pool already uses).
Player-edge/QUIC handles stay in `main.rs`: `cmd/gateway-svc`'s lib constructs
`Gateway::new()` WITHOUT `with_player_edge`/`with_passthrough` in checker mode —
gateway hosts no durable subscriptions, so the recorded event graph is unaffected;
main.rs adds the runtime builders. `cmd/server`'s lib **excludes `demos/webui`**
(added only in its `main.rs`), so `checkmodules → cmd libs` never pulls a demo
crate — archcheck's "demos only imported by cmd/server" rule keeps holding
textually and transitively. archcheck's "only gateway-svc + server may depend on
the `gateway` crate" rule gains the derived exception for the checker path
(`tools/checkmodules`/`topiccheck` via the cmd libs) — updated in the same
change. This is deliberately NOT the 2202 plan's full `ProcessSpec{protocol,…}` —
no protocol fencing exists to encode. `checkmodules`' "MUST track cmd/server"
comment dies; it now imports the cmd libs.

### Step 11 — checkers enforce the new seam `[opus]`

**What:** `tools/topiccheck/src/main.rs`, `tools/archcheck`, their tests;
`verify.sh`/`verify.ps1` stage wiring if flags change.

**Why now:** final shape is known; checks encode it without temporary exceptions.

**How:** topiccheck's `RecordingTransport` records `(SubscriptionSpec, topic,
version)`; `defined_topics()` carries the six contracts (version + history).
Validate per deployment profile (from Step 10): every subscribed topic is defined
with matching version; subscription IDs globally unique; each subscription hosted
by exactly one process per profile (replicas are the same process; gateway-svc
hosts no plane and is exempt); durable contract topics never subscribed via plain
`on` (existing check kept). archcheck: ban the strings
`EVENTS_SUBSCRIBERS`/`EVENTS_ORIGIN`/`"/events"` workspace-wide (enabled here —
Step 3 deleted the code, Step 4 rewrote the cmd-main doc headers) and ban SQL
references to `asyncevents.` outside `core/asyncevents`, `tools/eventctl`, and
`*/tests*`, with exactly one allowlisted pattern: calls to
`asyncevents.append_event(` (the plane-owned writer function config's trigger
uses). Split-proof scripts are shell, out of archcheck's scope — reviewed by eye.

### Step 12 — delete `core/outbox`, close docs, full verification `[inline]`

**What:** remove `core/outbox` from the workspace; `core/asyncevents/README.md`;
`CLAUDE.md` (seam 3 description, recipe step 5, core list, rating/config blurbs,
smoke-test notes); `docs/reference/` event-plane page if warranted; memory update
(`durable-event-plane-bus-owned.md` rewrite); final status doc.

**Why now:** deletion and doc claims come only after every checker and both
topology proofs pass.

**How:** delete the crate + workspace entry (nothing imports it after Step 3;
archcheck tripwire from Step 11 already guards regressions). Local wipe
instruction: fresh DB or `DROP SCHEMA asyncevents CASCADE` before first boot
(Plane::migrate also drops legacy tables defensively). Then the full net:
`cargo build/clippy/test --workspace`, `cargo run -p archcheck`, `-p topiccheck`,
`verify.ps1` (and `verify.sh` where available), `./split-proof.ps1`, monolith
parity, `scripts/smoke-split-asyncevents.sh`. Documentation states the contract:
delivery is at-least-once per subscription with a stable `event_id`; effects are
exactly-once for `TransactionalPg`; ordering is per-subscription in XID-allocation
order; caches are freshness (invalidation plane), not delivery. Update memory and
run `scripts/memory-sync.ps1 push`.

## Deliberate scale choices (documented so nobody "fixes" them ad hoc)

- One event per delivery transaction, drain loop per subscription — no batching
  knob until a real throughput need appears.
- One checkpoint per subscription — no partition-level parallelism; `partition_key`
  intentionally absent from the schema.
- Generation bump is an offline operator action (`eventctl bump-generation`); the
  shared/exclusive advisory-lock fence makes an online bump safe but nothing
  automates it.
- If the project ever leaves the single-Postgres boundary, the plane implementation
  is replaced by a broker; `emit_tx`, contracts, and `SubscriptionSpec`s survive.

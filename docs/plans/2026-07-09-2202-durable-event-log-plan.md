# Durable event log V2: topology-free pull subscriptions

**Date:** 2026-07-09 22:02  
**Status:** reviewed; the second grumpy-review blockers are addressed in this document  
**Supersedes:** the push/outbox/HTTP/inbox delivery mechanism described by
[`2026-07-09-1118-asyncevents-plane-plan.md`](2026-07-09-1118-asyncevents-plane-plan.md),
but not its decision that `asyncevents` is app-owned process infrastructure.

## Outcome

Replace `EVENTS_SUBSCRIBERS`, per-process outbox relays, `POST /events`, and inbox
dedup with one shared PostgreSQL append log and consumer-owned pull subscriptions.
The module-facing invariant remains:

- the publisher owns the versioned event contract and calls `emit_tx` inside its
  domain transaction;
- the consumer owns its globally unique subscription and handler semantics;
- monolith and split processes execute the same producer and consumer code;
- multiple replicas of a service form one durable consumer group for persistent
  effects; replica-local caches use a separate broadcast invalidation plane;
- a PostgreSQL handler commits its domain effect and checkpoint in the same
  transaction.

This is a successor, not a second permanent event system. Coexistence exists only
for the staged migration and has an explicit, DB-enforced cleanup gate.

**Deployment boundary:** every producer and durable consumer in this design shares
one PostgreSQL primary/transaction-ID domain, as every current monolith and split
process does. It is not a design for independently owned per-service databases or
multi-primary writes. Crossing that boundary replaces the plane implementation with
a broker/log while preserving event contracts and subscription descriptors; it does
not extend the XID protocol across databases.

## Non-negotiable correctness model

### Log position and visibility

`bigserial`/identity is never the checkpoint key. A V2 event position is:

```text
EventPosition = (generation: bigint, producer_xid: xid8, tie_breaker: bigint)
```

`producer_xid` is `pg_current_xact_id()` from the top-level producer transaction.
`tie_breaker` orders multiple events written by that same transaction only. For
the current generation a reader may observe only rows satisfying:

```sql
producer_xid < pg_snapshot_xmin(pg_current_snapshot())
```

Rows from a completed older generation are all eligible. Therefore no transaction
can later commit an event behind an advanced cursor. Ordering is transaction-ID
allocation order, not wall-clock/commit order. A long-running transaction delays
the visibility frontier and is alarmed; it does not cause a skip.

Every native enqueue and every SQL producer/mirror trigger first obtains the same
transaction-scoped shared advisory lock, reads `plane_meta.generation`, and then
inserts. A generation change obtains the exclusive form, waits for every shared
holder, and updates generation plus PostgreSQL `system_identifier` atomically.
Callers cannot provide a generation. `max_prepared_transactions = 0` is a startup
precondition because prepared event-producing transactions are forbidden.

### Delivery classes

- `TransactionalPg`: one event while holding the subscription row lock; handler
  effect and cursor commit in one PostgreSQL transaction.
- Replica-local/ephemeral state is not a durable subscription. It uses broadcast
  invalidation followed by an authoritative snapshot refresh.

V2 handlers do not perform direct external I/O. A consumer that must call an
external system transactionally writes a module-owned command outbox using the handed
PostgreSQL transaction; its idempotent dispatcher is a separate boundary. The event
worker therefore has one honest durable execution model and never holds its lock
across network I/O.

There is no automatic skip. A poison event applies exponential backoff from 1s to
5m and pauses the subscription after 20 consecutive failures. Paused subscriptions
block retention until an explicit operator decision.

### History and recovery are separate contracts

Publisher history policy:

```text
MinRetention(7 days) | ArchiveFromEpoch(GenerationBoundary)
```

`MinRetention` is a lower bound, never a maximum: active or paused subscription
checkpoints can retain data indefinitely. `ArchiveFromEpoch` is a materialized,
fenced generation boundary; every row in that generation and later is structurally
excluded from GC. It is not a label or approximate timestamp.

Consumer start/recovery mode has no default:

```text
StartPosition::Genesis
AfterRegistrationXid(generation, registration_xid)
ExplicitPosition(EventPosition)
ProjectionSnapshot { artifact_revision, position }
```

`AfterRegistrationXid` stores an exclusive XID floor, not a fabricated
`MAX(tie_breaker)`: all events produced by the registration transaction are
excluded. The floor is allocated and the subscription inserted in one transaction
and is immutable thereafter. `ProjectionSnapshot` means the module atomically owns
both a projection artifact and its log position. Inventory has no event-rebuild
claim: recovery is from database backup; its migration baseline is the existing
materialized state.

## Initial contract matrix

| Event contract | Publisher history | Durable subscription | Handler | Recovery |
|---|---|---|---|---|
| `player.registered/v1` | `MinRetention(7d)` | `audit.player-registered/v1` | `TransactionalPg` | migration baseline |
| `character.created/v1` | `MinRetention(7d)` | `inventory.character-created/v1`, `audit.character-created/v1` | `TransactionalPg` | inventory DB backup; audit baseline |
| `character.deleted/v1` | `MinRetention(7d)` | `inventory.character-deleted/v1`, `audit.character-deleted/v1` | `TransactionalPg` | inventory DB backup; audit baseline |
| `match.finished/v1` | `ArchiveFromEpoch` at bridge generation activation | `rating.match-finished/v1`, `leaderboard.match-finished/v1`, `audit.match-finished/v1` | `TransactionalPg` | rating/leaderboard baseline at the same boundary; audit baseline |
| `config.changed/v1` | migration-only legacy contract | audit legacy range only | `TransactionalPg` | explicit migration range |
| `config.changed/v2` | `MinRetention(7d)` | `audit.config-changed/v2` | `TransactionalPg` | migration baseline |
| `scheduler.fired/v1` | `MinRetention(7d)` | `audit.prune-on-scheduler/v1` | `TransactionalPg` | `AfterRegistrationXid` after migration |

Config cache refresh and inventory starter configuration are deliberately absent
from this table: they are invalidation/snapshot state, not durable projections.

## Target database schema

The V2 migration creates the following under `asyncevents`; names are normative:

- `schema_migrations(version bigint primary key, applied_at timestamptz)`;
- `plane_meta(singleton bool primary key, generation bigint not null,
  system_identifier numeric not null, minimum_process_protocol bigint not null,
  legacy_gc_frozen bool not null, consumer_rollback text not null check
  (consumer_rollback in ('OPEN','CLOSED')), config_emission_epoch bigint not null)`;
- `events(generation bigint not null, producer_xid xid8 not null, tie_breaker bigint
  generated always as identity, event_id text not null unique, topic text not null,
  contract_version integer not null check(contract_version > 0), payload jsonb not
  null, created_at timestamptz not null, primary key(generation,producer_xid,tie_breaker))`;
- `subscriptions(subscription_id text primary key, topic text not null,
  contract_version integer not null, handler_kind text not null check(handler_kind =
  'transactional_pg'), state text not null check(state in
  ('active','paused','retired')), cursor_generation bigint, cursor_xid xid8,
  cursor_tie bigint, floor_generation bigint, floor_xid xid8, next_attempt_at
  timestamptz, consecutive_failures integer not null default 0, spec_hash text not
  null, start_kind text not null, updated_at timestamptz not null)`;
- `legacy_topic_map(topic text primary key, contract_version integer not null,
  enabled bool not null)`;
- `legacy_claim_ranges(subscription_id text, legacy_subscriber text not null,
  first_outbox_id bigint, last_outbox_id bigint null, closed_at timestamptz,
  primary key(subscription_id,first_outbox_id))`;
- `legacy_delivery_gates(topic text, legacy_subscriber text, state text not null
  check(state in ('open','closed')), epoch bigint not null, final_outbox_id bigint,
  primary key(topic,legacy_subscriber), check((state='open' and final_outbox_id is
  null) or (state='closed' and final_outbox_id is not null)))`;
- `migration_boundaries(name text primary key, legacy_outbox_id bigint not null,
  completed_generation bigint not null, activated_generation bigint not null,
  created_at timestamptz not null)`;
- `history_contracts(topic text, contract_version integer, policy text not null,
  archive_generation bigint,
  primary key(topic,contract_version))`;
- `projection_baselines(subscription_id text primary key, artifact_revision text not
  null, generation bigint not null, producer_xid xid8 not null, tie_breaker bigint
  not null, created_at timestamptz not null)`;
- `process_leases(instance_id text primary key, process_id text not null, protocol
  bigint not null, epoch bigint not null, expires_at timestamptz not null)`;
- `subscription_hosts(subscription_id text, instance_id text, process_id text not
  null, consumer_group text not null, expires_at timestamptz not null,
  primary key(subscription_id,instance_id))`;
- `cleanup_manifest(singleton bool primary key, checked_at timestamptz, result jsonb,
  approved_by text, approved_at timestamptz)`.

The migration also creates `events_subscription_scan(topic,contract_version,
generation,producer_xid,tie_breaker)` and
`subscriptions_due(state,next_attempt_at,subscription_id)` indexes. Foreign keys bind
subscriptions and history rows to immutable contract metadata. Subscription CHECKs
require a complete cursor tuple or none, exactly the fields required by the selected
start mode, and no cursor/floor on retired rows. Claim ranges reference their
subscription; topic/version are derived from that immutable row rather than
duplicated. Range CHECKs require `first_outbox_id > 0`, `last_outbox_id >=
first_outbox_id`, and `closed_at` iff the upper bound exists; a partial unique index
allows at most one open range per subscription. Gate-serialized creation rejects
overlap rather than selecting an alias heuristically.
All invariant-bearing fields are `NOT NULL`; nullable columns are limited to the
cursor/floor variants not selected by `start_kind`, the open range's
`last_outbox_id/closed_at`, and `archive_generation` for a non-archive policy. Policy
CHECKs enforce those exact combinations.

`event_id` remains canonical `text` because mirrored legacy IDs are
`asyncevents:<outbox_id>`. V2-native IDs are generated by the plane and never parsed
by consumers. No `partition_key` is added until checkpoint-per-partition semantics
exist.

## Ordered implementation and migration plan

### Step 1 — freeze the public contracts in `core/bus` `[opus]`

**What:** `core/bus/src/lib.rs`, `core/bus/src/tests.rs`, every
`api/*/events/src/lib.rs`, their Cargo manifests and tests.

**Why now:** schema, workers, modules and checkers must compile against one explicit
topic/version/history vocabulary before any storage implementation exists.

**How:** evolve `EventType<T>` to contain an immutable `EventContract { topic,
version, history }`; replace bare `define(topic)` calls with
`define(topic, version, HistoryPolicy)`. Add `SubscriptionSpec`, `HandlerKind`,
`StartPosition`, and `LegacyClaimRange`. Change `Transport` so
`enqueue_tx` receives `&EventContract` and subscription registration receives the
consumer-owned descriptor rather than separate topic/subscriber strings. Add typed
`Bus::on_tx(&SubscriptionSpec, &EventType<T>, handler)` and raw equivalent. Reject a
descriptor whose topic/version differs from the event contract. Keep `AnyTx` and
`Delivery` engine-neutral; `Delivery::event_id()` remains stable text.

### Step 2 — make declarations module-owned and non-duplicated `[opus]`

**What:** `modules/{inventory,rating,leaderboard,audit}/src/lib.rs`, their tests,
`core/lifecycle/src/module.rs`.

**Why now:** the worker and checker need an authoritative catalog; extracting it
after worker wiring would leave a second hand-maintained graph.

**How:** add `ModuleEventSpec` and a default-empty `Module::event_spec()`. Each
consumer exports one `LazyLock<ModuleEventSpec>` containing its exact
`SubscriptionSpec`s. Registration helpers accept those descriptor objects directly,
so `init` never repeats topic/version/subscription IDs. Give every topic reaction a
unique versioned ID from the matrix above; audit no longer shares one subscriber
name across six topics. Remove durable `config-cache` and inventory's durable
`config.changed` descriptors. Unit tests assert descriptor uniqueness and that every
registered handler points at the same descriptor instance exposed by the module.

### Step 3 — replace manual composition catalogs with `ProcessSpec` `[opus]`

**What:** add `core/lifecycle/src/process.rs`; add `src/lib.rs` beside every
`cmd/*/src/main.rs`; update `core/app/src/lib.rs`, `tools/checkmodules`,
`tools/topiccheck`, `tools/requirecheck`, and the relevant Cargo manifests.

**Why now:** V2 readiness, protocol fencing and static graph validation need the
same process identity and module selection that the executable really boots.

**How:** define `ProcessSpec { process_id, protocol, consumer_group_mode, build }`,
explicit `DeploymentProfile` sets for `monolith` and `split`, and `ProcessRuntime`
for runtime-only edge/player handles. Each command package exports
`process_spec()`; its `main.rs` only constructs runtime handles and calls
`app::run_process`. A spec selects real module/stub factories and obtains durable
metadata through `Module::event_spec()`; it never redeclares subscriptions.
`checkmodules` invokes the same exported constructors in a recording runtime.
`topiccheck` validates exactly one logical host per subscription inside each
deployment profile, not across mutually exclusive profiles. Multiple replicas with
the same `process_id` are one consumer group; active hosts with different process IDs
for the same subscription are rejected unless the spec explicitly assigns both to
the same group. Closure bodies remain runtime objects and are never serialized or
source-parsed.

### Step 4 — install versioned V2 storage without activating delivery `[opus]`

**What:** add `core/asyncevents/migrations/0001_event_log_v2.sql` and
`0002_migration_control.sql`; update `core/asyncevents/src/lib.rs` (`Plane::migrate`),
tests and README.

**Why now:** all later rollout phases require durable control records and idempotent,
serialized migrations while the old relay still owns production delivery.

**How:** replace monolithic V2 DDL additions with ordered embedded SQL migrations,
recorded in `schema_migrations` under a global advisory migration lock. Preserve the
legacy outbox/inbox tables unchanged. Seed `plane_meta` with current
`pg_control_system().system_identifier`, generation 1, current process protocol,
`legacy_gc_frozen=false`, and rollback `OPEN`. The app migration only verifies access
to `pg_control_system()` and fails with the exact required DBA `GRANT`; granting it
is a documented privileged bootstrap prerequisite. Startup rejects a changed cluster
identity, `max_prepared_transactions != 0`, or any row in `pg_prepared_xacts`.

### Step 5 — implement commit-safe native append `[opus]`

**What:** split `core/asyncevents/src/lib.rs` into `store.rs`, `producer.rs`,
`migration.rs`, and `tests/{ordering,generation}.rs`; adapt `Plane::transport`.

**Why now:** mirroring and workers must consume the same canonical log writer and
position semantics.

**How:** in `Transport::enqueue_tx`, downcast `AnyTx`, take the shared advisory
generation lock, call `pg_current_xact_id()`, read generation inside the same
transaction, and insert `(event_id,topic,contract_version,payload)`. Never use
identity as a cursor. Add generation-bump code taking the exclusive advisory lock
and updating system identity atomically. Tests use two controlled PostgreSQL
transactions to prove: XID 100 can commit after 101 without being skipped; multiple
events in one transaction follow `tie_breaker`; the exclusive bump waits for an
in-flight shared writer; restored/new-cluster identity requires a fenced bump.

### Step 6 — implement the pull worker and failure state machine `[opus]`

**What:** add `core/asyncevents/src/{catalog,worker,retention,wakeup}.rs`, tests;
update `Plane::{start,stop}` and `core/app` readiness contributions.

**Why now:** consumers can be migrated only after the target executor has proven
ordering, retry and multi-replica behavior independently of legacy delivery.

**How:** reconcile code declarations into `subscriptions` at startup: local checks
run before migrations; DB reconciliation runs after plane migrations. Creation
requires an explicit start/recovery mode; an existing row with a different immutable
`spec_hash` fails readiness. Each process worker queries only subscription IDs for
which that process registered a local handler; a DB row never causes an unrelated
service to execute it. It renews `subscription_hosts` leases and rejects a live host
with a different `process_id` unless both specs name the same consumer group. For
`TransactionalPg`, select one due subscription row
`FOR UPDATE SKIP LOCKED`, compute the safe XID frontier, select one next event, open
a savepoint, insert any applicable legacy claim inside that savepoint, call the
handler, and commit effect plus cursor. On error/10s timeout, cancel the handler, wait
for connection protocol recovery, roll back to the savepoint, update attempt/pause
state outside it, and commit immediately. Workers never sleep while holding a row
lock. A single LISTEN/NOTIFY loop wakes the pool, with
one global 1s poll fallback rather than polling each subscription. A panicked worker
is supervised and makes readiness fail. Metrics cover lag, safe-frontier age,
attempts, pauses, lease expiry and oldest transaction age.

### Step 7 — implement retention, retirement and archive enforcement `[opus]`

**What:** `core/asyncevents/src/retention.rs`, `core/asyncevents/src/catalog.rs`,
operator SQL/check command under `tools/eventctl`, tests.

**Why now:** retention must be mechanically coupled to checkpoints before V2 receives
production data.

**How:** GC computes a per-topic floor from every active and paused subscription's
`effective_retention_position`: its cursor after the first success; before that its
exclusive XID floor, explicit position or projection position; `Genesis` pins the
start of available history. It honors the 7-day minimum and structurally excludes
every row in/after an archived generation. A paused/poisoned subscriber blocks deletion and raises
an age/disk alarm. Retirement is an explicit `eventctl retire-subscription` operation
that records actor/reason/final position; removing code is not retirement. Static
checker reports declaration drift; runtime readiness separately reports orphaned DB
rows. `eventctl` also exposes the generation bump, baseline and cleanup checks; it
never silently advances a poisoned checkpoint.

### Step 8 — add the replica-broadcast invalidation plane `[opus]`

**What:** add workspace crate `core/invalidation` with `src/lib.rs` and tests;
inject its handle through `core/lifecycle/src/context.rs`; own its lifecycle in
`core/app/src/lib.rs`; update root Cargo files and readiness tests.

**Why now:** durable cache subscriptions must not be removed until every process
replica has a topology-independent replacement.

**How:** one `PgListener` connection per DB process LISTENs to registered channels.
Consumers register named refresh callbacks during wiring. `app` starts the plane
after migrations but before module `start`, and startup fails unless every callback
completes its first authoritative refresh. Each committed NOTIFY invokes callbacks
independently; reconnect performs a full refresh; a 30s poll is the lost-NOTIFY
fallback. Cache replacement is atomic. Callback failures do not block siblings, but
readiness fails when any callback has not successfully refreshed for 60s. This plane
contains no durable checkpoint and promises freshness, not event delivery.

### Step 9 — give config a monotonic revision and exact snapshots `[opus]`

**What:** `modules/config/src/lib.rs`, tests; `api/config/api/src/lib.rs`;
`api/config/rpc/src/lib.rs`; `api/config/events/src/lib.rs`; config migration SQL
embedded by the module.

**Why now:** config and inventory cannot leave legacy durable cache handlers until
revision-based refresh works in monolith, split and multi-replica deployments.

**How:** add singleton `config.revision`. A row trigger covers INSERT, UPDATE and
DELETE, locks/increments the singleton, and NOTIFYs operation/key/revision after
commit. `snapshot_v2()` returns `{revision, settings}` from one SQL statement snapshot
(not two READ COMMITTED selects). Both local `Service` and remote `CachedConfig`
register invalidation callbacks; each replaces its entire map atomically and updates
`applied_revision` only after the swap. During phase 1 the trigger does not append a
native V2 event: the existing listener still emits legacy `config.changed/v1`, which
the mirror will capture. Preserve the old snapshot RPC during rolling compatibility.

### Step 10 — remove inventory's second config cache `[sonnet]`

**What:** `modules/inventory/src/lib.rs`, `modules/inventory/src/tests.rs`.

**Why now:** config refresh is authoritative and replica-local after Steps 8–9; the
extra `Starter` `RwLock` would retain ordering dependence and stale-state risk.

**How:** delete `Starter`, `on_config_changed`, and the durable
`config.changed` handler. The character-created handler reads both starter keys from
the injected `dyn configapi::Config` snapshot during its transaction. Keep only the
durable created/deleted subscriptions from the contract matrix. Tests update config,
wait for revision application, and prove grants use one coherent refreshed snapshot
on two inventory replicas.

### Step 11 — persist rating on the old plane before overlap `[opus]`

**What:** `modules/rating/src/lib.rs`, tests and new schema migration; rating API/RPC
tests; `cmd/rating-svc`; `core/asyncevents` protocol fence support; run/split scripts.

**Why now:** an in-memory and persistent handler sharing legacy subscriber `rating`
cannot coexist safely; rating must become durable before any V2 worker can race it.

**How:** first ship a compatibility release in which every legacy consumer,
immediately before its inbox claim, takes the shared transaction-scoped advisory lock
for `(topic,legacy_subscriber)` and verifies that the corresponding
`legacy_delivery_gates` row is `open`; every old handler/relay also obeys the
DB-backed process-protocol epoch. Integration tests prove an exclusive gate waits for
an in-flight handler and prevents every later claim. Then, for the rating cutover:
fence `match.finished` producers
and rating reads, remove routing to every legacy rating endpoint, terminate and verify
their DB sessions, create the rating table/baseline 1000, start only the persistent
old-plane `rating` handler, execute a write/read probe, then resume match producers.
Heartbeats are observability only. The table update uses the handed legacy delivery
transaction. Record the rating artifact revision so Step 15 can atomically bind it to
the V2 projection baseline.

### Step 12 — freeze legacy GC and install the exact mirror boundary `[opus]`

**What:** `core/asyncevents/migrations/0003_legacy_bridge.sql`, migration code/tests,
`tools/eventctl`, operational docs.

**Why now:** after all old consumers are persistent or refreshable, new events can be
mirrored without losing the evidence required for per-subscription cutover.

**How:** first install DB `BEFORE DELETE` guards on legacy outbox/inbox controlled by
`plane_meta.legacy_gc_frozen`, set it true, and verify deletion is rejected regardless
of binary version. Reject existing prepared transactions. In one transaction: lock
legacy outbox `SHARE ROW EXCLUSIVE`; acquire the **exclusive** generation advisory
lock; capture `boundary_id = coalesce(max(id),0)`; increment generation and define
the migration baseline as the end of the completed previous generation; materialize
the `match.finished/v1` archive epoch as the start of the new generation; populate
and freeze `legacy_topic_map`; create one open `legacy_delivery_gates` row and one
open `legacy_claim_ranges(first_outbox_id=boundary_id+1,last_outbox_id=NULL)` row for
every migrating subscription; freeze the old target graph; create the mirror trigger;
persist both boundaries; commit. The
trigger takes the generation shared lock, maps topic to the immutable legacy version,
uses exact `event_id = 'asyncevents:' || NEW.id`, and mirrors every subsequent row.
Consequently even a legacy transaction that allocated its XID before bridge
activation but inserts afterward is mirrored into the new generation and can never
land behind the migration baseline.
Unknown/disabled durable topics abort the producer transaction. Pre-boundary rows are
not replayed. With legacy GC still frozen, drain every pre-boundary row through the
frozen target graph and resolve every failure before recording each consumer's audited
materialized-state baseline; freshly delivered rows retain their exact inbox claims.
Claims already deleted before the freeze cannot be reconstructed, so the baseline is
the migration contract for that earlier history. Where old GC destroyed historical
proof, the manifest states that limitation instead of claiming retroactive exact
delivery.

### Step 13 — define and prove per-subscription legacy obligations `[opus]`

**What:** `core/asyncevents/src/{catalog,worker}.rs`, `tools/eventctl`, module
subscription descriptors and integration tests.

**Why now:** shared old/V2 processing is safe only when both paths contend on the
same exact legacy claim for a bounded range.

**How:** use the open gates/ranges atomically established by Step 12. The open upper
bound means every mirrored event is obligated
until cutover. Cutover takes the corresponding exclusive advisory lock, waits for all
in-flight legacy handlers, changes the gate to `closed`, captures the visible final
outbox tail and writes it as the immutable range upper bound in the same transaction.
Events committing later fall outside the range and are V2-only. The global process
protocol gate cannot substitute for this per-subscription fence.
`SubscriptionSpec.legacy_claim` applies only to mirrored IDs in
that range. The V2 worker inserts the exact old `(event_id,subscriber)` inbox claim inside
the handler savepoint: conflict means atomic no-op plus V2 checkpoint; success means
effect plus both claim and checkpoint commit. Close a range only after the old target
is fenced and its final obligated row is known. Legacy inbox GC remains forbidden
until every range is closed and its V2 checkpoint has passed the tail. `sent_at` alone
is never accepted as per-target delivery evidence.

### Step 14 — migrate consumers one subscription at a time `[opus]`

**What:** module specs/handlers, process specs, `eventctl`, run scripts and V2 worker
integration tests.

**Why now:** the bridge and shared-claim protocol are proven; per-subscription rollout
limits blast radius and preserves a defined consumer rollback window.

**How:** first migrate audit event recording, inventory delete/create, and audit
scheduler prune. Rating and leaderboard follow the coordinated projection cut in
Step 15. For each subscription handled here: create its V2 row at
the recorded baseline; start shadow catch-up using legacy claims; compare projection
state/lag; DB-fence the old endpoint; set final legacy range tail; let V2 cross it;
close the range; then mark that consumer rollback closed. Rolling back a consumer is
allowed only before its range closes and never by running both unfenced endpoints.
Test two replicas of the same service: `SKIP LOCKED` permits only one owner per event,
failover resumes from the shared checkpoint, and transactional effects occur once.

### Step 15 — bind and migrate rating/leaderboard projections atomically `[opus]`

**What:** `core/asyncevents` metadata operations, rating/leaderboard migrations,
`tools/eventctl`, tests.

**Why now:** the archive generation already began at the bridge boundary, but rating
and leaderboard cannot start V2 independently of a position that exactly matches
their current materialized state.

**How:** fence `match.finished` producers and rating reads; wait for the safe XID
frontier; drain all committed legacy match events; then take the exclusive legacy
delivery gates for rating and leaderboard and wait for their in-flight handlers. In
one transaction lock both projection states, capture the highest eligible mirrored
`match.finished` position, record both `(artifact/schema revision,position)`
baselines, create their V2 subscriptions/checkpoints at that same position, close
their legacy ranges by writing both gates' `final_outbox_id` and both ranges'
`last_outbox_id/closed_at` according to Step 13, and commit. The V2
baseline/checkpoint position is separate from that legacy outbox tail. Activate their V2 workers after that commit while
producers remain fenced. Execute state/read probes before
reopening producers and rating reads. The archive epoch remains the bridge generation
from Step 12; GC tests prove no row in/after it can be deleted. Existing history
before it is represented only by the materialized projection baselines. Audit and
inventory retain explicit baseline records without a false rebuildability claim.

### Step 16 — switch config emission from legacy V1 to native V2 `[opus]`

**What:** config migration/trigger, `modules/config`, process protocol fence,
config event contracts and tests.

**Why now:** all caches already use revision invalidation and audit can consume V2;
this avoids any window with two durable config events for one mutation.

**How:** DB-fence all legacy config producers, remove their routes/sessions, and in
one migration advance `config_emission_epoch` while replacing the revision trigger
with a trigger that also appends native `config.changed/v2` containing
`revision,operation,namespace,key,value`; this SQL producer takes the mandatory shared
generation lock and reads generation exactly like every other writer. Deploy config without listener-based legacy
emission and reopen writes. DELETE carries `value=null`. At no time may listener V1
emission and trigger V2 emission both be enabled. Prove one mutation produces exactly
one V2 event and every process replica converges to its revision.

### Step 17 — close consumer rollback, then move producers to native V2 `[opus]`

**What:** producer modules (`accounts`, `characters`, `match`, `scheduler`, config),
`core/asyncevents`, process protocol controls, rollout scripts.

**Why now:** once a producer writes only V2, rolling consumers back to the old push
plane would lose those events; therefore consumer rollback closes first.

**How:** set the DB consumer rollback state to `CLOSED` only after every legacy claim
range and projection comparison passes. Roll native producer binaries gradually:
old writers continue old-outbox + mirror; new writers append directly to V2. The
minimum accepted protocol rejects binaries that do not honor generation and epoch
fences. After the final old writer is fenced, record the mirrored tail and disable
the mirror. No application dual-write is introduced.

### Step 18 — replace topology scripts and split-proof assertions `[opus]`

**What:** `run.sh`, `run.ps1`, `split-proof.sh`, `split-proof.ps1`,
`scripts/smoke-split-asyncevents.sh`, `verify.sh`, `verify.ps1`, command docs.

**Why now:** only after native producers/consumers work can the deployment graph stop
configuring URLs without hiding a migration failure.

**How:** delete all `EVENTS_SUBSCRIBERS` and `EVENTS_ORIGIN` wiring and remove `/events`
expectations. Add assertions for: producer transaction rollback, XID inversion,
LISTEN loss with poll recovery, two replicas/one checkpoint, crash before/after
effect commit, poison pause without skip, retention blocked by pause, archive GC,
config broadcast to both replicas, persistent rating failover, protocol rejection,
and monolith/split parity using identical subscription IDs. Update split proof for
starter grant, wipe, leaderboard, rating, audit and scheduler using log positions and
checkpoints rather than `sent_at`.

### Step 19 — enforce the architecture statically and at readiness `[opus]`

**What:** `tools/topiccheck`, `tools/archcheck`, `tools/requirecheck`,
`tools/checkmodules`, `core/app` readiness tests.

**Why now:** the final runtime shape is known; checks can encode it without temporary
exceptions.

**How:** static checks validate unique `(topic,version)`, unique subscription IDs,
publisher ownership, explicit history/recovery, exactly one handler per declared
subscription in every selected process, and absence of module dependencies on
`asyncevents`/invalidation implementations. Runtime reconciliation reports declared
subscriptions missing in DB, orphaned DB subscriptions, spec hash drift, stale
invalidation callbacks, paused consumers and unsafe XID frontier. Add an archcheck
tripwire banning direct access to plane tables outside core/tests/eventctl. During
migration, references to `EVENTS_SUBSCRIBERS`, `EVENTS_ORIGIN` and `/events` are
allowed only in the explicitly named legacy bridge files; the unconditional ban is
enabled in Step 20 after those files are deleted.

### Step 20 — execute the auditable legacy cleanup gate `[inline]`

**What:** `tools/eventctl cleanup-check`, final destructive migration, old
`core/outbox` dependency/code, `/events` router, env parsing, legacy tests/docs.

**Why now:** irreversible deletion is last and requires machine-verifiable evidence,
not deploy order or missing heartbeats.

**How:** advance `minimum_process_protocol`; reject legacy writer/relay registration
and inbound delivery; remove legacy service routing; terminate remaining legacy DB
sessions; require zero accepted legacy leases plus orchestrator inventory
confirmation. `cleanup-check` must atomically prove: rollback `CLOSED`; every legacy
claim range closed; checkpoints beyond mirrored tails; no unsent legacy outbox rows;
mirror disabled only after producer fence; no accepted legacy protocols/endpoints;
no active descriptor depends on inbox; archive/baseline metadata valid; fresh backup;
and separate named operator approval. Persist the JSON result in `cleanup_manifest`.
The DROP migration fails unless every predicate is true, then removes old
outbox/inbox triggers/tables, relay, HTTP sink, `EVENTS_*` routing variables and the
now-unused `core/outbox` crate.
Enable the unconditional archcheck tripwire against `EVENTS_SUBSCRIBERS`,
`EVENTS_ORIGIN` and `/events` in the same cleanup commit, so the tree never enters an
intentionally failing checker state.

### Step 21 — full verification and documentation closure `[inline]`

**What:** workspace tests/checkers, PostgreSQL integration suite, both topology
proofs, `core/asyncevents/README.md`, `CLAUDE.md`, architecture reference and memory.

**Why now:** documentation may claim V2 guarantees only after the destructive gate
and at-risk tests pass.

**How:** run formatting, workspace build/clippy/tests, strict arch/require/topic
checks, V2 database tests with concurrency fault injection, complete `verify.ps1`,
and the Linux `verify.sh` path. Document XID-order semantics, restore generation
runbook, backup-only inventory recovery, archive epoch, poison-event operations,
config freshness SLO and process fencing. The final status record includes exact
cleanup-manifest output and test evidence.

## Rollback boundaries

1. Before bridge installation: ordinary binary rollback.
2. Bridge active, consumer range open: roll that consumer back only after DB-fencing
   its V2 worker; old outbox remains authoritative.
3. Consumer range closed: that consumer cannot return to legacy delivery.
4. Consumer rollback globally `CLOSED`, mixed producers: old writers remain safe via
   mirror, new writers are V2-only; consumer rollback is forbidden.
5. Mirror disabled: producer rollback below the accepted protocol is DB-rejected.
6. Cleanup manifest accepted and legacy tables dropped: restore requires database
   backup, not binary rollback.

## Review corrections incorporated

The second independent review's blockers are closed explicitly above: per-subscriber
delivery obligations replace `sent_at`; legacy claim aliases are bounded contract
data; the mirror has an immutable topic/version map; generation changes share a
mandatory writer fence; rating uses a coordinated DB epoch cutover; GC freeze is
DB-enforced; `AfterRegistrationXid` has a typed exclusive floor; config V1/V2
emission phases cannot overlap; snapshots carry an exact revision; `ProcessSpec` is
an executable shared source rather than a copied catalog; heartbeats cannot authorize
cleanup; and cleanup is a persisted, fail-closed manifest.

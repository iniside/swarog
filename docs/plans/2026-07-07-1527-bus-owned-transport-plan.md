# Plan — Bus-owned transport: collapse the async dual-path debt

**Date:** 2026-07-07 15:27 · **Revised:** 2026-07-07 (post grumpy-review, think-hard)
**Status:** DRAFT (revised; ready for user approval of dispatch tags)
**Decision locked with user:** Variant **A** — the messaging layer becomes a stateful
module owning schema `messaging` with private `outbox`/`inbox` tables. B (per-module
tables) and C (CDC/logical decoding) rejected: B regresses to today's scattered state,
C is disproportionate machinery for this repo.

> **Review note.** An independent think-hard review found a BLOCKER in the first cut:
> a single shared `messaging.outbox` drained by every process's relay silently loses
> events (a foreign process's relay marks a topic it doesn't subscribe to as
> "sent to nobody", `relay.go:167-170`). Variant A is preserved but the **relay
> ownership model is redesigned** (`origin` column + `SKIP LOCKED` + unconditional local
> delivery + per-subscriber tx). All review findings are folded into the steps below and
> tagged `[R#]`.

---

## Context

### The debt (what the user pointed at)

`modules/inventory/inventory.go` (and `modules/audit/audit.go`) carry **two hand-wired
delivery paths for the same async event**: a `bus.On(...)` subscription (monolith,
in-process) *and* a `ctx.Mux.HandleFunc("POST /events/...")` synchronous HTTP sink
(split, cross-process), guarded by an author-maintained "exactly one path per topology"
comment. Symmetrically, producers (`characters`, `scheduler`) hand-write `insertOutbox(tx,…)`
+ a post-commit `bus.Emit(…)` and construct their own `outbox.Relay`. A module therefore
**knows its own deployment topology** — a direct violation of seam #3 (the bus is the
single async glue) and of the "never monolith-only features" principle (memory
`never-monolith-only`).

**Verified scale (research, 2026-07-07):**
- Producers with the dual `insertOutbox`+`bus.Emit` shape: `characters` (`characters.go:246,259`
  create; `:310,321` delete), `scheduler` (`scheduler.go:~299` inline SQL + `:305` emit).
- Consumers with the dual `bus.On`+`POST /events/*`+per-schema `inbox` shape: `inventory`
  (`bus.On` `:142-143`; sinks `:158-159`; `consume`/`inventory.inbox` `:306-330,109-115`),
  `audit` (`Subscribe` loop + `POST /events/audit-<slug>` `:111-119`; `POST /events/scheduler-fired`
  `:134`; `consume`/`audit.inbox`).
- Domain effect logic is already **~80% shared** (`grantStarter`/`wipeCharacter` take a
  `rowQuerier` satisfied by both `*sql.DB` and `*sql.Tx`); the duplication is the *envelope*.

### Overlapping existing systems (Research-before-planning — MANDATORY)

| Existing system | What it does | Why not just extend it |
|---|---|---|
| **`bus/` (leaf)** | In-process async fanout. Imports only `log/slog`+`sync`. No transport seam (`bus.go` full read). | **We DO extend it, additively** — transport is an interface *defined in `bus/`*, implemented by a module (leaf stays module-free, rule #1). Adds `Transport`, `SetTransport`, `EmitTx`, `OnTx`, `OnTxRaw`; no existing caller changes. |
| **`outbox/` (leaf)** | Generic schema-parameterized relay: drains `<schema>.outbox`, POSTs to `/events/*`, at-least-once, `X-Event-Id`, per-subscriber ordering. | **We DO extend it** — add a local-delivery target, an `origin` filter, and `SKIP LOCKED` (see Relay ownership model). Still domain-agnostic, no module import. |
| **`registry` + `modules/remote` (SYNC analog)** | Cross-process *sync* calls already topology-transparent: consumer-defined interface + entrypoint swaps real service vs `remote.Stub` QUIC proxy. | **The template we copy for async.** Registry is passive pull storage; the bus is an active dispatcher, so the transport lives as a nil-able field on `*bus.Bus` installed via `SetTransport`. **But** messaging *also* `registry.Provide`s a marker so `Requires("messaging")` gives us a real boot check (see `[R2]`). |
| **`modules/config`** | DB-backed module owning schema `config`, `Migrate` DDL + NOTIFY trigger, cancellable `Start`/`Stop` LISTEN loop. Registered in every process. | Structural template for the new module. Mirror, don't extend. |
| **`bus.Subscribe` (untyped) in `audit`** | Subscribes by topic string to avoid importing every `*events` package. | Forces an **untyped** durable variant `OnTxRaw(topic, func(ctx,tx,raw) error)` alongside typed `OnTx[T]`. |

**Conclusion:** no seam already provides topology-transparent async delivery. The sync
plane solved it (registry+remote); `modules/messaging` is the missing async twin.

### Relay ownership model (post-review — this is the load-bearing correctness core) `[R1][R6]`

One shared `messaging.outbox` in one schema (Variant A), but delivery must be
**single-owner and race-free**:

1. **`origin` column.** Every outbox row is stamped `origin` = the writing process's stable
   identity (`MESSAGING_ORIGIN` env; per-`cmd` value, e.g. `server`, `characters-svc`;
   stable across restarts so a crashed process resumes its own unsent rows — never a
   pid/hostname). `EmitTx` stamps it; the producer never sets it.
2. **Each process's relay drains ONLY its own rows:** `SELECT … WHERE sent_at IS NULL AND
   origin = $self … FOR UPDATE SKIP LOCKED`. So `scheduler-svc`'s relay can never touch and
   mark-sent `characters-svc`'s `character.created` row — killing BLOCKER 1. `SKIP LOCKED`
   is belt-and-suspenders against any accidental double-drain within a process.
3. **The producing process owns delivery of its own events** to (a) local in-process
   subscribers and (b) remote peers. A consuming peer NEVER drains the outbox for a foreign
   event — it receives via HTTP `POST /events` and dedups into its own `messaging.inbox`.
4. **Per-subscriber independent delivery (fixes fate-coupling).** Each local subscriber is a
   separate delivery target with its **own** tx and its own inbox row keyed
   `(event_id, subscriber)`. A failing `audit` handler cannot roll back an `inventory` grant;
   it only pins redelivery of *its own* subscription (inventory's inbox dedup makes the
   re-run a no-op). The row's `sent_at` is set only when **every** target — each local
   subscriber + each remote URL — has durably acked.
5. **Local delivery is unconditional.** The relay's current "`len(urls)==0` → mark sent"
   shortcut (`relay.go:167-170`) is reordered: local targets are delivered first and
   independently of remote URLs, so the monolith (empty `EVENTS_SUBSCRIBERS`) still delivers
   to in-process subscribers.
6. **Head-of-line isolation `[R5]`:** the relay's `blocked` gate is keyed per **(topic, url)**
   (not per-url), so a poison `character.created` to a peer cannot stall `scheduler.fired` to
   the same peer. Inbound routing stays a single `POST /events` (topic in `X-Event-Topic`
   header) — the receiver owns one handler, but sender-side isolation is per-topic.

### The two locked design decisions

**Decision 1 — Two planes, chosen by *durability intent*, never by topology.**
- **Best-effort plane** = today's in-process bus (`Emit`/`On`, mailboxes). Local-only,
  fire-and-forget, zero DB. **Unchanged.** For in-memory/idempotent reactions with no
  cross-process durability need (`rating`; `inventory.onConfigChanged`).
- **Durable plane** = outbox log + inbox dedup + relay, owned by `messaging`, reached via
  `EmitTx`/`OnTx`/`OnTxRaw`. Exactly-once, transactional, topology-transparent. The module
  never sees outbox/inbox/HTTP/`EVENTS_SUBSCRIBERS`/`origin`.

**Decision 2 — The durable plane is outbox-driven end to end, in BOTH topologies; no
in-process fast-path.** `EmitTx(tx, topic, v)` writes only the `messaging.outbox` row
(inside the caller's tx). A durable subscriber (`OnTx`) always receives via the relay
(local in-tx `consume` and/or remote HTTP). "Exactly one path per topology" *disappears* —
one path, always.
- **Latency:** an `AFTER INSERT` trigger fires `pg_notify('messaging_outbox', topic)`; the
  relay LISTENs (config's pattern) and wakes immediately, 500 ms ticker as safety net.
  **Cost acknowledged `[R11]`:** per-insert NOTIFY serializes on Postgres's commit-time
  notify queue — fine at this scale, flagged not hidden. The ticker is the correctness floor;
  NOTIFY is only a latency optimization (NOTIFY is best-effort/droppable, so we never *rely*
  on it — the ticker guarantees eventual drain).
- **Documented behavior change:** monolith durable-topic reactions that were zero-I/O
  in-process callbacks now incur a durable write + inbox-dedup tx. This is a **strict
  correctness upgrade** — the old monolith `bus.On` path had no dedup and no atomicity with
  the domain effect; only the split sink did. Rule 7 already permits async/eventual.

**Rejected for Decision 2 (documented):** `EmitTx` also doing post-commit local fanout
(research options A/B/C) — A keeps two producer calls; B needs a commit-hook Go lacks; C
breaks "subscribers see only committed events." Single outbox-driven path beats all three.

**Rule-10 note (the Postgres question, decided as A):** `characters`' domain tx writes
`messaging.outbox`; messaging's per-subscriber `consume` tx writes `messaging.inbox` and runs
the consumer's effect against the consumer's schema. This **bends** rule 10's literal "touch
only your own schema" but not its intent: no cross-module FK, no reading another module's
*domain* rows, the cross-schema write authored by the owner, tx shared as infra (like
`ctx.DB`). One Postgres → one tx spans schemas trivially.

### Scope boundary (explicit)

**In scope** — topics that traverse the durable relay *today*: `character.created`,
`character.deleted` (`characters` → `inventory`, `audit`), `scheduler.fired`
(`scheduler` → `audit`). Migrated producer+consumer onto the durable plane.

**Deliberately deferred (with reason):** `audit` also has generic `Subscribe` +
`POST /events/audit-<slug>` sinks for `player.registered`, `config.changed`,
`match.finished`. Their producers (`accounts`, `config`, `match`) emit **best-effort only**
(`bus.Emit`, no outbox) — nothing POSTs to those sinks in any real topology; they are **dead
routes**. Step 7 **verifies** this (grep `EVENTS_SUBSCRIBERS` in `run.sh`/`run.ps1`/docs — if
any value targets them, deletion is blocked and this becomes a larger change) then deletes
them, keeping audit's *best-effort in-process* logging for those topics. Making them durable
in split needs `accounts`/`config`/`match` to adopt `EmitTx` — separate, larger, out of scope.

**`match.finished` / `leaderboard` clarification `[R7]`:** `leaderboard` is Postgres-backed
(`leaderboard.scores`) and reacts to `match.finished` via best-effort `bus.On`
(`leaderboard.go:44`). It is durable state fed by a lossy event. It is safe **only because
`match.finished` is monolith-only today** (no `cmd` hosts `match` without `leaderboard`) —
*not* because leaderboard "needs no durability." When `match` is ever split, leaderboard must
move to `OnTx`. Recorded so nobody later trusts "leaderboard is fine best-effort."

---

## Steps

### Step 1 — Extend `bus/` with the durable API + transport seam + boot marker `[opus]`

**(a) What.** `bus/bus.go`: add `import "database/sql"`, `"context"`, `"errors"` (all
stdlib — leaf rule intact). Add:
```go
var ErrNoTransport = errors.New("bus: no durable transport installed")

type Transport interface {
    EnqueueTx(tx *sql.Tx, topic string, payload []byte) error          // writes messaging.outbox in caller tx
    SubscribeTx(topic, subscriber string,                              // subscriber = stable name for the (event_id,subscriber) inbox key
        h func(ctx context.Context, tx *sql.Tx, payload []byte) error)
}
func (b *Bus) SetTransport(t Transport)  // panic on double-set (mirror registry.Provide)

func EmitTx[T any](b *Bus, tx *sql.Tx, et EventType[T], v T) error {
    if b.transport == nil { return ErrNoTransport }
    p, err := json.Marshal(v); if err != nil { return err }
    return b.transport.EnqueueTx(tx, et.topic, p)
}
func OnTx[T any](b *Bus, et EventType[T], subscriber string, h func(context.Context, *sql.Tx, T) error)
func OnTxRaw(b *Bus, topic, subscriber string, h func(context.Context, *sql.Tx, json.RawMessage) error)
```
`OnTx`/`OnTxRaw` are no-ops when `transport == nil` (a best-effort-only process is legal).
The generic→bytes collapse happens exactly at `EnqueueTx`/the `SubscribeTx` closure.

**(b) Why now / order.** Everything downstream compiles against this. Purely additive — tree
stays green after this step alone.

**(c) How — the boot check, done for real `[R2]`.** The first draft's "mirror
`validateRequires`" was wrong — `SetTransport` is not a registry service, so
`validateRequires` (name-matches `Requires()`) can't see a missing transport. Fix:
`modules/messaging` (Step 3) **additionally** does `registry.Provide(ctx.Registry,
"messaging", marker)` in `Register`, and durable producers/consumers add `"messaging"` to
their `Requires()` (Steps 4-7). Then a process hosting `characters` without `messaging` fails
**loud at `validateRequires`** (`app.go:305-318`) — the existing mechanism, no new machinery.
`EmitTx`'s `ErrNoTransport` becomes a belt-and-suspenders runtime guard, not the primary net.

**(d) Dispatch:** `[opus]` — core seam, generics→bytes boundary, nil-transport semantics.

### Step 2 — Extend `outbox.Relay`: origin filter, SKIP LOCKED, local targets, per-(topic,url) isolation `[opus]`

**(a) What.** `outbox/relay.go`:
- `NewRelay(db, schema, origin string, subscribers, localTargets []LocalTarget, log)` where
  `LocalTarget = { Subscriber string; Deliver func(ctx, topic string, payload []byte, eventID string) error }`.
- `pending()` → `… WHERE sent_at IS NULL AND origin = $1 ORDER BY id FOR UPDATE SKIP LOCKED`.
- `deliver()`: for each row, deliver to **every** local target (independent, unconditional)
  **and** every remote URL; `markSent` only when all ack. `blocked` keyed **(topic, url)**.

**(b) Why now / order.** `messaging` (Step 3) constructs this and passes its per-subscriber
in-process dispatch as `localTargets`; the relay must support origin + local targets first.

**(c) How.** Stays domain-agnostic (function types, no module import). The "all targets ack →
markSent" gate preserves at-least-once; per-target independence + the consumer inbox make it
effectively exactly-once *per subscriber*. Update `outbox/relay_test.go` +
`relay_prop_test.go`: origin filter, SKIP-LOCKED non-interference between two origins, local
target delivery, markSent only when local+remote both ack, per-(topic,url) block isolation.

**(d) Dispatch:** `[opus]` — the markSent/ordering/origin invariants under partial failure are
the subtle correctness core.

### Step 3 — New `modules/messaging` (owns schema `messaging`, implements `bus.Transport`) `[opus]`

**(a) What.** New `modules/messaging/`. Implements `Module`, `Registrar`, `Migrator`,
`Starter`, `Stopper`, `bus.Transport`.
- **Struct literal / `Register` allocate `localHandlers map[string][]localSub` `[R3]`** — MUST
  be allocated in `Register` (phase 1) or the struct, NEVER `Init`: consumers registered
  *before* messaging call `SubscribeTx` during their phase-2 `Init`, which runs before
  `messaging.Init` (messaging is registered last, Step 8). A map allocated in `Init` = nil-map
  append panic at boot. Guard the map with a mutex.
- **`Register`** (phase 1): `ctx.Bus.SetTransport(m)` **and** `registry.Provide(ctx.Registry,
  "messaging", m)` (the boot marker, `[R2]`). Both in phase 1 so every consumer's phase-2
  `OnTx` sees the transport and `validateRequires` sees the service.
- **`Migrate`**: `CREATE SCHEMA IF NOT EXISTS messaging;` +
  `messaging.outbox(id bigserial, origin text NOT NULL, topic text, payload jsonb, created_at,
  sent_at)` + partial index `(id) WHERE sent_at IS NULL` +
  `messaging.inbox(event_id text, subscriber text, processed_at timestamptz default now(),
  PRIMARY KEY(event_id, subscriber))` + `AFTER INSERT ON messaging.outbox` trigger →
  `pg_notify('messaging_outbox', NEW.topic)`.
- **`EnqueueTx`**: `INSERT INTO messaging.outbox(origin,topic,payload) VALUES($1,$2,$3::jsonb)`
  on the caller's tx, origin = `MESSAGING_ORIGIN`.
- **`SubscribeTx(topic, subscriber, h)`**: append `{subscriber,h}` to `localHandlers[topic]`
  under the mutex.
- **`Init`**: `m.db = ctx.DB`; read `MESSAGING_ORIGIN` + `EVENTS_SUBSCRIBERS`
  (`outbox.ParseSubscribers`); build one `localTargets` list (one per (topic,subscriber)
  each wrapping `m.consume`); construct the single `m.relay = outbox.NewRelay(ctx.DB,
  "messaging", origin, subs, localTargets, log)`; register the single inbound sink
  `ctx.Mux.HandleFunc("POST /events", m.handleInbound)`.
- **`consume(ctx, subscriber, eventID, topic, payload)`** (used by both a local target and
  `handleInbound`): `BeginTx` → `INSERT messaging.inbox(event_id,subscriber) ON CONFLICT DO
  NOTHING` → if new, invoke that ONE subscriber's handler with `tx` → `Commit`. **Per
  subscriber, own tx** (`[R6]`): independent fate.
- **`handleInbound`**: read `X-Event-Topic`+`X-Event-Id`; for each local subscriber of that
  topic, `m.consume(...)`. (This is the receiver side in a consuming peer.)
- **`Start`/`Stop`**: run/halt relay + `messaging_outbox` LISTEN loop + a **housekeeping
  ticker** (see retention below), cancellable ctx + `done`, config's pattern. `Stop` must
  **wait for in-flight `consume` to finish** before returning (`[R3]` shutdown safety), not
  just cancel.
- **Housekeeping / retention `[R10]` (user chose: build now):** the ticker (default hourly,
  `MESSAGING_HOUSEKEEP_INTERVAL`) prunes both ledgers past a window (default 168h,
  `MESSAGING_RETENTION`): `DELETE FROM messaging.inbox WHERE processed_at < now()-$win` and
  `DELETE FROM messaging.outbox WHERE sent_at IS NOT NULL AND sent_at < now()-$win`. Bounded
  batch (`… LIMIT n`) per tick to avoid long locks. Self-owned by messaging — no coupling to
  scheduler.

**(b) Why now / order.** Provides the API runtime; Steps 4-7 need it present.

**(c) How — noted risks.** Inbound consolidation to one `POST /events` removes receiver-side
route sprawl; sender isolation is preserved by the per-(topic,url) block gate (Step 2), so
`[R5]` is addressed without per-topic routes. Retention is now built here (housekeeping
ticker above), not deferred.

**(d) Dispatch:** `[opus]` — new module, `consume`/dedup correctness, lifecycle ordering,
transport impl, LISTEN/NOTIFY loop, shutdown drain.

### Step 4 — Migrate producer `characters` onto `EmitTx` `[sonnet]`

**(a) What.** `modules/characters/characters.go` + `store.go`:
- **`Requires()`**: add `"messaging"` (`[R2]` boot check).
- **Delete:** `characters.outbox` DDL (`schemaDDL:66-75`); `store.insertOutbox`
  (`store.go:54-61`); `m.relay` field + `outbox.NewRelay` (`:111-112`); relay `Start`/`Stop`
  (`:127-140`) — and the whole `Start`/`Stop` methods (relay was their only reason →
  `characters` drops `Starter`/`Stopper`); post-commit `bus.Emit` (`:259,321`); the
  now-dead `payload, err := json.Marshal(evt)` blocks (`:240-245,304-309`) `[R8]` since
  `EmitTx` marshals internally; unused `outbox`/`os` imports.
- **Change:** in `handleCreate`/`handleDelete`, before `tx.Commit()`, call
  `bus.EmitTx(m.bus, tx, charactersevents.CreatedEvent, evt)` (resp. `DeletedEvent`), with
  its error routed to the existing rollback path.

**(b) Why now / order.** Depends on Steps 1+3. Pairs with Step 6 at runtime (once messaging
wired in Step 8).

**(c) How.** Deletion list is now complete incl. the dead marshal (`[R8]`). The one judgment
(drop `Starter`/`Stopper`) is decided: **yes**. Confirm `go build` + `characters` tests.

**(d) Dispatch:** `[sonnet]` — fully-specified deletion + one-line swap; no new design.

### Step 5 — Migrate producer `scheduler` onto `EmitTx` `[sonnet]`

**(a) What.** `modules/scheduler/scheduler.go`: `Requires()` += `"messaging"`; delete
`scheduler.outbox` DDL (`:73-82`), `m.relay` field + construction (`:118-119`), relay
`Start`/`Stop` lines (`:129-133,159-161`), and the now-dead inline `json.Marshal` for the
outbox payload in `fire`; in `fire` (`:275-306`) replace the inline `INSERT INTO
scheduler.outbox` + post-commit `bus.Emit` with `bus.EmitTx(m.bus, tx,
schedulerevents.FiredEvent, …)` before commit. **Keep** `Start`/`Stop` (scheduler owns its
emission loop) — only remove the relay lines.

**(b) Why now / order.** Depends on Steps 1+3; independent of Step 4 (different files).

**(c) How.** `fire` uses a session-scoped `*sql.Conn` tx for the advisory lock; `EmitTx` takes
that same tx — locking unchanged.

**(d) Dispatch:** `[sonnet]` — mechanical, fully specified.

### Step 6 — Migrate consumer `inventory` onto `OnTx` `[opus]`

**(a) What.** `modules/inventory/inventory.go`:
- **`Requires()`**: add `"messaging"`.
- **Delete:** the two `POST /events/*` handlers + registrations (`:158-159`); `consume`
  (`:306-330`); `inventory.inbox` DDL (`:109-115`); the two best-effort
  `bus.On(charactersevents.*)` (`:142-143`) + `onCharacterCreated`/`onCharacterDeleted`
  shells (`:245-256`).
- **Add:** `bus.OnTx(ctx.Bus, charactersevents.CreatedEvent, "inventory", func(ctx,tx,e) error
  { return m.grantStarter(ctx, tx, e.CharacterID) })` and the `DeletedEvent`→`wipeCharacter`
  twin. Effect funcs unchanged (`rowQuerier` already takes `*sql.Tx`).
- **Keep:** `bus.On(configevents.ChangedEvent, m.onConfigChanged)` — best-effort, no persist,
  stays on the best-effort plane.

**(b) Why now / order.** Depends on Steps 1+3. Pairs with Step 4.

**(c) How — correctness.** Effect now always runs inside messaging's per-subscriber `consume`
tx with `(event_id,"inventory")` dedup, both topologies → exactly-once-transactional. Call
`grantStarter`/`wipeCharacter` only with the handed `tx`, never `m.store.db`. Inventory drops
its own inbox (moved to `messaging.inbox`).

**(d) Dispatch:** `[opus]` — handler-signature change + effect atomicity.

### Step 7 — Migrate consumer `audit`; delete verified-dead sinks `[opus]`

**(a) What.** `modules/audit/audit.go`:
- **`Requires()`**: add `"messaging"`.
- **`scheduler.fired`:** replace typed `bus.On(schedulerevents.FiredEvent,…)` +
  `POST /events/scheduler-fired` + `consume` with `bus.OnTx(ctx.Bus,
  schedulerevents.FiredEvent, "audit", func(ctx,tx,f) error { return m.prune(ctx, tx) })`.
- **Relayed domain topics** (`character.created`, `character.deleted`): replace
  `Subscribe`+`POST /events/audit-<slug>`+`consume` with `bus.OnTxRaw(ctx.Bus, topic,
  "audit", func(ctx,tx,raw) error { return m.record(ctx, tx, topic, raw) })` (untyped — audit
  keeps not importing producers' `*events`).
- **Dead sinks** (`player.registered`, `config.changed`, `match.finished`): **verify first**
  (grep `EVENTS_SUBSCRIBERS` in `run.sh`/`run.ps1`/docs — no value targets `audit-*` for
  these). If clean, delete those `POST /events/audit-*` routes; keep best-effort in-process
  logging via `ctx.Bus.Subscribe(topic, …)`. If NOT clean → stop, escalate (scope grows).
- **`domainTopics` split `[R9]`:** split the single `domainTopics` set into `durableTopics`
  (`character.created`, `character.deleted`) and `bestEffortTopics` (the three dead-sink
  topics); update the `audit_test.go` anti-drift assertion to cover both sets explicitly.
- **Delete:** `audit.inbox` DDL + `audit.consume`.

**(b) Why now / order.** Depends on Steps 1+3+5. Pairs with Step 5.

**(c) How.** `m.record` → `(ctx, tx, topic, raw)`, ledger insert on the handed tx (atomic with
`(event_id,"audit")` dedup). Dead-sink deletion carries its verification evidence in the
commit body.

**(d) Dispatch:** `[opus]` — untyped variant + ledger atomicity + dead-route verification +
the fate-decoupling (audit failure must not touch inventory — guaranteed by per-subscriber tx,
Step 3).

### Step 8 — Wire `messaging` into every hosting process + enforcement `[sonnet]`

**(a) What.**
- Register `&messaging.Module{}` **last** in `mods` for `cmd/server`, `cmd/characters-svc`,
  `cmd/inventory-svc`, `cmd/scheduler-svc`. `cmd/gateway-svc` untouched (no `app.Run`).
- `.go-arch-lint.yml`: `messaging: { in: modules/messaging }`;
  `messaging: { mayDependOn: [ lifecycle, contracts, bus, outbox ] }`; add `messaging` to
  `cmdServer`/`cmdCharactersSvc`/`cmdInventorySvc`/`cmdSchedulerSvc` `mayDependOn`.
  **Invariant:** `bus`/`outbox` gain NO `deps` toward `messaging` (leaf rule #1); consumers
  reach messaging only via `ctx.Bus` + the `"messaging"` registry marker — no module imports
  `modules/messaging`.
- `internal/app/app.go:200-221`: update shutdown-order comment to name `messaging` (relay
  stops in step 4 / reverse order, and `messaging.Stop` drains in-flight `consume`, `[R3]`).
- `run.sh`/`run.ps1`: set `MESSAGING_ORIGIN` per process; update `EVENTS_SUBSCRIBERS` to
  `topic=<peerBaseURL>` pointing at the single `/events` route.

**(b) Why now / order.** After module + all producers/consumers exist; makes it live.

**(c) How — registration-position decision `[R3]`.** Transport *install* is
position-independent (Register phase 1 precedes all Init — proven, `app.go:44-60`), which is
also why the `localHandlers` map MUST be allocated in `Register` not `Init` (Step 3). Register
`messaging` **last** so reverse-order `Stop` halts delivery **first** (before consumer modules
tear down); `messaging.Stop` additionally waits for in-flight `consume`. Document at each
registration line.

**(d) Dispatch:** `[sonnet]` — mechanical wiring + config; the judgment (position) pre-decided.

### Step 9 — Tests + verify net `[opus]` (new correctness) / `[sonnet]` (mechanical) `[R4]`

**(a) What.**
- **New:** `modules/messaging/*_test.go` — `EnqueueTx` write+origin stamp; NOTIFY-driven
  drain; two-origins-don't-drain-each-other (BLOCKER-1 regression test — the load-bearing
  one); per-subscriber `consume` dedup (deliver twice → effect once); **fate isolation** (one
  subscriber's handler error does not roll back another's, `[R6]`); `handleInbound` dedup;
  `Register`-allocates-map-and-installs-transport-before-any-`Init` (`[R3]`).
  `internal/app/app_test.go` — messaging in Build/Migrate/Start/Stop + `validateRequires`
  fails a producer-without-messaging process (`[R2]`).
- **Update:** `outbox/relay_test.go` + `relay_prop_test.go` (origin, SKIP-LOCKED, local
  targets, per-(topic,url) block); `characters_test.go` + `wire_contract_test.go`,
  `scheduler_test.go` (relay gone → assert `EmitTx`); `inventory_test.go`, `audit_test.go`
  (inbox/consume moved to messaging; `domainTopics` split → `durable`/`bestEffort`, `[R9]`).
- **Housekeeping test:** insert an inbox row + a sent outbox row past the retention window,
  tick, assert both pruned; a fresh row survives; batch `LIMIT` respected.
- **topiccheck `[R4]`:** teach `tools/topiccheck` to recognize `bus.OnTx` **and**
  `bus.OnTxRaw` (and note `scheduler.fired` moved off `bus.On`), else `character.created`,
  `character.deleted`, `scheduler.fired` go red. Both new subscribe funcs must be detected,
  not just `OnTxRaw`.
- **Verify the at-risk path (memory `verify-the-at-risk-path`):** run **split** —
  `characters-svc` + `inventory-svc` (+ their `MESSAGING_ORIGIN`/`EVENTS_SUBSCRIBERS`), create
  a character, assert the starter grant lands in inventory via relayed `character.created`,
  delete it, assert holdings wiped. **Also** run `scheduler-svc` alongside to prove its relay
  does NOT eat `characters-svc`'s events (the exact BLOCKER-1 scenario). Capture committed
  evidence. Then `verify.ps1 --all`.

**(b) Why now / order.** Last — validates the whole change; `--all` runs topiccheck (now
taught), apidiff (no `*events` payload mutated — clean; messaging adds no published contract),
go-arch-lint (Step 8 rules).

**(c) How.** The split live-verify **with scheduler-svc present** is the load-bearing proof;
a monolith-only or two-process pass would hide the multi-relay race the review caught.

**(d) Dispatch:** `[opus]` for messaging correctness tests + split live-verify; `[sonnet]` for
mechanical test-file updates.

---

## Dispatch summary (for approval at ExitPlanMode)

| Step | Work | Lane |
|---|---|---|
| 1 | `bus/` durable API + transport seam + boot marker | `[opus]` |
| 2 | `outbox.Relay` origin/SKIP-LOCKED/local-targets/isolation | `[opus]` |
| 3 | new `modules/messaging` | `[opus]` |
| 4 | `characters` → `EmitTx` (+`Requires` messaging) | `[sonnet]` |
| 5 | `scheduler` → `EmitTx` (+`Requires` messaging) | `[sonnet]` |
| 6 | `inventory` → `OnTx` | `[opus]` |
| 7 | `audit` → `OnTx`/`OnTxRaw` + verified dead-sink cleanup | `[opus]` |
| 8 | wiring + arch-lint + shutdown comment + run scripts | `[sonnet]` |
| 9 | tests (+BLOCKER-1 regression) + 3-process split verify + `verify.ps1 --all` | `[opus]`/`[sonnet]` |

Trailers: `[opus]`→`Claude Opus 4.8`, `[sonnet]`→`Claude Sonnet 4.6`. Commit after each step
(Conventional Commits, scope = touched module(s), note `(Step N — …)`).

## Review findings — disposition

All think-hard review findings folded in: **BLOCKER 1** → Relay ownership model (origin +
SKIP LOCKED + unconditional local + single-owner drain). **BLOCKER 2** → registry marker +
`Requires("messaging")` boot check (Step 1c/3/4-7). **MAJOR 3** → map in `Register` + `Stop`
drains in-flight (Step 3/8). **MAJOR 4** → topiccheck teaches `OnTx`+`OnTxRaw` (Step 9).
**MAJOR 5** → per-(topic,url) block gate (Step 2). **MAJOR 6** → per-subscriber tx + inbox PK
`(event_id,subscriber)` (Step 3). **MINOR 7/8/9/10/11** → leaderboard rationale fixed / dead
marshal deletion listed / `domainTopics` split / inbox retention noted / NOTIFY cost noted.

## Open questions — RESOLVED with user (2026-07-07)

1. **`origin`:** new env `MESSAGING_ORIGIN`, defaulted per `cmd`, stable across restarts. ✓
2. **Inbound route:** single `POST /events` + `X-Event-Topic` header; sender isolation via the
   per-(topic,url) block gate. ✓
3. **`messaging.inbox` retention:** **build pruning now** — housekeeping ticker in messaging
   prunes inbox + sent outbox past a retention window (Step 3). ✓

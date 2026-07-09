# AnyTx: engine-neutral durable-events seam (bus drops sqlx)

**Date:** 2026-07-09 14:22
**Status:** reviewed (grumpy punch list 2026-07-09 addressed in place; reviewer
COMPILED the Target API + all call-site shapes in a scratch crate against sqlx
0.8.6 — the type-system bet is proven, not assumed)
**Decided with user:** a module knowing its OWN store engine is fine; a module must
NOT learn from the events API that the plane stores events in Postgres. The
backplane (`core/asyncevents`) may stay Postgres-typed. A module whose store is
non-Postgres (e.g. Mongo) must be able to consume events with exactly-once
*effects in its own store* — which requires exposing the delivery `event_id`.

## Context

### The leak (named precisely)

`core/bus` — a foundation leaf — depends on sqlx solely because four signatures
name `&mut sqlx::PgConnection`: `Bus::emit_tx`, `Transport::enqueue_tx`,
`TxHandler::call`, and the `on_tx` closure bound. The abstraction promises
"durable events"; the types say "durable events atomic with a Postgres
transaction". Producer-side, a non-Postgres-store module cannot even call
`emit_tx` (nothing to pass) — and, honestly stated, this plan removes the
*signature* obstacle, not the physics: with the only production transport being
Postgres, a foreign-store producer's every emit would fail `TxEngineMismatch`
until an outbox in ITS engine exists (a second Transport impl — out of scope,
per the user's decision that the backplane may stay Postgres). Consumer-side
the leak is subtler: the handler
receives the plane's delivery tx, and the exactly-once-effects guarantee is
*contingent on the handler writing through it* — nothing enforces that today
(verified: the crash-window for a foreign-store effect already exists; the
current wording just doesn't admit it).

### The physics that shapes the fix (research-verified, 6 subagents, 2026-07-09)

- **Exactly-once effects = dedup-check and effect atomic in ONE store.** Today
  that store is the shared Postgres (inbox row + module effect in one tx). For a
  foreign-store consumer the equivalent is an idempotent upsert keyed by the
  delivery id in ITS store. The plane cannot provide that atomicity across
  engines — it can only hand over (a) a stable `event_id` and (b) its own
  delivery tx for engine-matched consumers.
- **`event_id` already exists and is one variable away from the handler.**
  Minted once (`outbox::deliver_with`: `"{schema}:{row.id}"`,
  core/outbox/src/lib.rs:304), rides both paths (LocalTarget arg / `X-Event-Id`
  header), lands in `Inner::consume` (core/asyncevents/src/lib.rs:161) where it
  keys the inbox INSERT — and is lexically in scope at the `handler.call` line
  but not passed. No format change; no migration needed.
- **Erasure is mechanically sound.** `PgConnection` is an owned,
  lifetime-parameter-free, `Send` struct (sqlx-postgres 0.8.6) → `Any` holds; the
  `&mut *tx` deref-reborrow every call site already performs yields exactly the
  `&mut PgConnection` to erase. `Transaction<'c, _>` itself is NOT `'static`, so
  erasure MUST happen after the deref — call sites go from implicit coercion to
  an explicit wrap. `bus::Error` is already sqlx-free; the in-process plane
  already uses `Arc<dyn Any>` erasure in the same file (precedent, not a new
  technique). async_trait threads a `AnyTx<'_>` parameter identically to today's
  `&mut PgConnection`; no `'static` bound bites (handler futures are awaited
  inline in `consume`, never spawned — must stay that way).
- **Blast radius is small and enumerated:** 6 producer emit sites (5 modules;
  accounts passes `&mut Transaction` one level up → `&mut **tx`), 8 consumer
  subscription call sites (audit's `on_tx_raw` loop covers 6 topics from one
  site; 3 handlers ignore the conn entirely: inventory:665, rating, config-cache),
  1 production `TxHandler::call` invocation (`Inner::consume` — the single
  linchpin), 4 trivial test transports (`_conn` unused in all), 2 audit
  `TxHandler` impls, the `_tx_threading_type_checks` compile-proof, and the
  FOUR BLOCKER-1 rustdoc blocks (emit_tx, on_tx, subscribe_tx, TxHandler).
  `core/outbox` is untouched (its `DeliverFn` seam already passes `event_id` and
  never sees a connection).

### Why this shape and not the alternatives

- **Why not generics (`Bus<Tx>` / `Transport<Tx>`)**: the generic would infect
  `Context`, every stored handler, and the object-safe `dyn Transport` — the
  exact BLOCKER-1 wall that produced the concrete typing. Runtime-checked
  erasure with a loud mismatch error moves engine-matching to the composition
  root, where topology already lives.
- **Why not consumer-side "plane writes to Mongo"**: the plane can't own foreign
  stores; the correct generalization is dedup-with-the-effect in the consumer's
  store, enabled by `event_id`. The plane's inbox stays as the redelivery gate
  and the dedup for engine-matched consumers.
- **Why expose `event_id` as part of a `Delivery` struct, not a bare arg**: one
  more field (e.g. producer `origin`, which consumers cannot see today) can be
  added later without re-breaking every handler signature.

## Target API (all in `core/bus`, sqlx dep DELETED)

```rust
/// A type-erased mutable borrow of the caller's transactional context.
/// Producer side: YOUR domain tx. Consumer side: the plane's delivery tx.
/// The events seam never names an engine; the party that owns the concrete
/// store downcasts (its own engine is its own business).
/// Second field: the concrete type's name, captured at construction — a
/// `&mut dyn Any` alone yields only a TypeId at downcast time, so the mismatch
/// error could not name what it GOT without this (reviewer MAJOR #1).
pub struct AnyTx<'a>(&'a mut (dyn Any + Send), &'static str);

impl<'a> AnyTx<'a> {
    pub fn new<T: Any + Send>(tx: &'a mut T) -> AnyTx<'a>;  // stores type_name::<T>()
    /// The concrete transaction, or Error::TxEngineMismatch naming both types.
    /// NOTE: callers need a `mut` binding (`mut delivery` / `mut tx`).
    pub fn downcast<T: Any>(&mut self) -> Result<&mut T, Error>;
}

/// What a durable handler receives per delivery.
pub struct Delivery<'a> {
    /// Stable across redeliveries ("{schema}:{outbox_id}") — the idempotency key
    /// for effects in a store the plane's tx cannot reach.
    pub event_id: &'a str,
    /// The plane's delivery transaction. Downcast it in your store layer if your
    /// store shares the plane's engine; ignore it otherwise.
    pub tx: AnyTx<'a>,
}

pub async fn emit_tx<T: Serialize>(&self, tx: AnyTx<'_>, et: &EventType<T>, v: &T) -> Result<(), Error>;
pub fn on_tx<T, F>(&self, et: &EventType<T>, subscriber: &str, handler: F)
where F: for<'a> Fn(Delivery<'a>, T) -> BoxFuture<'a, Result<(), Error>> + Send + Sync + 'static;

pub trait Transport: Send + Sync {
    async fn enqueue_tx(&self, tx: AnyTx<'_>, topic: &str, payload: &[u8]) -> Result<(), Error>;
    fn subscribe_tx(&self, topic: &str, subscriber: &str, handler: Arc<dyn TxHandler>);
}
pub trait TxHandler: Send + Sync {
    fn call<'a>(&'a self, delivery: Delivery<'a>, payload: Vec<u8>) -> BoxFuture<'a, Result<(), Error>>;
}

// Error gains one engine-neutral variant:
Error::TxEngineMismatch { expected: &'static str, got: &'static str /* type_name-based */ }
```

The generalized contract, one sentence (goes into the rustdoc + README):
*delivery is at-least-once with a stable `event_id`; effects are exactly-once
iff the dedup-check and the effect are atomic in the consumer's own store — via
the handed delivery tx when engines match, via an idempotent `event_id`-keyed
write otherwise.*

## Steps

### Step 1 — `core/bus`: AnyTx + Delivery, sqlx dep deleted `[fable]`

**(a)** core/bus/src/lib.rs, src/tests.rs, Cargo.toml.
**(b)** Everything else compiles against these types; first by necessity.
**(c)**
- Add `AnyTx` (two-field struct per Target API — type name captured in `new`,
  `downcast` errors with `TxEngineMismatch { expected, got }`) and `Delivery`.
  Rewrite the four signatures per the Target API. `TypedAdapter` decodes then
  calls `(self.handler)(delivery, v)`. Remove the sqlx Cargo dep (sqlx appears
  only as fully-qualified paths in the four signatures — there is no `use sqlx`
  line); `Error` gains `TxEngineMismatch` (everything else already sqlx-free).
- Update `_tx_threading_type_checks` FIRST — it is the compile-proof pinning the
  new HRTB shape (`for<'a> Fn(Delivery<'a>, T) -> BoxFuture<'a, _>` with the
  future borrowing the delivery). With the sqlx dev-dep gone the compile-proof
  cannot fabricate a `PgConnection` — use any `Any + Send` local (`u32` suffices;
  the reviewer's scratch model proves the HRTB pins identically). Then the other
  durable-plane tests (`Raw`/`H` TxHandler impls, `on_tx` closures,
  FakeTransport) — the fakes' `_conn` is unused, signature-only edits; add one
  NEW unit test: `downcast` success with a concrete type + mismatch produces
  `TxEngineMismatch` naming both types.
- Rewrite the three BLOCKER-1 rustdoc blocks: the named-trait rationale stands
  (unchanged); the "takes `&mut PgConnection`, not `&mut Transaction`" paragraphs
  become "takes `AnyTx`, constructed from the deref of your concrete tx
  (`AnyTx::new(&mut *tx)`) — erasure after the deref, because `Transaction<'c>`
  is not `'static`; the wrapper borrows only for the call, never past your
  commit". State the generalized contract on `on_tx`/`TxHandler`.
- Constraint carried forward as a doc note on `TxHandler`: handler futures are
  awaited inline by the plane (never spawned); a future implementation must not
  require `'static` of the delivery borrow.

### Step 2 — `core/asyncevents`: downcast point + event_id threading `[fable]`

**(a)** core/asyncevents/src/lib.rs, src/tests.rs.
**(b)** The single production Transport/consume implementation; unblocks all
call-site sweeps.
**(c)**
- `Inner::enqueue_tx(mut tx: AnyTx<'_>, ...)`: `let conn = tx.downcast::<sqlx::PgConnection>()?;`
  then the existing INSERT — this is THE producer-side engine gate; the mismatch
  surfaces at the FIRST EMIT (which can be arbitrarily post-boot — a
  request-path emit fails on first use, not at startup; say so in the rustdoc).
- `Inner::consume`: construct `Delivery { event_id, tx: AnyTx::new(&mut *tx) }`
  at the existing `handler.call` line (event_id is already a local); dedup INSERT
  and commit logic byte-identical. `handle_inbound`/`build_local_targets`
  unchanged (they pass event_id into consume already).
- Crate tests: `RecordHandler::call` new signature (may now assert the received
  `event_id` equals the one passed to `consume` — cheap strengthening);
  `enqueue_tx_writes_row_with_origin`/`relay_drains_only_its_own_origin` wrap
  their `&mut tx` in `AnyTx::new(&mut *tx)`; `plane_transport_is_live_...`
  closure signature.
- Rustdoc: `enqueue_tx` ("inside the PRODUCER's domain tx") and `consume` ("same
  connection/tx") reworded to the generalized contract; note that for an
  engine-matched consumer nothing changed semantically.

### Step 3 — producers: 6 emit sites wrap explicitly `[sonnet]`

**(a)** modules/accounts/src/lib.rs:174 (fn takes `tx: &mut Transaction` →
`AnyTx::new(&mut **tx)`), modules/characters/src/lib.rs:219,262,
modules/match/src/lib.rs:109, modules/config/src/lib.rs:271,
modules/scheduler/src/lib.rs:177 (all local `tx: Transaction` →
`AnyTx::new(&mut *tx)`), plus the test emit in modules/inventory/src/tests.rs:274.
**(b)** Mechanical; requires only Step 1's type.
**(c)** Replace each `emit_tx(&mut tx, ...)` with `emit_tx(AnyTx::new(&mut *tx), ...)`
(accounts: `&mut **tx`). The scheduler site's borrow must still end at
`tx.commit()` so `fire` can reuse the locked connection for the advisory unlock
— the wrapper borrows only for the call, so no change, but the executing agent
verifies it compiles with the unlock path intact. Update the three inline
"coerces via DerefMut" comments (config/scheduler/characters) — the coercion is
now an explicit wrap.

### Step 4 — consumers: 9 handlers to `Delivery` `[opus]`

**(a)** modules/inventory/src/lib.rs:638,643,665; modules/rating/src/lib.rs:149;
modules/leaderboard/src/lib.rs:155 (+ `record_win`);
modules/audit/src/lib.rs:91-152 (RecordHandler/PruneHandler);
api/config/rpc/src/lib.rs:147.
**(b)** After Steps 1–2 the closure bound forces these; the downcast-placement
decision is the one judgment call.
**(c)** Downcast policy: **once, at the top of the handler, in the module's own
code** — store methods keep their `&mut PgConnection` signatures (they are the
module's private Postgres layer; renaming their params buys nothing):
- inventory CREATED/DELETED: closure takes `mut delivery`, then
  `let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;` and
  `grant_starter(conn, ...)` / `wipe_character(conn, ...)` unchanged.
- leaderboard: same (`mut delivery`), then `record_win(conn, ...)` unchanged.
- audit RecordHandler/PruneHandler (`TxHandler` impls): `call` takes
  `mut delivery: Delivery<'a>`, downcast inline at the top, SQL unchanged.
- rating / inventory-config-watch / config-cache stub: already ignore the conn —
  accept `Delivery` and ignore both fields (`let _ = delivery;`).
- New imports are limited to `bus::{AnyTx, Delivery}` where used (producers add
  `AnyTx`, consumers `Delivery`); the downcast target `sqlx::PgConnection` is
  already in each module's imports (its own store).

### Step 5 — test transports, checkers, workspace green `[opus]`

**(a)** tools/requirecheck/src/main.rs (NoopTransport), tools/topiccheck/src/main.rs
(RecordingTransport), modules/scheduler/src/tests.rs (FakeTransport +
`bus_with_fake`), core/bus/src/tests.rs follow-through if Step 1 left anything,
modules/*/src/tests.rs compile fixes (inventory round-trip test's emit already
in Step 3).
**(b)** Last compile holdouts; gate before verify.
**(c)** All four fakes: signature-only edits (`_tx: AnyTx<'_>`). Then:
`cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings
&& cargo test --workspace`, plus `cargo run -p archcheck`,
`requirecheck -- --strict`, `topiccheck -- --strict` — all green.

### Step 6 — audit ledger gains `event_id` (droppable) `[sonnet]`

**(a)** modules/audit/src/lib.rs (SCHEMA_DDL + RecordHandler INSERT + tests).
**(b)** First real consumer of `event_id`; proves the API and gives the ledger a
durable cross-reference to the inbox. Additive DDL
(`ALTER TABLE audit.log ADD COLUMN IF NOT EXISTS event_id text`), INSERT gains
the column from `delivery.event_id`. If the reviewer or user judges it scope
creep, drop the step — nothing depends on it.

### Step 7 — docs + memory `[opus]`

**(a)** core/asyncevents/README.md, CLAUDE.md, memory.
**(b)** After code settles.
**(c)**
- README: reword the exactly-once sentences (lines ~45-53, ~92-94) to the
  generalized contract; add a short "non-Postgres consumer" paragraph (idempotent
  event_id-keyed effect); note the producer-side gate (`TxEngineMismatch`).
- CLAUDE.md seam #3: "consumer `on_tx`/`on_tx_raw` with a subscriber name" gains
  the one-line generalized contract; constraint 9 wording check. No literal
  handler signatures exist in CLAUDE.md (verified) — small surgical edits.
- Memory: update `durable-event-plane-bus-owned.md` (AnyTx/Delivery, bus is
  sqlx-free, contract wording) + MEMORY.md line; memory-sync push.

### Step 8 — verify `[inline]`

`./verify.ps1` full (split-proof exercises live cross-process delivery + dedup +
leaderboard exactly-once with the new seam). Trailer audit. Commits: Steps 1–5
one stack commit (workspace compiles only after 5), Step 6 and 7 separate.

## Risks / decisions

- **Runtime engine check replaces compile-time typing.** Deliberate: the generic
  alternative infects Context + every stored handler (BLOCKER-1). The mismatch
  is a composition-root bug, surfaces at boot/first-emit, loud, names both types.
- **Semantics for existing modules: zero change.** Same tx, same dedup, same
  ordering (ordering is entirely pre-handler in outbox — verified untouched).
  The exactly-once wording becomes *honest* (it was always contingent on the
  handler writing through the handed tx); no behavior moves.
- **`event_id` format unchanged** ("{schema}:{row.id}") — no inbox migration;
  smoke-script/tests that parse the format stay valid.
- **`origin` is NOT exposed** to consumers (they can't see who produced an event
  today; `Delivery` makes adding it later non-breaking). Out of scope.
- **outbox crate untouched** — its DeliverFn seam already carries event_id and
  never sees a connection.

# Account deletion and the `player.deleted` purge fan-out

**Status:** revision 2, after adversarial review (16 findings, 8 blocking). The
review response is at the end.
**Prerequisite for:** the `storage` fortress (P1#7), which needs a live producer for
its own purge subscription rather than shipping a consumer with none — the shape
`mail` already shipped once and still carries as a known gap.

Tracker row: `docs/roadmap/feature-tracker.md:141` — *Account self-delete + GDPR
export ❌ accounts — "Only server-side prune today."*

---

## Context — the overlapping systems, and why a new op rather than an extension

This is **not** a new fortress: one new `#[http]` op pair, one new durable topic,
seven purge subscriptions in existing modules, six scheduler-driven sweeps.

- **`accounts`' scheduler prune** (`modules/accounts/src/lib.rs:936-965`) deletes only
  `expires_at <= now()` rows. **Why not extend it:** a `scheduler.fired` handler
  cannot be caused by a player's request and carries no subject.
- **`accounts`' `ON DELETE CASCADE`** (`modules/accounts/src/lib.rs:127`, `:136`,
  `:151`) tears down identities, sessions and refresh tokens. **Why not rely on it
  alone:** cascade is confined to schema `accounts`; constraint #10 forbids
  cross-module FKs, so six other schemas hold `player_id` columns nothing collects.
- **`inventory.wiped_characters`** (`modules/inventory/src/lib.rs:83-92`) is the
  precedent for a tombstone that outlives a delete and guards a sibling handler.
  **Why not reuse it:** keyed on `character_id`, owned by inventory.
- **The retention sweeps** (`modules/mail/src/projection.rs:106-207`,
  `modules/groups/src/projection.rs:44-181`) are the batching precedent this plan
  copies. **Why not model the purge as retention:** retention is age-driven and
  repeating; deletion is subject-driven and fires once.

---

## Findings that shape the design

**F1 — a purge must be resumable across fires, budget-bounded and monotone.**
Not "it runs outside a delivery transaction" — the mail sweep this plan copies *is*
a delivery-transaction handler under the same bound. What saves it is
`PRUNE_BUDGET = 5s` inside a 10s `ASYNCEVENTS_HANDLER_TIMEOUT`
(`core/asyncevents/src/worker.rs:31`) plus a batch/watermark pattern that **commits
partial progress with the checkpoint** and resumes on the next fire. A `player.deleted`
handler cannot resume — it fires once — so the deleting must move to a repeating
trigger. Nothing bounds the work: `MAX_MEMBERS` caps a group at 500, nothing caps
groups-per-player, accepted friend edges are unbounded.

**F2 — one paused consumer strands a player half-deleted, `/readyz` green.**
The liveness stamp is set by any delivery on any subscription
(`core/asyncevents/src/worker.rs:493`, `:522`), and the `state = 'active'` predicate
(`:205`) simply skips a paused one. No `ReadyCheck` reads `state = 'paused'`.
`eventctl skip` abandons that module's rows permanently; not skipping stalls every
subsequent player behind the same cursor.

**F3 — the sweep must be total.** `groups`' last-admin rule answers `Conflict`
(`modules/groups/src/service.rs:586-593`). A `Conflict` from a durable handler is an
`Err`: backoff, then pause. A domain-invariant conflict must be *resolved* inside the
sweep, never reported. (This risk lives in the sweep, not in the Step 4 handler,
which only inserts a tombstone.)

**F4 — session revocation in the producing transaction closes the resurrection race.**
A live access token is valid up to 60 minutes (`modules/accounts/src/store.rs:12`) and
`verify_session` is uncached (`modules/gateway/src/verifier.rs:104-121`). `accounts`
owns both the session table and the append, so deleting sessions in the same
transaction as `emit_tx` puts every consumer strictly after revocation is globally
visible, by the eligibility clause (`core/asyncevents/src/worker.rs:232-237`).
Step 6's write guard therefore covers only the residue: a request already past
verification, and any server-to-server write.

**F5 — no gate proves a specific consumer subscribes.** `unsubscribed()` is keyed on
`(topic, version)` (`tools/topiccheck/src/main.rs:340-341`) — **existence-level, not
consumer-level**. With seven consumers, six can be missing and both profiles stay
green. `split-proof` is the only instrument that can catch it and its content is
hand-written, so Step 9's per-consumer cursor assertions are the only proof this
rollout has.

---

## Decisions settled here

| # | Decision | Rationale |
|--:|---|---|
| D1 | **Hard delete**, no `deleted_at` | `kill_family_tx`'s "deletion, not a flag" (`modules/accounts/src/store.rs:575-579`) and the FK cascade. Soft delete would need four read predicates amended and a redefinition of what `Directory` returns. |
| D2 | **Tombstone in the delivery tx, budgeted sweep on a repeating trigger** | F1. |
| D3 | **Required server-minted ticket**, `#[retry_safe]` on `delete_account` only | See the two-leg note below. |
| D4 | **Handle reserved permanently** | Owner's decision. Closes impersonation-by-recycling against `friends`' denormalized handles. Cost: one discriminator per deleted name, forever, in a new permanent table. |
| D5 | **`player.deleted` carries the final handle** | Owner's decision. Makes purge-emitted events readable in the ledger. D4 makes it safe from recycling. Cost is bounded by D9, not unbounded. |
| D6 | **`wallet.ledger.player_id` is overwritten with a sentinel uuid**, never NULLed | The column is `uuid NOT NULL` (`modules/wallet/src/lib.rs:87`); a NULL raises `23502` inside the delivery transaction and pauses the subscription. A sentinel needs no `ALTER`, which the wipe-not-migrate rule requires. The ledger IS the dedup registry (`modules/wallet/src/store.rs:131-153`) and the starter key is `starter:{player_id}` (`modules/wallet/src/projection.rs:102`), so the row must survive with its key claimed. |
| D7 | **A sole admin's group is dissolved** | Owner's decision, against my recommendation of succession. **Recorded consequence: other members lose their group because one member left, and `groups` has no `group.deleted` topic, so a dissolution is indistinguishable from N leaves in the audit ledger.** |
| D8 | **`StartPosition::Genesis` for all seven** | Losing a deletion is a permanent retention leak with no repair path; re-delivering into an already-purged module is a no-op. The start position is part of the immutable `spec_hash` — not revisable without burning all seven ids. |
| D9 | **`HistoryPolicy::MinRetention { days: 90 }`** | **Revised in rev 2.** `KeepForever` is *not* checkpoint-coupled — the sweep only scans `policy = 'min_retention'` (`core/asyncevents/src/retention.rs:229-230`), so it would retain `player_id` + handle in a queryable table **permanently**, as the direct product of an erasure feature. 90 days keeps the ledger readable, leaves room for `storage` as the eighth consumer, and lets the record expire. |

### D3, stated in two legs because they are different problems

`#[retry_safe]` → `RetryMode::OnceAfterReconnect` is consumed **only** by
`core/remote`'s `Stub` (`core/remote/src/lib.rs:373-393`): it governs the
gateway→accounts-svc **internal edge** replay. It does not touch a client's HTTP retry.

- **Client leg.** `delete_account` is `auth = "player"`, so a client re-POST after a
  successful deletion is rejected by the gateway's `SessionsVerifier`
  (`modules/gateway/src/verifier.rs:105-120`) before it reaches accounts. **A client
  replay is a 401 by construction** and the receipt cannot help it. The client learns
  the outcome from the first response or not at all.
- **Internal-edge leg.** A gateway→accounts replay after a reconnect *does* reach the
  handler, and there the receipt row makes it return the original success instead of
  double-executing. That, and operator forensics, is what the receipt is for.

`begin_delete` is **not** `#[retry_safe]`: minting is not idempotent, and a
`#[retry_safe]` mutation needs its own idempotency semantics. It **upserts a single
live ticket per player**, so a repeated call returns the same ticket — that property
is what makes the op safe, and it is stated in the contract doc, not inferred.

**Explicitly NOT in scope:** GDPR export; redaction of `asyncevents.events` and
`audit.log`; `leaderboard`/`rating`/`match`, which cannot purge because
`match.finished` carries opaque contestant strings, not player ids; `mail`, which
stores no `player_id`; inbox rows *mentioning* the deleted player in rendered body
text.

---

## What survives a deletion, stated plainly

- **The two tables this feature adds are permanent identity registries.** The
  handle-reservation table (D4) and the deletion-receipt table (Step 3) both retain
  the deleted player's identity in schema `accounts`, by design, forever.
- `asyncevents.events` retains `player.deleted` — **`player_id` and the handle** — for
  90 days (D9), and longer if any subscription is paused, since GC is
  checkpoint-coupled for `MinRetention`.
- **The purge itself mints new events naming the player:** `character.deleted` × K,
  `friend.removed` × N (which denormalizes *both parties'* handles),
  `group.member_left` × M. Each carries its own topic retention.
- `audit.log` keeps a verbatim copy of every audited topic for
  `AUDIT_RETENTION_DAYS` (default 30).
- `wallet.ledger` keeps the movements with a sentinel owner (D6).
- `leaderboard.scores`, `rating.ratings`, `match.matches` keep the contestant string
  forever — unmatchable.
- `mail.outbox` keeps recipient/subject/body up to `MAIL_RETENTION_DAYS`, forever for
  `parked` rows.
- `groups.groups.creator_id` keeps a raw uuid of a player who no longer exists; the
  column is `NOT NULL` and DDL here is wipe-not-alter.
- **D7's consequence:** other members' group is gone.

---

## Contract shape

```rust
// api/accounts/events/src/lib.rs
pub struct PlayerDeleted { pub player_id: String, pub handle: String }
pub static PLAYER_DELETED: LazyLock<EventType<PlayerDeleted>> =
    LazyLock::new(|| define("player.deleted", 1, HistoryPolicy::MinRetention { days: 90 }));
```

`define(` **must be one physical line** with a plain decimal version literal
(`tools/topiccheck/src/tests.rs:267-351` panics otherwise). A `golden_samples()` entry
is mandatory. No `Option` fields.

```rust
// api/accounts/api/src/lib.rs, on the existing Auth trait
#[http(verb = "POST", path = "/accounts/delete/begin", auth = "player", success = 200)]
async fn begin_delete(&self, identity: Identity) -> Result<DeleteTicket, Error>;

#[http(verb = "POST", path = "/accounts/delete", auth = "player", success = 200)]
#[retry_safe]
async fn delete_account(&self, identity: Identity, ticket: String) -> Result<DeleteReceipt, Error>;
```

Both on `Auth`, so `opscatalog-gen`'s `rpc_modules()`, `csharp-client-gen`'s
`PROVIDERS` and `phase_a()` need **no** edit. DTOs are `String`-only
(`tools/csharp-client-gen/src/scrape.rs:429-432`).

### The tombstone table, settled here — one shape, two jobs

Each purging module owns:

```sql
CREATE TABLE IF NOT EXISTS <schema>.purged_players (
    player_id  uuid PRIMARY KEY,
    handle     text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    swept_at   timestamptz            -- NULL = still owed work
);
```

The sweep selects `WHERE swept_at IS NULL`, does its deletes, and **stamps
`swept_at`; it never deletes the row.** The row is permanent — that is Step 6's write
guard, and it is why the mail sweep's delete-and-watermark shape is adapted, not
copied verbatim. Growth is one row per deleted player, the cost `inventory` already
documents for `wiped_characters`.

---

## Step 1 — close the pre-existing `public-api` gap  `[inline]`

**(a)** `cargo run -p verifyctl -- --bless-public-api`, reviewing the diff.

**(b) Why now.** The stage is red on this tree *now*: the baseline holds 21 files
against 23 api crates — `groupsapi.txt` and `groupsevents.txt` are missing from the
previous rollout. The stage reports one Pass/Fail, so a later surface change would be
indistinguishable from this failure and both would be waved through.

**(c)** Diff the baseline directory; the only new files may be the two `groups*` ones.

**Red after this step:** nothing new. `public-api` goes green.

**(d)** `[inline]`.

## Step 2 — the contract  `[sonnet]`

**(a)** `api/accounts/events/src/lib.rs` (payload, `define`, `golden_samples()`),
`tools/topiccheck/src/main.rs`'s `defined_topics()`, `modules/audit`'s
`DURABLE_TOPICS` + `DURABLE_SPEC_IDS`, `--bless-contract-golden`.

**(b) Why now.** Everything imports it. Audit's entry must land in the same commit:
its drift test derives `want` from `accountsevents::golden_samples()` (the crate loop
already exists at `modules/audit/src/tests.rs:106-108`), so the sample alone turns it
red demanding a sink or a `NOT_LOGGED` entry.

**(c)** The spec id is derived, not chosen: `audit.player-deleted.v1`
(`durable_spec_ids_zip_with_topics` kebabs both `.` and `_`). The self-checks run
before the golden diff, so the bless cannot skip them.

**Red after this step:** `contract-golden` until blessed in the same commit, and
**`public-api` for `accountsevents`** — the crate exports `PlayerDeleted` +
`PLAYER_DELETED` from this step, not from Step 3 as rev 2 first said. Since the
`accountsevents` surface is *complete* after this step (Step 3 adds `accountsapi`
ops, not events), it is blessed here and the crate is closed permanently, shrinking
Step 4's bless to `accountsapi`.
`--durability-strict` is **green** — audit's list edit becomes a real `on_tx_raw`
subscription pinned to version 1, and `unsubscribed()` keys on `(topic, version)`.
*(Rev 1 asserted the opposite and was wrong; that claim justified a rollout boundary
that does not exist. Rev 2 then mis-stated the `public-api` step by one — the third
gate-state prediction in this plan's history to be wrong, which is why every
remaining step's red list is to be **observed**, never predicted.)*

**(d)** `[sonnet]`.

## Step 3 — the producer  `[opus]`

**(a)** `modules/accounts`: the handle-reservation table, the receipt table,
`begin_delete`, `delete_account`, the emit.

**(b) Why now.** The topic exists and is subscribed by audit; this makes it real.
Steps 3 and 4 are independent and may land in either order.

**(c)** Non-negotiables:
- **One transaction**: session revocation, handle reservation, receipt row, player
  `DELETE`, `emit_tx` (F4). The reservation and receipt tables carry **no FK to
  `players`**, or the cascade takes them with the row.
- The ticket is minted from `OsRng` at `begin_delete` and **upserted** so a repeat
  returns the same live ticket; it is validated at the single insert authority, the
  `notifications` `_idem_send` shape (`modules/notifications/src/service.rs:105-108`,
  `:350-365`), not form-side.
- A ticket belonging to another player, or unknown, is `NotFound` — never `Forbidden`.
- `oauth_states` has no cascade; in-flight LINK rows are left to their 10-minute TTL.
  Record it; do not add a cascade.

**Red after this step:** `codegen-freshness` (stale `opscatalog/src/generated.rs` and
the csharp tree), `conformance` (new `InputKey`s need `input_policies()` rows),
`public-api` (`accountsapi`/`accountsevents` surfaces). Step 4 closes all of them and
lands immediately after — the window is one step, not five.

**(d)** `[opus]` — core-implementer, *think hard*.

## Step 4 — the gates  `[opus]`

**(a)** `DEV_CLIENT_POLICY`, conformance `input_policies()` rows, regenerated
`opscatalog/src/generated.rs` and `clients/csharp/Generated/`, the four hand-regenerated
`tools/csharp-client-gen/testdata/` goldens, the three hand-written sets in
`tools/csharp-client-gen/src/tests.rs`, `--bless-public-api` (`accountsapi` only —
`accountsevents` closed in Step 2), and **`--bless-contract-golden`**: Step 3 adds two
`wire` lines and one `rpc-body` line, which rev 2 omitted from this list. Observed, not
predicted.

**(b) Why now.** Moved ahead of the consumers in rev 2: Steps 5-8 add no `#[http]` op,
so deferring the gates behind them left `verifyctl --fast` red for five steps.

**(c)** Order derived by execution: `golden_covers_the_known_surface` reads the
committed testdata, not the live scrape, so it goes red **only after** the goldens are
re-blessed — the count and both `BTreeSet` literals are a separate edit.
`DEV_CLIENT_POLICY`'s reverse test iterates the *generated* catalog, so it flips red at
regeneration; missing it is a runtime 403 for `dev-key-client`, not a gate failure.
Re-bless `public-api` with the same diff-the-directory discipline as Step 1.

**Red after this step:** nothing. `--fast` is green again.

**(d)** `[opus]`.

## Step 5 — the seven consumers  `[opus]`, one module per commit

**(a)** A `player.deleted` subscription in `characters`, `inventory`, `friends`,
`groups`, `notifications`, `wallet`. (Audit's raw sink landed in Step 2.)

**(b) Why now.** F5: no gate proves a consumer exists, so batching them makes a
missing one invisible. One module per commit, each reviewable alone.

**(c)** Every handler does **O(1) work**: insert `<schema>.purged_players`
(`ON CONFLICT DO NOTHING`) on the handed connection, carrying `player_id` and `handle`.
No deletes, no emits, no capability calls, nothing off the delivery connection.

Non-uniform deltas: `characters` and `friends` gain their **first** durable
subscription (new `projection.rs`, `mod` wiring, a `futures` dep); `notifications` and
`wallet` already depend on `accountsevents`, the other four need the dep. **No module
gains a `requires()` entry** — a subscription is pull-based and the consumer never
calls accounts (`notifications` proves it: empty `requires()`, five consumed topics).

**Red after this step:** nothing.

**(d)** `[opus]` — six dispatches.

## Step 6 — the schedules and the sweeps  `[opus]`

**(a)** Three scheduler files plus one sweep per purging module.

**(b) Why now.** After the tombstones exist to drain.

**Errata, from the first third landing (`53f1841`..`78b4337`) — these OVERRIDE the text
below, which the remaining sweeps must not follow as written:**

1. **ONE shared schedule name `player-purge`, not one per module.** The promise is a
   maximum — a player is deleted when the *slowest* module has swept — so six rows let
   an operator accelerate three and leave three at a day, producing a half-purged
   player and a latency claim nobody can state. Each module keeps its own subscription
   id and checkpoint, so a paused sweep in one does not stall another.
2. **The budget is checked at the PLAYER boundary, not between batches.** Between
   batches puts up to 512 statements between checks; the player boundary is the
   smallest point that preserves the invariant the emitting sweeps need, and it is
   verified to sit after both the delete and the stamp so a player can never be
   stranded between them.
3. **`ORDER BY` must name the table.** A bare `ORDER BY player_id` resolves to the
   SELECT's output column `player_id::text`, so the sort key is text, the PK btree
   cannot serve it, and the floor degrades to a filter on a full seq scan — measured
   at ~1850× with an external merge spilling to disk **inside the delivery
   transaction**. Write `ORDER BY <schema>.purged_players.player_id`.
4. **Carry `Cargo.lock` in each sweep's commit.** No verify stage catches a stale lock
   (`--locked` appears only on `cargo install` lines), so a fresh checkout is dirty on
   first build.
5a. **`characters` takes NO `pg_advisory_xact_lock`, reversing the bullet below
   (landed then removed, `2df4ba2`→this commit).** The lock buys no purge guarantee:
   it is released at the DELIVERY commit, not at the player boundary, so a `create`
   blocked on it wakes after the tombstone is stamped, counts 0, inserts, and is
   never revisited — the same orphan as with no lock, except that a `create` which
   would have committed *before* the DELETE now lands after it. `create`'s cap gate
   is a plain non-locking `SELECT count(*)` (`store.rs:130`), so the lock cannot
   serialize it either. The orphan is closed by Step 7's `purged_players` write
   guard, mirroring `inventory`'s `wiped_characters` check in `grant_starter`. The
   per-fire player cap went with it — its sole justification was bounding
   simultaneously held locks. `groups` keeps ITS cap on its own argument: its
   per-group write-path locks are real.

5. **Known gap, to close uniformly across all six or not at all:** each fire walks the
   whole PK index filtering `swept_at IS NULL` on the heap, and the table grows one
   permanent row per deleted player, so scan cost tracks *total* deletions rather than
   outstanding work. The fix is one partial index per module
   (`… (player_id) WHERE swept_at IS NULL`); adding it to some modules only would make
   the six tombstone tables disagree.

**(c) The seed is a named sub-step, not an implied one.** Without it nothing fires:
- `api/scheduler/events/src/lib.rs:54-73` — a `schedule_names::*` const per module;
- `modules/scheduler/src/lib.rs:165-179` — the `ON CONFLICT DO NOTHING` seed rows,
  interval **86400** (daily), matching the five existing;
- `modules/scheduler/src/tests.rs:232-245` — `seeded_schedule_names_are_contract`,
  whose own doc says a name left out is unguarded.

Daily is what makes the purge-latency promise **one day**, not "eventually". Say so.

Sweep shape: `PRUNE_BATCH = 256`, `FOR UPDATE SKIP LOCKED`, a `PRUNE_BUDGET` exit
checked **between** batches, and `swept_at` stamped rather than the row deleted.

Per-module semantics, settled here:
- `characters`: `DELETE … WHERE player_id RETURNING`, then one `character.deleted`
  per returned row — **never** `list_by_player`, which truncates at 1000. Take
  `player_lock_key` first, or a concurrent `create` lands a character for a purged
  player. The cascade **must** go through `character.deleted`: it is the only writer
  of `inventory.wiped_characters`, the only guard against a late `character.created`
  resurrecting holdings.
- `inventory`: `clear_owner_exec(Owner::player(pid))` only.
- `friends`: `WHERE $1::uuid IN (low_id, high_id) RETURNING`, emitting
  `friend.removed` with a new additive `REASON_ACCOUNT_DELETED`.
- `groups`: **`pg_advisory_xact_lock` is transaction-scoped** — there are no "short
  transactions" inside a delivery handler, and every group lock is held until the
  delivery commits. So: a **savepoint per group** inside the delivery tx, and a hard
  cap of `GROUPS_PER_FIRE` groups per fire so the number of simultaneously held locks
  is bounded and known. Dissolve per D7, emitting `group.member_left` for every
  remaining row. The sweep must be **total** (F3): it resolves the last-admin
  invariant, never returns `Conflict`.
- `notifications`: `DELETE WHERE player_id`.
- `wallet`: `DELETE FROM wallet.balances WHERE player_id`, then
  `UPDATE wallet.ledger SET player_id = <sentinel>::uuid WHERE player_id = $1` (D6),
  leaving `idempotency_key` claimed.

Emits come **only** from `DELETE … RETURNING`, so a redelivery emits nothing.

**Red after this step:** nothing.

**(d)** `[opus]` — *think hard*.

## Step 7 — the tombstone as a write guard  `[opus]`

**(a)** Each purging module's live write paths consult `purged_players`.

**(b) Why now.** F4 closes the authenticated-token leg; this closes the residue — a
request already past verification, and any server-to-server write.

**(c)** `insert_pending_tx`, `insert_membership_tx` and siblings check the tombstone.
The row is permanent by the Step-3 DDL, so the guard does not evaporate when the sweep
finishes.

**(d)** `[opus]`.

**Errata (as landed, `af39fd4`..`544d6a1`).**

1. **The guard sits at each module's service write path, not at the `insert_*_tx`
   statements.** A store helper (`is_player_purged[_tx]`) reads the tombstone; the
   caller decides the status, because the store layer can only answer an
   `sqlx::Error`, i.e. a 500, where the contract wants 409.
2. **Guarded:** `characters.create`, `inventory.grant`, `friends.request` (caller
   side), `groups.create`, `groups.join`, `notifications`' one insert authority
   `Service::deliver_on`, `wallet`'s one movement authority `Service::apply_on`.
   **Deliberately not guarded, with reasons:** `characters.delete`,
   `friends.accept/decline/remove`, `groups.respond/decide` — they mutate or delete a
   PRE-EXISTING row the sweep collects by `player_id` in any state, so they cannot
   leave a row it would miss; `friends.request`'s and `groups.invite`'s TARGET —
   resolved through `accountsapi::Directory::find_by_handle`, which selects `FROM
   accounts.players`, a row `accounts` deletes in the same transaction that emits
   `player.deleted`, so a tombstoned target has already answered 404 one call earlier;
   `inventory.grant_starter` — its owner is a CHARACTER, guarded by the existing
   `wiped_characters` tombstone.
3. **Status.** `Conflict` on every op-facing path (the request is well-formed and
   names a real past player; the caller is either asking about ITSELF or is a server,
   so there is no enumeration oracle a 404 would have to protect). Two deviations,
   both forced by the durable plane: `notifications` answers `Invalid`, the one status
   `deliver_or_skip` already turns into `Ok(())`, so a deleted recipient cannot pause
   every player's inbox; `wallet` answers a new `Outcome::Purged` variant rather than
   an `Err`, because an `Err` out of `apply_on` is propagated by contract and would
   pause `wallet.player-promoted.v1`.
4. **No advisory lock — a plain read.** The tombstone row is permanent, so any
   statement taken after the plant commits sees it, and the plant commits strictly
   before the sweep can see the row to act on it.
5. **Known gap (the residual window the plain read leaves).** A live write transaction
   that read the tombstone BEFORE the plant committed and commits its row AFTER that
   player's sweep has stamped `swept_at` leaves one orphan row no later sweep
   revisits. Closing it requires the plant handler AND every guarded write path in all
   six modules to share one per-player advisory lock — which would also put a lock on
   `wallet`'s hot movement path, where the store's own note explains there is
   deliberately none. Not taken, but the orphan is not uniformly the unreachable row
   the first cut of this note claimed: for `characters`/`inventory`/`notifications`/
   `wallet` every read path IS identity-keyed and that identity is gone, so those four
   really do leak one dead row apiece. `friends` and `groups` are different — `friends`'
   `page` (`modules/friends/src/service.rs:305-350`) keys on the surviving counterparty,
   and `groups`' `page_group` (`modules/groups/src/service.rs:314`) keys on the group,
   so an orphan there is *visible* to the counterparty or co-members and *removable* by
   them (`friends.remove`, `groups.decide` reject) — just never revisited by a sweep.
   The one unrecoverable shape is an orphan from `groups.create`: the deleted player is
   the group's sole `ROLE_ADMIN`, `decide`'s kick requires an admin and `leave` is the
   only self-removal, so that group ends up with no reachable actor at all. The cost of
   closing all of this remains a cross-cutting lock on six fortresses. The window is
   ordinarily a day wide only in the sense that the sweep is daily; it shrinks to a
   scheduler tick when a tombstone lands just before a due fire, so it is narrow but
   real — not "impossible".

## Step 8 — operator visibility  `[opus]`

**(a)** A readiness check on a paused subscription, and a read-only admin view of
in-flight purges.

**(b) Why now.** F2: today a half-deleted player is invisible.

**(c)** A `httpmw::READINESS_SLOT` check reading `state = 'paused'` for the process's
own subscriptions. The admin view reads `purged_players` (`swept_at IS NULL`).

**(d)** `[opus]`.

**Errata (as landed) — the readiness check is NOT built, and F2 overstated the
invisibility.**

1. **No `ReadyCheck` reads `state = 'paused'`, deliberately.** `/readyz` in this repo is
   a BOOT and RESTART gate, never a load-balancer gate — nothing routes traffic on it.
   `weles::supervisor::step` (`weles/src/supervisor.rs:230-245`) answers a live process
   that is `NotReady` past `HEALTH_DEADLINE` (30s) with `Directive::Kill`, then backoff,
   then `Failed` after `MAX_CONSECUTIVE_FAILURES` (5); `devctl`/`splitproof` gate boot on
   the same probe. A paused subscription is DURABLE state in
   `asyncevents.subscriptions`, so the verdict is identical on every respawn: the check
   would convert one stalled subscription into a guaranteed kill-loop of a process that
   is serving HTTP correctly — and it would kill the very worker that `eventctl resume`
   needs alive to drain the backlog. The plan's "right for a purge that must complete"
   inverts: killing the process is what guarantees the purge never completes.
2. **F2's "invisible" was wrong.** A paused subscription is already exported per process
   as `asyncevents_subscription_paused_state{subscription}` and
   `asyncevents_subscriptions_paused`, restricted to the process's OWN subscription ids
   (`core/asyncevents/src/plane_metrics.rs:133-168`, ids from `Plane::start`), plus
   `asyncevents_retention_blocked_age_seconds` when the pause holds back GC, and
   `cargo run -p eventctl -- list` prints `paused_since`. What F2 correctly names is that
   NO `ReadyCheck` reads it — which is the right design, not the gap.
3. **What was actually missing in that alert shape, and is now closed:** every plane gauge
   is PUSHED by the 10s `refresh_loop`, whose death is log-only and whose per-tick error
   was a bare `tracing::warn!`. A dead or persistently failing loop freezes every gauge at
   its last value, so an alert keyed on `..._paused_state` would silently never fire.
   `asyncevents_metrics_refresh_total` / `asyncevents_metrics_refresh_errors_total` make
   that staleness self-evident (`rate(...) == 0`); the alert rule is
   `asyncevents_subscription_paused_state == 1` AND a non-flat refresh rate.
4. **The paused state cannot reach the admin portal at all.** `core/*` never imports an
   `api/` crate (constraint 1), so the plane cannot contribute an `adminapi::Item`; and
   archcheck rule 14b bans any schema-qualified `asyncevents.<table>` read outside the
   plane, so no module can render it either. Structural known gap: the plane's operator
   surface is `/metrics` + `eventctl`, never `/admin`.
5. **The admin view is a KPI on each purging module's EXISTING page, not a new page and
   not a cross-schema reader.** The six tombstone tables live in six fortress schemas, so
   each module reads its own: `Purges Pending` (count of `swept_at IS NULL`) with the
   oldest pending `created_at` as the sub-line. It rides `admin.adminData` unchanged in
   split. A TABLE was not an option: `adminapi::Content` carries exactly ONE `table`, and
   every one of the six pages already spends it.
6. **Step 6's known gap #5 (the partial index) is closed here, uniformly across all six**,
   because the KPI's `WHERE swept_at IS NULL` is a second consumer of the same access path
   and would otherwise seq-scan a table that grows one permanent row per deleted player.
   `<schema>.purged_players_pending_idx ON (player_id) WHERE swept_at IS NULL` serves both
   the sweep's `ORDER BY <schema>.purged_players.player_id` keyset walk and the KPI's
   aggregate.
7. **Deliberately not built:** a producer-side "deletions requested" KPI on the accounts
   page (the denominator that would make a paused PLANT visible as a disagreement between
   modules) — the plant-paused case is exactly what `..._paused_state` reports, and a
   `count(*)` over the permanent receipts table is the unbounded scan this step is trying
   to avoid.

## Step 9 — split-proof assertions  `[test-author]`, `model:"opus"`

**(a)** `[DL1]`-`[DL9]` in `tools/splitproof/src/main.rs`, both topologies.
(`[AD*]` is taken by the admin suite.)

**(b) Why now.** F5: the only instrument that can prove seven consumers exist.

**(c)** Per consumer, **three assertions in the same check, all of them**:
1. target rows in that schema = 0;
2. a second player's identical rows **unchanged** (the `[GR6]` survivor shape);
3. that subscription's cursor in `asyncevents.subscriptions` advanced past the event.

(1) alone passes if the sweep deleted the wrong table; (3) alone passes if the
tombstone landed and the sweep did nothing.

Plus: the schedule forced with `UPDATE scheduler.schedules SET last_fired =
to_timestamp(0)` (the harness pattern at `tools/splitproof/src/main.rs:1633`, `:2553`);
the handle reservation blocking a re-mint; and the internal-edge replay of
`delete_account` returning the original receipt. **The client replay is a 401 by
construction — assert that, not a 404.**

**Prove one assertion non-vacuous** by perturbing what it names and reverting
byte-identically.

**(d)** `[test-author]` at `model:"opus"`.

## Step 10 — module unit tests  `[test-author]`, `model:"sonnet"`

**(a)** Tests for Steps 3-8 in each module's `src/*_tests.rs`.

**(c)** Each names its previously-wrong branch: the ticket upsert returning the same
live ticket; the handle reservation blocking a re-mint; the sweep's survivor assertion
*and* the survivor emitting nothing; the batch loop terminating across a `created_at`
tie at the boundary; the tombstone blocking a post-purge write; wallet's sentinel row
keeping its `idempotency_key` claimed; **the canonical-id handling at the store layer**
(the wallet precedent, `modules/wallet/src/store.rs:139-140` — this cannot be a
split-proof assertion, because `delete_account` takes no caller-supplied uuid).
Confirm zero `SKIP: postgres unreachable` lines and say so.

**(d)** `[test-author]` at `model:"sonnet"`.

## Step 11 — acceptance  `[inline]`

**Precondition, operator action, blocking:** `DROP SCHEMA accounts CASCADE` and boot
fresh. Step 3 moved the handle-mint authority into a new `accounts.handles` table,
which starts empty — every pre-existing player row holds a `(lower(display_name),
discriminator)` pair with no claim, so a registration can claim a pair a legacy player
already holds and then raise `23505` on `accounts.players`, aborting the caller's
transaction as a 500 rather than a retry. Wipe is the sanctioned migration strategy
here; a backfill is banned. **No gate reports this** — `cargo test -p accounts` passes
because its display names are random — so it must be done deliberately before
acceptance and before any `devctl up`.


`cargo run -p verifyctl -- --fast`, then `--all --strict`. One rollout at a time.
**Redirect to a file and read the stage table — a trailing `echo $?` reports the
echo's status, not verifyctl's.** (That mistake was made on this repo on 2026-09-10.)

## Step 12 — documentation  `[docs]`, `model:"sonnet"`

Named files: `docs/roadmap/feature-tracker.md:141`,
`docs/reference/game-backend-feature-gaps.md:79`,
`docs/reference/event-plane-ops.md` (the paused-purge operator story),
`api/friends/events/src/lib.rs:88-93` (its doc enumerates `Removed.reason` as exactly
three values — a fourth makes it false on a public contract surface),
`CLAUDE.md` (the accounts paragraph, and the audit topic count — corrected to **15**
by the groups rollout's docs step `a0d44095`, and made stale again by this feature's
own Step 2, so the fix is 15 → 16; verify the live `DURABLE_TOPICS` length rather
than trusting either number),
`.agents/shared/gamebackend.md`, and this plan's errata.

**The known-gap list must be complete or it is silence implying coverage:** no GDPR
export; no redaction of the event log or audit ledger; `leaderboard`/`rating`/`match`
unpurgeable pending a `match.finished` payload version; `mail` unpurgeable; inbox rows
*mentioning* the deleted player unreachable; `groups.groups.creator_id` dangling by
design; **D7 — a sole admin's deletion dissolves their group, and no `group.deleted`
topic exists, so the ledger cannot distinguish a dissolution from N leaves**; purge
latency bounded by the daily sweep, not immediate; and the two new permanent identity
registries this feature adds.

### A pre-existing paging twin found while fixing Step 6's floor

`modules/groups/src/store.rs:582-588` (`group_page_sql`) is a real twin of the
`ORDER BY` defect: it selects `player_id::text` and orders by the unqualified name, so
the sort key is text, while its keyset cut at `:462` compares
`(created_at, player_id) < ($2::timestamptz, $3::uuid)` — the **uuid** column. The two
orders coincide only because canonical uuid text is fixed-length with hyphens at
identical offsets; nothing states or pins that, and the ordering cannot be
index-served either way. Latent, not currently wrong. It is a different authority
(keyset pagination, not a purge floor) and needs its own paging tests, so it is
recorded here rather than fixed inside this rollout.

`modules/inventory/src/store.rs:215-222` (`list_owners`) has the same shape and is
benign: a `GROUP BY` already forces the aggregate, there is no range predicate to
demote, and the admin list is `LIMIT`-capped.

### Two pre-existing gate defects found while auditing Step 4

Neither is caused by this rollout; both were proven by mutation and are recorded so
they are not re-discovered as new.

1. **A conformance `Validated { cap }` number is decoration.** Replacing the row's
   `cap` with `999_999` leaves `conformancecheck` at exit 0 while the report prints
   `validated(999999): …` as fact — nothing compares the row's number to the
   `CapCase` that actually enforces it. The only binding of a policy cap to a probe is
   `checks::CREDENTIAL_CAPS`, which covers `loginFederated`'s providers alone. So a
   row can publish a bound the code does not enforce. Minimal closure: for every
   `Validated` row in a module that declares an `InputByteCaps` fixture, require a
   `CapCase` whose declared case name the row cites — matching by number alone is
   insufficient, as `CREDENTIAL_CAPS`' own doc explains.
2. **`OPAQUE_REQUEST_TYPES` matches the written path's bare segment.** Its doc claims
   *"Matched against the written path, not the last segment, so a foreign
   `somecrate::Identity` is not allowlisted by sharing a name"*, and the committed test
   only exercises the qualified form. A contract writing `use somecrate::Identity;`
   then `actor: Identity` renders as `"Identity"` and **is** allowlisted — the exact
   escape the doc says is closed. Closure: list `opsapi::Identity` and normalise
   imports, or resolve the `use` before matching.

---

## Review response (rev 1 → rev 2)

Blocking findings 1-8, all addressed: D9 changed to `MinRetention{90}` with the
`KeepForever`-is-not-checkpoint-coupled error corrected; D6 changed from NULL to a
sentinel uuid because the column is `NOT NULL`; the `purged_players` DDL settled with
`swept_at` stamped rather than the row deleted, resolving the drain-vs-permanence
contradiction; the `groups` sweep respecting `pg_advisory_xact_lock`'s transaction
scope via savepoints and a per-fire cap; **the false red-window claim deleted** —
Steps 3 and 4 are independent, and audit's Step-2 sink subscribes the topic; the gates
step moved from 8th to 4th, shrinking the red window from five steps to one; the
`public-api` re-bless added to Step 4; and the six schedule seeds made a named
sub-step with the interval and its one-day latency consequence.

Findings 9-16, all addressed: F1 restated as resumable/budget-bounded/monotone rather
than "outside a delivery transaction"; the survivors list extended with the two new
permanent tables, the purge's own minted events, the handle, and D7's consequence;
D3 split into its client (401 by construction) and internal-edge (receipt) legs, with
`#[retry_safe]` removed from `begin_delete` and its upsert semantics stated;
assertion tags moved to `[DL*]`; the canonical-id assertion moved from split-proof to
Step 10's store-layer tests, since the op takes no caller-supplied uuid; Step 9's three
per-consumer assertions spelled out as "and"; the four citation drifts corrected; and
Step 12's two missing named files added.

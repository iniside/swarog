# Purge race locks (plan B) — plan

Status: approved by the owner 2026-11-18 (revised plan); steps 1–3 landed (`dd789cc2`, `8622b65d`); the owner ruled on proof-audit batch 1 on 2026-11-18, and the review-fix landed; steps 4–5 are in progress.

Revision 2026-11-18: the owner approved the original twelve-step plan on 2026-11-18. No step was
dispatched, and `git log --grep=purge-race-locks` shows no step commit. The plan was then
revised to the current agent workflow (`.agents/shared/planning-dispatch.md`, the
`implementation-dispatch` and `research-plan-present` skills):
- Each separate `[test-author]` step is folded into its code step. These are in-module tests, not
  verify gates, splitproof assertions or a new harness, so the implementer writes them.
- `proof-auditor` runs in batches, after steps 3, 5 and 6, instead of after every test step.
  The owner ruled that the concurrency tests are the risk surface.
- Step 1 is now `[inline]` because it changes one literal in one file.
- False comments are fixed in the step that falsifies them. Only cadence prose outside the
  steps' files stays in the docs step.
- ACCOUNT-DELETION-05 is closed by the step that closes the race, not by the docs step.

The scope, invariants and lock design are unchanged.

## Context

Owner decision 2026-11-17 ("wyscigow nie zakceptujemy. to akurat powazny blad"): this reverses
the "Known gap … Not taken" decision in `docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:437-460`.
That decision is filed today as ACCOUNT-DELETION-05 "accepted" in `docs/todo/account-deletion.md`.

**The race.**
1. A writer reads "no tombstone".
2. The tombstone commits.
3. The purge sweep runs under READ COMMITTED. It cannot see the writer's uncommitted INSERT, so it
   stamps `swept_at` without purging that row.
4. The writer commits, and its row outlives erasure forever.

Wallet (39e2f802, plan A) and storage already close this race. This plan closes it in
**characters, friends, notifications, inventory and groups**.

**Owner decisions (2026-11-17):**
- **Lock pattern.** Use wallet and storage's pattern: one per-player `pg_advisory_xact_lock`.
  Every guarded writer takes it before its tombstone read. The sweep claims at most **64 players
  per fire** and takes the same lock per player before it purges.
- **Cadence.** The `player-purge` schedule becomes more frequent, so 64 per fire drains any
  realistic deletion rate. This applies to all seven sweeps, wallet and storage included.
- **Counterparty tombstones.** `friends.request` and `groups.invite` also check the counterparty's
  tombstone, under that player's lock. A purged counterparty gets `NotFound(NO_SUCH_PLAYER)`,
  the same answer as a directory miss, so the check adds no oracle.
- **Inventory.** The dev-gated inventory `grant` is fixed too.

**Out of scope:**
- ACCOUNT-DELETION-01/02/03 (event-log redaction; leaderboard, rating, match and mail purge).
- Other players' notification bodies that quote a deleted handle.
- `verifyctl`, which waits until the admin panel lands (owner 2026-11-13).

The plan adds no new module, event, capability or contract. The only schema change is the
scheduler seed value, and the wipe-as-migration rule applies.

**Lead bookkeeping:** done. ACCOUNT-DELETION-05 is already under `## In progress` in
`docs/todo/account-deletion.md`, citing this plan.

## Verified facts (research 2026-11-17; re-checked by core-reviewer)

**Shared:**
- `emit_tx` takes `WRITER_LOCK_KEY` in shared mode (`core/asyncevents/src/store.rs:15-18,33,145`).
  Only the offline `bump_generation` takes it exclusive (`:544-561`).
- Every handler, sweeps included, runs inside the plane's `SAVEPOINT deliver`
  (`core/asyncevents/src/worker.rs:257`).
- The worker processes one event per transaction (`worker.rs:194,238`).
- There are `WORKERS = 2` per process (`core/asyncevents/src/lib.rs:53`). The handler timeout is
  10 s (`worker.rs:31`), and `SWEEP_BUDGET` is 5 s.
- Only one copy of a given sweep runs at a time: its subscription row is claimed with
  `FOR UPDATE SKIP LOCKED` (`worker.rs:207`).
- **sqlx and dropped transactions.** sqlx defers a dropped transaction's ROLLBACK. That can leave
  an advisory lock held and stall the next writer, so every non-committing return must call
  `tx.rollback()` explicitly. This is the repo rule at `modules/characters/src/service.rs:166-168,217-220`.
- **Precedents:**
  - wallet: `store.rs` (`wallet_player_lock_key`, `lock_player_tx`) and `projection.rs`
    (`WALLET_PLAYERS_PER_FIRE`);
  - wallet tests: Race 1, Race 2, the fire bound, `racer_pool`, `finish`, key-matched
    `wait_advisory_parked`, `assert_claimed_by_the_next_fire`, `taken_player_key`,
    `fixed_player`, and the spawn-plus-terminate cleanup (51fb57aa);
  - storage: `store.rs:125-140,265-277` and `projection.rs:30-42,59-123`.
- **Scheduler seed.** `modules/scheduler/src/lib.rs:174-176` seeds `('player-purge', 86400)` with
  `ON CONFLICT DO NOTHING`. 300 s is inside the CHECK bounds. No test or splitproof assertion pins
  86400 for `player-purge`; only comments state it.
- Postgres orders uuids by memcmp over the 16 bytes, which is the same as the order of the 32
  lowercase hex digits.

**characters:**
- `Player::create` (`service.rs:106-190`) runs:
  1. the lock (`player_lock_key`, `characters.player/`, `:41-56`);
  2. `is_player_purged_tx`;
  3. the cap check;
  4. the INSERT;
  5. `emit_tx`;
  6. commit.
- It is the only writer that inserts a player row.
- The sweep (`projection.rs:67-135`) takes no lock. It uses `PURGE_BATCH=256` with a loop and
  `PURGE_FLOOR`, and deletes, emits, then stamps.

**friends:**
- `request` (`service.rs:367-487`):
  - resolves the target through the directory before the transaction;
  - calls `lock_requester_tx(me)` (`friends.requester/`, `store.rs:49-85`);
  - then calls `is_player_purged_tx(me)` only.
- The edge names both players. The target's tombstone is never checked; the comment at
  `:27-32` relies on the directory miss.
- `accept`, `decline` and `remove` only UPDATE or DELETE rows that already exist.
- The sweep (`projection.rs:67-131`) takes no lock. It uses `PURGE_BATCH=256` with a loop and
  `PURGE_FLOOR`, and `purge_edges_tx` deletes edges from either side (`store.rs:323-337`).

**notifications:**
- `deliver_on` (`service.rs:329-359`) is the only insert path. It runs the guard, then
  `insert_tx`, and takes no lock.
- Its callers are:
  - the four durable handlers, on the handed delivery transaction (`projection.rs:189-286`, via
    `deliver_or_skip` `:125-164`);
  - `send_operator_mail` (`service.rs:379-404`), on `pool.acquire()` autocommit. Only tests call
    it (`tests.rs:913-921,945,1544,1572`).
- `PurgeSweepHandler` (`projection.rs:418-475`) is a unit struct with no lock. It uses
  `PURGE_BATCH=256` with a loop.

**inventory:**
- `Holdings::grant` (`service.rs:108-145`) is dev-gated (`lib.rs:164`). It runs as separate
  autocommits: `item_exists` (`store.rs:226`), then `is_player_purged` (`:119`), then
  `grant_pool` (`:144`).
- It takes no lock.
- The character-owned path already serializes on `inventory.character/`.
- The player sweep (`projection.rs:263-310`) takes no lock and uses `PURGE_BATCH=256` with a loop.
- `docs/todo/inventory.md` tracks a known failing sweep-handler test.

**groups:**
- **Writers that insert:**
  - `create` (`service.rs:354-425`): tombstone guard, then insert. No lock.
  - `join` (`:490-583`): G, then guard, then insert.
  - `invite` (`:683-746`): directory lookup before the transaction, then G,
    `visible_role_tx`, `counts_tx` (`:722-732`), then insert. No tombstone guard.
- `respond`, `decide` and `leave` touch existing rows under G.
- The sweep (`projection.rs:288-358`):
  - takes no player lock;
  - runs `PURGE_BATCH=256` with a loop and `PURGE_FLOOR`;
  - bounds groups per fire with `GROUPS_PER_FIRE=64`;
  - per player, runs `player_groups_tx` (committed memberships only), then a per-group
    `SAVEPOINT purge_group` around `purge_one_group`, which takes G;
  - marks a player Skipped on a group error, leaving it unstamped.
- **Ordering hazard.** Taking player locks inside the per-player loop can deadlock against
  `join`: the sweep holds G1 and waits on P_b, while `join(G1)` holds P_b and waits on G1.

## Invariants

1. **One player key per module.** Each of the five modules has ONE per-player key: FNV-1a over a
   module namespace plus the uuid normalised to 32 lowercase hex digits. Copy it from wallet; do
   not import it.
   - Every path that inserts a row naming a player takes that key on the same connection and
     transaction as the write, BEFORE it reads that player's tombstone, and holds it until
     commit or rollback.
   - The sweep takes the same key per claimed player before purging that player.
2. **Lock order.**
   - A transaction takes its player locks before any module-owned non-player advisory key
     (groups `G`).
   - When it takes several player locks, it takes them in ascending normalised-uuid order, which
     is the sweep's claim order.
   - A sweep acquires every player lock of its fire before its first `G`, never inside the
     per-group `SAVEPOINT purge_group`.
   - The plane's shared `WRITER_LOCK_KEY` is exempt from this order: shared never conflicts with
     shared, and exclusive is taken only by the offline `bump_generation`.
   - A writer holds at most two player locks (friends); every other writer holds at most one.
3. **Bounded fire.**
   - One constant, `<MODULE>_PLAYERS_PER_FIRE = 64`, is both the claim `LIMIT` and the bound on
     player locks per fire. There is no batch loop and no `PURGE_FLOOR`.
   - A fire may stop early on `SWEEP_BUDGET`, but only at a player boundary.
4. **No oracle.** A purged counterparty gets exactly the directory-miss answer. The guard runs
   before any check whose answer could tell "purged" apart from "absent", such as a cap or
   capacity check.
5. **Lock release.** Every new or changed transaction that holds a player lock calls
   `tx.rollback()` explicitly on every return that does not commit. That includes database-error
   returns, which must not leave through a bare `?` while the transaction is open (owner ruling
   2026-11-18).
6. **Drain cadence.** The `player-purge` seed is **300 s**, so 64 per fire drains about 18k
   deleted accounts per day per module.
   - **Accepted limit (groups only), for the owner to confirm.** A player that is persistently
     Skipped is reclaimed first on every fire, so k such players cut that fire to 64−k.
7. **No topology branch.** No schema change beyond the seed value. Advisory keys are
   database-global, so the same locks serve the monolith and the split.

## Steps

Every code step writes its own tests to the bar "revert the fix and the test must fail". Each
mutation it lists must be shown red. Steps 2–6 follow the wallet test harness: Race 1, Race 2, the
fire bound, `racer_pool`, `finish`, the key-matched `wait_advisory_parked`,
`assert_claimed_by_the_next_fire`, `taken_player_key`, `fixed_player` and the
spawn-plus-terminate cleanup. That harness is an existing precedent, not a new harness. No step adds
a verify gate or a splitproof assertion, so there is no `[test-author]` step.

Modules go simplest first and groups last, so the pattern is proven on the simple shapes before
the two-key ordering. Steps 2–6 touch disjoint files. A step's review may run beside the next
implementation, but implementations run one at a time, because each one runs `cargo test` (one
rollout at a time).

Every dispatch prompt carries:
- `comments: default NONE`, plus the step's **Comments** line;
- "`cargo fmt` is not safe here; hand-format";
- the path `.agents/skills/code-navigation/SKILL.md`;
- the `safe-verification` skill for every cargo run.

**Race-test rules (owner rulings 2026-11-18, after proof-audit batch 1):**
- **Park point.** Park the writer at a point inside the module, after its guard, wherever one
  exists, for example a uniqueness conflict on an uncommitted row held by a gate transaction.
  Detect the park with a check matched to the gate's transaction id. Hold the event plane's
  `WRITER_LOCK_KEY` exclusively only when the module has no such point. That use follows the
  documented exception in `docs/reference/testing.md` (held only under the crate's `DB_LOCK`,
  released as soon as the competing transaction is parked behind the racer), and the step's report must say why no module-scoped
  park point exists.
- **Key tests.** A lock-key test pins behaviour, not the formula:
  - the key is the same for every spelling of one uuid;
  - different uuids give different keys;
  - the race tests read the key back from `pg_locks`.

  It never recomputes FNV-1a over the namespace string.

A step fixes, in its own commit, every comment in its own files that it makes false. A comment
that cannot be made true in one line is deleted. The step reports false comments outside its files
for step 7.

### Step 1 — player-purge cadence `[inline]`
**(a) What:** `modules/scheduler/src/lib.rs:175`, the seed literal only.
**(b) Why first:** it is independent and tiny. It bounds the backlog that the 64-per-fire cap would
otherwise create, including for wallet and storage.
**(c) How:**
- Change the seed to `('player-purge', 300)`. No test or splitproof assertion pins 86400 for
  `player-purge` (Verified facts), so this is one literal in one file. That is under the dispatch
  threshold, so there is no subagent and no review.
- Run `cargo test -p scheduler` under the `safe-verification` skill.
- Existing databases keep 86400 because of `ON CONFLICT DO NOTHING`. Report the one-line operator
  `UPDATE` for the local dev DB as an owner action; do not run it.
- The cadence comments this makes false are fixed in step 7.
**(d) Lane:** `[inline]` (main session).

Commit: `fix(scheduler): drain purged players every five minutes (purge-race-locks step 1)`.

### Step 2 — characters lock and race tests `[opus]`
**(a) What:** `modules/characters/src/{store.rs,service.rs,projection.rs}`,
`modules/characters/src/tests/purge.rs`, `modules/characters/src/tests.rs`.
**(b) Why now:** this is the simplest shape: one writer, one key, and the key already exists.
**(c) How:**
- Lift `player_lock_key` and its lock query into a single `Store::lock_player_tx`, with the key
  unchanged. `create` calls it.
- Sweep:
  - `CHARACTERS_PLAYERS_PER_FIRE = 64` becomes the only claim `LIMIT`;
  - delete the loop and `PURGE_FLOOR`;
  - take the lock before each player's DELETE.
- Fix the `is_player_purged_tx` "plain read, no advisory lock" doc.
- **Tests:**
  - **Race 1:** `create` is in flight past its guard; the sweep parks on the player's key; then
    it purges the character.
  - **Race 2:** the sweep holds the player; `create` parks, then gets 409 `PLAYER_DELETED`.
  - **Fire bound:** 65 tombstones on the lowest fixed ids; the fire stamps exactly 64.
  - **Existing sweep tests:** move them to fixed low ids with `assert_claimed_by_the_next_fire`.
    Replace the batch-loop test (`src/tests/purge.rs:250`), which the removed loop invalidates.
  - No sleeps.
  - **Mutations, each shown red:** sweep lock removed, writer lock removed, limit raised.
  - **Topology:** the lock is database-global, so in-module tests on the shared Postgres cover
    the split.
- Run build, clippy, then `cargo test -p characters` three times.

**Comments:** none new. The existing sqlx explicit-rollback note stays.
**(d) Lane:** `core-implementer`, opus.

Commit: `fix(characters): serialize character creation and the purge sweep per player (purge-race-locks step 2)`.

### Step 3 — friends lock and race tests `[opus]`
**(a) What:** `modules/friends/src/{store.rs,service.rs,projection.rs}`,
`modules/friends/src/tests.rs` (purge section) and its helpers.
**(b) Why now:** this introduces the two-player order, while the shape is still simpler than
groups.
**(c) How:**
- Generalise the requester key into one per-player key. The namespace string is a naming choice.
- Add `lock_pair_tx(a, b)`: it locks both ids in ascending normalised-uuid order, or once if
  `a == b`.
- In `request`, replace `lock_requester_tx(me)` with `lock_pair_tx(me, target)`. Then check:
  - `is_player_purged_tx(me)`: 409 `PLAYER_DELETED`, unchanged;
  - `is_player_purged_tx(target)`: exactly the directory-miss answer. Read it from the code.

  The cap still holds because the requester's lock is held.
- Sweep: `FRIENDS_PLAYERS_PER_FIRE = 64` as the single claim. No loop, no `PURGE_FLOOR`. Lock
  before `purge_edges_tx`.
- Every new non-commit return rolls back explicitly (Invariant 5).
- Fix the false docs: `service.rs:27-32` (it relies on the directory miss) and the
  `is_player_purged_tx` "no advisory lock" doc.
- **Tests:**
  - Race 1, requester tombstoned mid-transaction.
  - Race 1, target tombstoned mid-transaction.
  - Race 2, both parties: the request parks, then answers 409 for the requester and the
    directory-miss answer for the target.
  - **Order test:**
    - Players A < B (fixed low ids) are both tombstoned.
    - A request between them parks while holding one lock.
    - One sweep claims both players.
    - The test asserts there is no deadlock error and that both are purged.
    - With the pair order reversed (mutation), it must fail with 40P01.
  - Fire bound.
  - Move the existing sweep tests to `low_uuid()` ids plus the claim precondition. Replace any
    test pinned to the removed loop.
  - **Mutations, each shown red:** sweep lock removed, target guard removed, pair order reversed,
    limit raised.
- Run build, clippy, then `cargo test -p friends` three times.

**Comments:** one line on `lock_pair_tx`, giving the ascending order as the sweep's claim order.
**(d) Lane:** `core-implementer`, opus.

Commit: `fix(friends): serialize requests and the purge sweep on both players' locks (purge-race-locks step 3)`.

### Step 4 — notifications lock and race tests `[opus]`
**(a) What:** `modules/notifications/src/{store.rs,service.rs,projection.rs,conformance.rs}`,
`modules/notifications/src/tests.rs` (purge and operator sections).
**(b) Why now:** this is a single-key shape with two caller kinds, a delivery transaction and an
operator path.
**(c) How:**
- Add a `notifications.player/` key and `Store::lock_player_tx`.
- In `deliver_on`, take the lock after `validate_new` and before `is_player_purged_tx`. A
  malformed id hashes its raw bytes and cannot raise 22P02.
- Move `send_operator_mail` from `pool.acquire()` to `pool.begin()`:
  - run `deliver_on` and `matches_tx` on that transaction, then commit;
  - every non-commit return rolls back explicitly;
  - report that the nudge now fires before commit, as it already does on the durable path.
- Give `PurgeSweepHandler` the key, following the module's existing handler shapes.
- Sweep: `NOTIFICATIONS_PLAYERS_PER_FIRE = 64` as the single claim. No loop. Lock before each
  DELETE.
- Fix the false docs: `service.rs:307-310,366`, `conformance.rs:39` (`pool.acquire()`) and the
  `is_player_purged_tx` "no advisory lock" doc.
- **Tests:**
  - Race 1 on the durable path: a delivery handler parks between its guard and its insert.
  - Race 1 on the operator path: this proves the move from autocommit to a transaction.
  - Race 2.
  - Fire bound.
  - Move the existing sweep tests to fixed low ids.
  - **Mutations, each shown red:**
    - writer lock removed;
    - sweep lock removed;
    - operator path reverted to autocommit;
    - operator error path without explicit rollback, if it can be made observable; otherwise say
      why it cannot;
    - limit raised.
- Run build, clippy, then `cargo test -p notifications` three times.

**Comments:** none new.
**(d) Lane:** `core-implementer`, opus.

Commit: `fix(notifications): serialize inbox delivery and the purge sweep per recipient (purge-race-locks step 4)`.

### Step 5 — inventory lock and race tests `[opus]`
**(a) What:** `modules/inventory/src/{store.rs,service.rs,projection.rs}`,
`modules/inventory/src/tests.rs`, `docs/todo/inventory.md` and `docs/todo/inventory-done.md`
(only if the known failing test changes state).
**(b) Why now:** this is a single-key shape, and it changes autocommits into one transaction.
**(c) How:**
- Add an `inventory.player/` key and a `Store::lock_player_tx`, separate from
  `inventory.character/`.
- `Holdings::grant`, after the dev gate and validation, runs one transaction:
  1. `begin`;
  2. `item_exists` (its answer stays first, as today);
  3. `lock_player_tx`;
  4. `is_player_purged`;
  5. `grant_exec`;
  6. `commit`;
  7. then `list`.

  Every answer stays the same as today. Every non-commit return rolls back explicitly.
- Delete `Store::is_player_purged`, `Store::grant_pool` and `Store::item_exists`, the
  pool-autocommit forms. Replace them with transaction-scoped forms if those don't already exist.
  Move every test caller to the transaction-scoped forms in this step; there is no second rail.
- Player sweep: `INVENTORY_PLAYERS_PER_FIRE = 64` as the single claim, with no loop and no
  floor. Take the lock before `clear_owner_exec`.
- Fix the false docs: `service.rs:94-103`, `projection.rs:261-262` and the
  `is_player_purged_tx` "no advisory lock" doc.
- **Tests:**
  - Race 1 and Race 2 on the player-owned grant.
  - Fire bound.
  - Move the existing sweep tests to fixed low ids. Run the sweep in
    `sweep_clears_player_owned_holdings_but_keeps_character_owned_rows` inside a transaction.
  - Re-seed through `grant_exec` on a transaction or through `Holdings::grant`.
  - **Known failing test:** re-check
    `sweep_handler_survivor_untouched_and_ignores_foreign_schedule_name`, which
    `docs/todo/inventory.md` tracks.
    - If this step fixes it, move the item to the done file in this commit.
    - Otherwise leave the item and report the test's state.
  - **Mutations, each shown red:** grant lock removed, sweep lock removed, limit raised.
- Run build, clippy, then `cargo test -p inventory` three times.

**Comments:** none new.
**(d) Lane:** `core-implementer`, opus.

Commit: `fix(inventory): serialize player grants and the purge sweep per player (purge-race-locks step 5)`.

### Step 6 — groups lock and race tests `[opus]`
**(a) What:** `modules/groups/src/{store.rs,service.rs,projection.rs}`,
`modules/groups/src/purge_tests.rs`, `modules/groups/src/tests.rs`,
`docs/todo/account-deletion.md`, `docs/todo/account-deletion-done.md`.
**(b) Why last:** it is the only module with two lock kinds, so it needs the P-before-G order. It
closes the race in the last module, so it also closes ACCOUNT-DELETION-05.
**(c) How:**
- Add a `groups.player/` key and `Store::lock_player_tx`.
- Writer lock order:
  - `create`: P(me) → guard → insert.
  - `join`: P(me) → G → guard → insert.
  - `invite`: P(target) → G → `visible_role_tx` → `is_player_purged_tx(target)` → `counts_tx`
    → insert. The target guard runs before the capacity check, and gives exactly the
    directory-miss answer (Invariant 4).
  - `respond`, `decide` and `leave` are unchanged; they take G only.
- Sweep: one claim with `GROUPS_PLAYERS_PER_FIRE = 64` (no loop, no `PURGE_FLOOR`). Then take P
  for EVERY claimed player, in claim order, before any G and outside every `purge_group`
  SAVEPOINT. Then run the existing per-player, per-group purge. `GROUPS_PER_FIRE` still bounds
  the number of groups.
- Every new non-commit return rolls back explicitly.
- Fix the false docs: `store.rs:147-148`, `service.rs:29-33` and the `is_player_purged_tx`
  "no advisory lock" doc.
- **Tests:**
  - **Race 1** (writer first): `create`, `join` (the first membership in an existing group) and
    `invite` (target tombstoned), each in flight, with the sweep parking. For `create`, assert
    the outcome the code produces and explain it.
  - **Race 2** (sweep first): `join` and `invite` park behind an open sweep. They then answer
    deleted and directory-miss respectively.
  - **The former deadlock shape:**
    - Fixed ids a < b; a is in G1 and is being purged.
    - Tombstoned b has an in-flight `join(G1)`.
    - Use a deterministic park point.
    - Assert no deadlock error and that both players are purged.
  - `invite` to a full group with a purged target gets the directory-miss answer, not "full".
  - Fire bound.
  - Existing sweep tests stay on the low `fixed_uuid` tiers, with the claim precondition. The
    65-group cap test stays green.
  - **Mutations, each shown red:**
    - P taken after G in the sweep: the deadlock test must catch it;
    - `join` P removed;
    - `invite` guard removed;
    - `invite` guard moved after `counts_tx`;
    - limit raised.
- Run build, clippy, then `cargo test -p groups` three times.
- Close ACCOUNT-DELETION-05: move it to `docs/todo/account-deletion-done.md` with ref
  `purge-race-locks step 6` in this commit.
- In the report:
  - write out each path's lock sequence and confirm that no path takes P after G;
  - list the Skipped head-of-line limit (Invariant 6) under **Leftovers for owner**.

  The lead writes it to `docs/todo/groups.md` only if the owner defers it.
- If the step runs past about 60 tool calls, hand off at a part boundary (code, then tests). The
  review runs on the step's cumulative diff.

**Comments:** one line on the sweep's lock phase: all P before any G, outside `purge_group`.
**(d) Lane:** `core-implementer`, opus.

Commit: `fix(groups): serialize create, join and invite with the purge sweep per player (purge-race-locks step 6)`.

### Step 7 — cadence prose `[docs]`
**(a) What:** every `player-purge` cadence statement that step 1 made false, plus the historical
plan's erratum. The exact files are listed under (c).
**(b) Why last:** the prose describes landed code.
**(c) How:**
- **Cadence statements** (one day becomes five minutes):
  - the `PLAYER_PURGE` contract doc, `api/scheduler/events/src/lib.rs:74-81`;
  - "shared daily `player-purge`" in `modules/{characters/src/lib.rs:245, friends/src/lib.rs:206,
    groups/src/lib.rs:198, inventory/src/lib.rs:288, storage/src/lib.rs:129,
    wallet/src/lib.rs:310}`;
  - `modules/storage/src/projection.rs:27,39-41`;
  - `modules/wallet/src/projection.rs:148`;
  - `modules/groups/src/projection.rs:218,237`;
  - `modules/characters/src/projection.rs:20` and `modules/friends/src/projection.rs:20`;
  - `modules/scheduler/src/lib.rs:12` (module header);
  - `tools/splitproof/src/main.rs:2682,3074`;
  - `.agents/shared/gamebackend.md:250,451-452`;
  - `docs/reference/event-plane-ops.md:101`;
  - `README.md:140` ("daily sweep");
  - `.claude/rules/domain-modules.md`, accounts bullet ("a daily `player-purge` sweep");
  - `docs/roadmap/feature-tracker.md:145,256`.

  Any statement in this list that an earlier step already corrected is skipped. Leave the 86400 s
  *prune* schedules alone.
- Any false comment that steps 2–6 reported outside their files.
- `docs/reference/module-reference.md`: the "Do NOT copy" notes.
- **Historical plan:** add an erratum line to
  `docs/plans/2026-09-10-1917-account-deletion-purge-plan.md`. Do not rewrite it.
- Do not edit `docs/todo/`. Step 6 closes ACCOUNT-DELETION-05, and the lead writes any deferred
  leftover.

**(d) Lane:** `docs-writer`, sonnet.

Commit: `docs(account-deletion,scheduler,splitproof): describe the five-minute player-purge cadence (purge-race-locks step 7)`.

## Verification / review

- **Reviews:**
  - A fresh `core-reviewer` (opus, ≥ the author's tier) reviews each of steps 2–6, on the step
    commit plus the cumulative diff from the step's start commit. A review is never resumed.
  - Each task gets one review round. None of these steps touches `core/`, split/edge wiring or a
    verify gate, so there is no second round.
  - Step 1 is under the threshold and gets no review.
  - In step 7, only the `api/scheduler/events` contract doc comment counts as code. It gets one
    `core-reviewer` pass; the rest of step 7 is prose and is not reviewed.
  - **`proof-auditor` (opus, ≥ the author's tier), in batches.** The owner ruled on 2026-11-18
    that the concurrency tests are the risk surface. Each batch audits the tests in its steps'
    commits, including any review-fix commits:
    - after step 3: steps 2 and 3 (characters, friends);
    - after step 5: steps 4 and 5 (notifications, inventory);
    - after step 6: step 6 (groups).

    It checks that:
    - every race test parks on the key it claims (the key-matched `wait_advisory_parked`);
    - every listed mutation was actually shown red;
    - no test passes by timing;
    - the order and deadlock tests would see 40P01.

    A batch does not hold back the next implementation, because the files are disjoint. A local
    finding gets a fix commit carrying `Kind: review-fix` in the audited module, made after any
    in-flight step has landed. A structural finding pauses the plan until the owner rules. An
    auditor is never resumed.
  - A structural finding pauses the plan until the owner rules.
- **Cargo discipline:** follow the `safe-verification` skill. Run one cargo command at a time,
  each preceded by its own `tasklist` check.
- **Final checks:**
  - `cargo test --workspace --no-fail-fast`;
  - `cargo run -p archcheck`;
  - `cargo audit` with both ignores.
- **Topology:** the locks are database-global and topology-blind, and the split runs the same
  service code behind the RPC stub. Splitproof already forces `player-purge` due. No verifyctl
  runs until the admin panel lands.
- **Known, not fixed here:** with `WORKERS = 2`, a notifications delivery parked on a player lock
  and a running sweep can occupy both workers of one process for the length of a fire. A sweep
  that overran its 5 s budget could push the parked delivery past its 10 s timeout and add one
  failure toward a pause. This only happens for deleted recipients.
- **Owner action (reported by step 1):** an existing database keeps `interval_seconds = 86400`
  until its scheduler schema is wiped or that one row is updated.

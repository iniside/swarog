# Plan: `wallet` — virtual currency balances + append-only ledger

*Sequence step 1 of the feature tracker ([docs/roadmap/feature-tracker.md](../roadmap/feature-tracker.md)).
Closes P0#3 of [game-backend-feature-gaps.md](../reference/game-backend-feature-gaps.md).*

*Revision 4 — folds in the core-reviewer pass over the landed Step 1 (widened idempotency identity, movement + balance caps). Revision 3 added the **optional starter grant** (Steps 6-7) at the user's call, and
folds it into the design at source (Step 2's service shape, Step 4's wiring) rather than
bolting it on. Revision 2 applied the `core-reviewer` punch list; both sets of changes are
itemised at the bottom.*

---

## Context — why a new fortress, and why not extend an existing one

Per the Research-before-planning rule, the overlapping systems and the explicit
"why not extend X":

| Candidate | What it does | Why NOT extend it |
|---|---|---|
| **`inventory`** | per-character holdings of catalog items, `bigint` quantity with a `CHECK (quantity >= 0 AND <= 2000000)` | Closest twin, and the gap doc calls our "coin" item a fake stand-in. Rejected for three concrete reasons: (1) holdings are keyed by **owner (player\|character)** + item, but currency is per-**player** only — sharing the table means a character-scoped row is representable and meaningless; (2) inventory has **no ledger** — a balance with no auditable history is not a wallet, and retrofitting an append-only ledger onto `inventory.holdings` changes the module's contract for every existing consumer; (3) `Holdings::grant` is documented in-code as **"NOT a reference-grade mutation pattern"** (`modules/inventory/src/service.rs:89-98`): three separate pool autocommits, no idempotency key, accumulating `ON CONFLICT … quantity + EXCLUDED.quantity`. Money must not inherit that. Its `grant_starter` projection, by contrast, IS the reference — Step 6 copies it. |
| **`config`** | DB knobs, monotonic revision, live-reload | Currency *definitions* are domain data with referential integrity and admin CRUD, not process knobs — they stay in wallet's schema. But the **starter-grant policy** (which currency, how much) is exactly a knob, and Step 6 reads it from `config`, the way inventory reads `inventory/starter_item`+`starter_qty`. |
| **`match`** | `report` with a REQUIRED `ReportId`, dup-same → 202 no-op, dup-different → 409 | The pattern source: the repo's only correct mutating-idempotency exemplar, including the "row disappeared" arm at `modules/match/src/lib.rs:210`. |
| **`rating` / `leaderboard`** | blind relative increments inside a delivery tx | Same accumulate shape, safe *only* because increment and cursor advance share the delivery tx. Wallet's client-facing path has no delivery tx, hence its own idempotency key. |

**Decision recorded 2026-07-28 (reading A):** the wallet module **owns the currency
catalog in its own schema**. A module that wants the currency types asks over RPC
(`walletapi::Wallet::currencies`). Rejected alternative: modules declaring their own
currencies via a contribution slot — it splits the ledger's authority for a seam we do
not need yet.

**Scope boundary.** No store, no IAP, no rewards engine. Wallet exposes: read balances
(player + wire), read currencies, credit/debit as a wire capability + an admin action, and
an **optional, config-driven starter grant** on `player.registered`. There is deliberately
**no player-facing mutation** — a client can never move its own money.

---

## Design decisions (settled here, not during implementation)

### D1 — Wire method names must not collide

Wire method = `format!("{prefix}.{}", to_lower_camel(name))` (`tools/rpc-macro/src/lib.rs:163`),
and `handle`/`handle_identity` → `assert_unregistered` **panics** on a duplicate
(`core/edge/src/server.rs:139-155`). Both wallet traits share `prefix = "wallet"`, so
method names are chosen globally distinct:

| Trait | Method | Wire method | Registry key |
|---|---|---|---|
| `walletapi::Wallet` (wire-only) | `balances`, `currencies`, `credit`, `debit` | `wallet.balances`, `wallet.currencies`, `wallet.credit`, `wallet.debit` | `wallet.wallet` |
| `walletapi::Player` (`#[http]`) | `my_balances`, `list_currencies` | `wallet.myBalances`, `wallet.listCurrencies` | `wallet.player` |

(`registry::key(prefix, to_snake(TraitName))` — `tools/rpc-macro/src/lib.rs:475-486`.
`wallet.wallet` mirrors the existing `leaderboard.leaderboard`.)

### D2 — Amounts are `i64` minor units; balances are non-negative by DB CHECK

`amount bigint NOT NULL CHECK (amount >= 0 AND amount <= 1e15)` named `balances_amount_check`
(both bounds — see below). Insufficient
funds is **SQLSTATE 23514 with that exact constraint name** → `Error::conflict` → 409,
matched as narrowly as inventory matches `holdings_quantity_check`
(`modules/inventory/src/store.rs:17-21`). The DB is the authority; the service does not
pre-check-then-write.

`bigint`, never `int4`: an int4 overflow (22003) inside a delivery tx once poison-paused a
subscription (`modules/inventory/src/lib.rs:50-59`).

**Both ends of the range are capped, and that is what keeps `bigint` overflow (22003) out of
the delivery transaction (rev 4).** `MAX_MOVEMENT_AMOUNT = 1_000_000_000_000` (contract const,
beside the byte caps) bounds a single movement; the balance CHECK is
`amount >= 0 AND amount <= 1_000_000_000_000_000`. Both are far below `i64::MAX ≈ 9.2e18`, so
`amount + delta` can never overflow — a credit past the ceiling hits 23514, which is already
mapped to 409, instead of 22003, which is not mapped at all. This matters most on the
**delivery** path — and the mechanism there has **two arms, which earlier revisions of this
plan conflated** (corrected after reading `core/asyncevents/src/worker.rs`):

- the handler returns `Err` ⇒ the plane runs `ROLLBACK TO SAVEPOINT deliver`
  (`worker.rs:257` sets it, `:288` uses it), records the failure, commits and backs off;
  after 20 consecutive failures the subscription **pauses**. No 25P02 — the savepoint
  restores the transaction.
- the handler **swallows** the DB error and returns `Ok` — which is exactly what posture A
  mandates for a data-quality problem — ⇒ the transaction is still aborted, so the plane's
  checkpoint `UPDATE` (`worker.rs:268`) fails with **25P02** and the delivery step errors.

Both outcomes are bad and the second is the nastier one, because it is what the mandated
posture produces. That is precisely why the pre-check is required rather than optional: it
is the only way to satisfy "never `Err` on a data problem" AND "never leave the delivery tx
aborted" at the same time. Same overflow class that already bit inventory once
(`modules/inventory/src/lib.rs:50-59`). `validate_movement` rejects
`amount <= 0 || amount > MAX_MOVEMENT_AMOUNT` as `Invalid`, and D9's config read clamps
against the same const.

**Three** SQLSTATEs are interpreted: `23514` (insufficient funds **or** balance ceiling → 409), `23503` (unknown
currency → 400), `22P02` (malformed player uuid → 400). The contract carries
`player_id: String` while the columns are `uuid`, so **every statement casts `$n::uuid`**
and the 22P02 arm exists — the house pattern at `modules/characters/src/store.rs:28` and
`:53,:65,:78,:104,:136`. Without the cast sqlx binds text against a uuid parameter; with
the cast but no 22P02 arm a malformed id is a 500 instead of a 400.

### D3 — Idempotency: the `match::report` construction, corrected for a value-returning method

Every mutating call carries a REQUIRED `idempotency_key` (≤128 bytes, non-empty).
`wallet.ledger` has `UNIQUE (idempotency_key)`. The movement logic runs as
**`apply_on(conn, movement, sign)`** — see D8 — and never opens or closes a transaction
itself. **`apply_on` validates the movement itself (corrected after review):** the sign and
range guard is the contract's central promise ("`amount` is ALWAYS POSITIVE — the direction
is the method"), so it belongs in the authority, not in one of its two callers. An earlier
draft put `validate_movement` only in the pool wrapper, reasoning that the delivery path
must skip rather than `Err` — but D9's handler already pre-checks the amount **before**
calling `apply_on`, so validating inside it costs the delivery path nothing and closes the
hole where Step 8's admin `grant` with `amount = -500` would debit a balance and publish a
negative delta on a grant. The wrapper's pre-call validation stays, as the
reject-before-opening-a-transaction optimisation it always should have been:

1. `INSERT INTO wallet.ledger (idempotency_key, player_id, currency, delta, reason, balance_after)
   VALUES ($1,$2::uuid,$3,$4,$5,0) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id::text`
2. `None` ⇒ the key is already used. Re-`SELECT player_id::text, currency, delta, reason,
   balance_after FROM wallet.ledger WHERE idempotency_key = $1` **on the same connection**, then:
   - same `(player_id, currency, delta, reason)` → `Outcome::Duplicate(existing.balance_after)`;
   - different → `Outcome::Conflict`;
   - **no row** → `Error::internal("conflicting ledger row disappeared")` — the arm `match`
     also has (`modules/match/src/lib.rs:210`). Unreachable under READ COMMITTED; must not
     be an `unwrap`.
3. `Some(id)` ⇒ move the balance by the signed delta. **This step was specified wrong in
   revisions 1-4 and the error shipped; corrected here after Step 5's tests caught it.**

   The original shape was a single `INSERT … ON CONFLICT (player_id, currency) DO UPDATE SET
   amount = wallet.balances.amount + $3 … RETURNING amount`, with "a debit passes a negative
   `$3`". That is broken for **every** debit: Postgres validates the table's CHECK against the
   **tentative INSERT row** before it detects the conflict and routes to `DO UPDATE`, so a
   negative `$3` trips `balances_amount_check` regardless of the existing balance. Proven
   directly — balance 100, debit 30, `ERROR: new row … violates check constraint
   "balances_amount_check" DETAIL: Failing row contains (…, -30)`, while a plain `UPDATE`
   yields 70. Three earlier passes missed it because every probe exercised a debit against a
   *missing* row, where 23514 IS the right answer.

   The corrected shape is UPDATE-first, INSERT-as-fallback:
   ```sql
   UPDATE wallet.balances SET amount = amount + $3, updated_at = now()
    WHERE player_id = $1::uuid AND currency = $2
    RETURNING amount
   ```
   and only when that reports zero rows:
   ```sql
   INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid,$2,$3)
    ON CONFLICT (player_id, currency) DO UPDATE
      SET amount = wallet.balances.amount + $3, updated_at = now()
    RETURNING amount
   ```
   The UPDATE path evaluates the CHECK on the RESULTING row, which is the intended semantics:
   insufficient funds and the ceiling both surface as 23514 → 409. The fallback keeps the
   previously-correct missing-row behaviour, including 23503 → 400 for an unknown currency,
   and stays race-safe for a credit (two concurrent first-ever credits both miss the UPDATE,
   one INSERTs, the other takes `DO UPDATE`).
4. `UPDATE wallet.ledger SET balance_after = $amount WHERE id = $id::uuid`
5. `emit_tx(AnyTx::new(&mut *conn), &walletevents::CHANGED, &evt)` — **only on this branch**,
   never on the duplicate branch.
6. `Outcome::Applied(amount)`

**Why the duplicate arm returns the stored `balance_after`, not a fresh balance read.** The
method returns `i64`. If the replay re-read the *current* balance, a movement landing in
between would make the replay return a different value than the original call — which is
exactly what would invalidate D4. Returning the ledger row's own `balance_after` makes a
replay observationally identical to the original.

**Why `reason` is IN the comparison (rev 4).** The published contract doc says a duplicate
key carrying "the same movement" replays and a different one is 409. Comparing only
`(player_id, currency, delta)` would make `credit(K, 100, "promo")` followed by
`credit(K, 100, "refund")` a silent success that records the FIRST reason — the caller gets
a balance for a movement it did not describe, with no error. Widening the tuple keeps the
contract text true: the key identifies the whole movement. A genuine wire replay carries an
identical payload and still collapses to `Duplicate`; only an *edited* resubmit gets 409,
which is correct — it is a different movement and deserves its own key.

**The idempotency-key namespace is GLOBAL to the wallet, and the contract must say so
(added after review).** `UNIQUE (idempotency_key)` carries no `player_id`, so a caller that
mints one key per *business event* (`"season-3-payout-batch-7"`) and credits 200 players
under it gets one `Applied` and 199 × 409 — 199 players silently unpaid. Keeping the
constraint global is deliberate: a cross-player key collision is a caller bug and should
surface loudly rather than be silently absorbed. But that only works if the contract states
the scope, so `Movement::idempotency_key`'s doc must say the namespace is the whole wallet
and callers key per `(player, business event)` — which D9's `starter:{player_id}` already
does correctly. Do NOT quietly widen the constraint to `(player_id, idempotency_key)`; that
changes what a Conflict means and would need its own recorded decision.

**Why the ledger insert is first:** it is the dedup gate. If the balance moved first, a
duplicate key would be detected only *after* double-spending.

**Why a rejected debit is safe:** the CHECK violation aborts the transaction, so the ledger
row disappears and the key is **not** consumed — the caller may retry after topping up.

**Isolation dependency (explicit).** Step 2's re-`SELECT` observes committed data **only
under READ COMMITTED**, where each statement takes a fresh snapshot after the INSERT
finished waiting on the conflicting transaction. Under REPEATABLE READ it would return
`None` and take the arm above. Postgres' default is READ COMMITTED and nothing here changes
it; the arm exists so the assumption fails loudly rather than silently.

**Aborted-transaction rule (explicit).** After 23514 or 23503 the transaction is aborted:
**any** further statement fails with `25P02` until ROLLBACK. On those two arms the only
legal next action is to unwind. If a richer 409 message ever needs the current balance, it
must be read from the pool **after** the rollback — never on the aborted connection.
Without this rule an implementer enriching the "insufficient funds" message turns a 409
into a confusing 500. **This is also why the delivery path (D8) must never reach those
arms** — an aborted delivery tx cannot commit its checkpoint, so it would poison the
subscription.

Every no-op and rejection arm on the pool path calls `tx.rollback()` **explicitly**; a
dropped sqlx tx defers its ROLLBACK and holds row locks
(`modules/characters/src/service.rs:150-156`, `modules/match/src/lib.rs:215-218`).

**No advisory lock.** Unlike `characters::create` (count-then-write) this is a single
`UPDATE … amount + $delta` — the row lock serializes concurrent movements and the CHECK
rejects the loser.

### D4 — `#[retry_safe]` on credit/debit, and exactly what earns it

Reads are `#[retry_safe]` trivially. `credit`/`debit` also get it, but the justification is
**not** "match does it": `matchapi::Match::report` returns `Result<(), Error>`
(`api/match/api/src/lib.rs:47-48`), so a replay has no value that can diverge. Wallet
returns `i64`. The attribute is legal **only because of D3's duplicate arm returning the
stored `balance_after`** — that is what makes a replay after reconnect observationally
identical. `RetryMode::OnceAfterReconnect` is selected purely from `m.retry_safe` at four
generation sites (`tools/rpc-macro/src/lib.rs:565, 757, 809, 885`), so the replay is real.

If the key ever becomes optional, or the duplicate arm ever re-reads the live balance, the
attribute must come off in the same diff. Step 5 test #2 pins it.

### D5 — `wallet.changed` ships with a consumer in the same rollout

`ALLOW_UNSUBSCRIBED` is empty (`tools/topiccheck/src/main.rs:78`) and the blocking fortress
stage runs `--durability-strict` (`tools/verifyctl/src/stages/fortress.rs:43`), so a
defined-but-unsubscribed topic FAILs. Step 3 adds `wallet.changed` as audit's **7th ledger
sink** (`audit.wallet-changed.v1`, `on_tx_raw`, `StartPosition::Genesis`) — genuinely
wanted, and it removes the exception entirely.

**The gating authority is `defined_topics()`** (`tools/topiccheck/src/main.rs:183-200`), a
hand-maintained list edited in Step 9 — **not** the `bus::define` call. So the real ordering
constraint is *Step 3 before Step 9*.

`HistoryPolicy::MinRetention { days: 30 }`, matching `AUDIT_RETENTION_DAYS`'s default.
**Immutable after first emit** (`core/asyncevents/src/store.rs:440-462`). The authority for
money history is `wallet.ledger` — a real table we retain — so 30 days is right and
`KeepForever` would be hoarding.

### D6 — Currency catalog is data; dev seeding is an explicit opt-in

`wallet.currencies(code, display_name, kind, decimals, created_at)`; balances carry
`currency text NOT NULL REFERENCES wallet.currencies(code)` — an **in-module** FK (legal;
`cross_schema_fk_violations` compares against the module's own directory name). Unknown
currency → 23503 → `Error::invalid` → 400.

`WALLET_DEV_SEED` (explicit-only, default OFF, self-healing upsert, loud warn when ON)
seeds `gold` (soft) and `gems` (hard), the `APIKEYS_DEV_SEED` shape
(`modules/apikeys/src/lib.rs:237-307`). It must be wired in **three** fleet sites — Step 4.

No `revision` column: an unused CAS field for a future this plan descopes is gold-plating.

### D7 — Ports and the Postgres session budget

Next free pair: **HTTP 8092, edge 9010** (max in use 8091/9009,
`tools/processctl/src/fleet.rs:491`).

`PG_SESSION_BUDGET = 97 - 10 = 87` (`fleet.rs:35,45`); each DB-backed split svc reserves
`SPLIT_SERVICE_POOL_MAX(3) + PLANE_DEDICATED_SESSIONS(4) = 7`, scheduler 8, current total
**78**. Wallet-svc makes it **85 of 87** — two sessions of headroom, which the **13th**
DB-backed process breaks. Known gap 5. The stale comment "11 DB-backed processes share one
local Postgres" (`fleet.rs:79`) becomes false and is fixed in the same rollout.

### D8 — One movement authority, two callers: pool tx and handed delivery tx

The starter grant (D9) credits from **inside the plane's delivery transaction**, while
credit/debit credit from their **own pool transaction**. Two code paths for the same money
movement would be the classic duplicated authority, so the movement logic is written **once**:

```rust
enum Outcome { Applied(i64), Duplicate(i64), Conflict }

// The authority. Runs D3 steps 1-5 on a caller-owned connection.
// NEVER begins, commits or rolls back — transaction control belongs to the caller.
async fn apply_on(&self, conn: &mut PgConnection, m: &Movement, sign: i64) -> Result<Outcome, Error>;
```

- **Pool path** (`credit`/`debit`): `pool.begin()` → `apply_on` → `Applied(v)`/`Duplicate(v)`
  → `commit`/`rollback` + `Ok(v)`; `Conflict` → `rollback` + 409.
- **Delivery path** (starter grant): `apply_on(delivery_conn, …)` → `Applied`/`Duplicate` →
  `Ok(())`; `Conflict` → **warn + `Ok(())`** (posture A, D9).

`credit` and `debit` differ only in the sign they pass. The authority is one function, not
two, and not one-plus-a-special-case.

### D9 — Optional starter grant on `player.registered`

**Opt-in by data, not by env.** Two `config` knobs, read through the injected
`configapi::Config` reader exactly as inventory reads its starter spec
(`modules/inventory/src/projection.rs:65-71`):

| namespace | key | type | compiled default | meaning |
|---|---|---|---|---|
| `wallet` | `starter_currency` | string | `""` | currency code to grant |
| `wallet` | `starter_amount` | int | `0` | minor units to grant |

Empty currency **or** amount ≤ 0 ⇒ **feature off**: the handler returns `Ok(())` having
done nothing. So the default build grants nothing, and enabling it is a config write
(live-reloadable, admin-editable) — no new env var, no code change. That is what "optional
but present" means here.

**One currency, not a list.** Config values are plain strings
(`configapi::Setting.value: String`); a JSON list would need its own parser, schema and
validation inside a durable handler. Single currency + amount matches inventory's single
starter item and is the minimal sufficient closure. A multi-currency starter is a natural
later extension of the same knobs.

**Idempotency key: `starter:{player_id}`.** Natural, deterministic, no UUID — a
redelivery, a replay, or a second `player.registered` for the same id all collapse to
`Outcome::Duplicate`.

**`StartPosition::AfterRegistration`, not `Genesis`.** Genesis would *promise* a retroactive
grant it cannot deliver: `player.registered` carries `MinRetention { days: 7 }`
(`api/accounts/events/src/lib.rs:37-38`), and with audit caught up the retention floor
advances, so registrations older than 7 days are already prunable. Coverage would be
"whoever happens to still be retained" — nondeterministic. `AfterRegistration` gives a
predictable contract (new players only); back-filling existing players is the admin grant
page, an explicit operator action rather than a surprise mass-credit. **This choice is
permanent for subscription id `wallet.player-registered.v1`** — `spec_hash` is immutable
(`core/asyncevents/src/catalog.rs:98-108`), so changing it later requires a new id.

**`apply_on` validates the movement itself, so the handler's pre-checks must be exhaustive
BY CONSTRUCTION, not by enumeration (corrected after the Step 2 review).** Validation lives
in the authority (D3), which means any movement the handler builds that `validate_movement`
would reject becomes an `Err` on the delivery path — and posture A forbids that. Enumerating
"check the amount and the currency" is not enough: `validate_movement` has seven reject
branches, and the review found a live gap where a 33-byte catalog currency passed
`currency_exists_tx` and then failed the contract's 32-byte cap. The closure is structural,
not a longer checklist: the catalog DDL now carries `CHECK (octet_length(code) <= 32)`, so a
row `validate_movement` would reject cannot exist; the amount is clamped by the handler; and
`reason` + `idempotency_key` are compile-time-fixed strings (`"starter-grant"`,
`"starter:{player_id}"`). With those four, every branch is unreachable by construction.
**Ordering is therefore a correctness constraint, not style: the amount clamp and
`currency_exists_tx` run BEFORE `apply_on`, never after.**

**Posture A — the handler must never poison its subscription.** A bad config value or a
missing currency is a property of the *config*, not of the event; returning `Err` would
back off and, after 20 failures, pause `wallet.player-registered.v1` for **every**
subsequent player (`core/asyncevents/src/worker.rs:88`). So, mirroring
`modules/inventory/src/projection.rs:115-129`:

- amount ≤ 0 / empty currency → skip silently (feature off);
- amount > `MAX_MOVEMENT_AMOUNT` → `warn!` + `Ok(())`. **The wire paths reject an out-of-range
  amount as 400; this path must not** — an `Err` here is what poisons the subscription, so the
  same value is a 400 on HTTP and a warn-and-skip on delivery. That asymmetry is posture A, not
  an inconsistency;
- currency **not present in `wallet.currencies`** → `warn!` + `Ok(())`;
- `Outcome::Conflict` (the key exists with different values — should be impossible) →
  `warn!` + `Ok(())`.

**The currency check is a pre-`SELECT` on the delivery connection, never a caught FK error.**
`SELECT 1 FROM wallet.currencies WHERE code = $1` on the same handed conn, exactly as
inventory pre-checks with `item_exists_exec`. This is not defensive style — it is required
by D3's aborted-transaction rule: letting the FK fire (23503) aborts the *delivery*
transaction, and the plane's checkpoint UPDATE afterwards would then fail with 25P02, i.e.
the subscription poisons on the very error we meant to tolerate. The pre-check plus
`amount > 0` removes every abort risk from the delivery path by construction.

**Ripples this creates** (folded into the steps, not appended):
- `WalletModule::requires()` → `vec!["config"]`, and `init` resolves
  `require::<dyn Config>(&key("config","reader"))` — in `init`, never `start`, or
  `requirecheck` cannot observe it (`tools/requirecheck/src/main.rs:28-34`).
- `cmd/wallet-svc` gains a `config` stub, and the fleet gains a `config-svc` dependency +
  `CONFIG_EDGE_ADDR` peer.
- `modules/wallet` depends on `accountsevents` (an events crate — importable by any module).

---

## Step 1 — Contract crates `api/wallet/{api,events,rpc}` + workspace registration

**(a) What.** New crates `walletapi`, `walletevents`, `walletrpc`; entries in the root
`Cargo.toml` `[workspace] members` (lines 92-117) and `[workspace.dependencies]` (286-311).

**(b) Why now.** Everything downstream names these crate paths.

**(c) How.**

`api/wallet/api/src/lib.rs` — deps exactly `opsapi`, `rpc-macro`, `async-trait`, `serde`,
`serde_json`. **Never** `tokio`/`sqlx`/`edge`/`remote` — `FORBIDDEN_API_DEPS`
(`tools/archcheck/src/main.rs:91-94`). **No `adminapi`** either (rev 4, item 4): wallet
only CONSUMES accounts' extension point, from `modules/wallet/src/admin.rs`.

```rust
pub struct Balance { pub currency: String, pub amount: i64 }
pub struct Currency { pub code: String, pub display_name: String, pub kind: String, pub decimals: i32 }
pub struct Movement {
    pub idempotency_key: String,
    pub player_id: String,
    pub currency: String,
    pub amount: i64,      // always positive; direction is the method
    pub reason: String,
}
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub const MAX_CURRENCY_CODE_BYTES: usize = 32;
pub const MAX_REASON_BYTES: usize = 256;
pub const MAX_MOVEMENT_AMOUNT: i64 = 1_000_000_000_000;   // rev 4, D2

#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Wallet: Send + Sync {
    #[retry_safe] async fn balances(&self, player_id: String) -> Result<Vec<Balance>, Error>;
    #[retry_safe] async fn currencies(&self) -> Result<Vec<Currency>, Error>;
    #[retry_safe] async fn credit(&self, movement: Movement) -> Result<i64, Error>;  // D4
    #[retry_safe] async fn debit(&self, movement: Movement) -> Result<i64, Error>;
}

#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Player: Send + Sync {
    #[http(verb = "GET", path = "/wallet/me", auth = "player", success = 200)]
    #[retry_safe] async fn my_balances(&self, identity: Identity) -> Result<Vec<Balance>, Error>;

    #[http(verb = "GET", path = "/wallet/currencies", auth = "player", success = 200)]
    #[retry_safe] async fn list_currencies(&self, identity: Identity) -> Result<Vec<Currency>, Error>;
}
```

`auth = "player"` **requires** a leading `opsapi::Identity`; `auth = "none"` **forbids** one
(`tools/rpc-contract-model/src/lib.rs:174-191`).

`api/wallet/events/src/lib.rs` — deps exactly `bus`, `serde`, `serde_json`:
```rust
pub struct Changed {
    pub player_id: String, pub currency: String,
    pub delta: i64, pub balance_after: i64,
    pub reason: String, pub ledger_id: String,
}
pub static CHANGED: LazyLock<EventType<Changed>> =
    LazyLock::new(|| define("wallet.changed", 1, HistoryPolicy::MinRetention { days: 30 }));

#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> { /* every field populated */ }
```
No `Option<…>` fields — `tools/topiccheck/src/golden.rs:295-338` would then demand a second
`None` sample.

`api/wallet/rpc/src/lib.rs` — glue and admin re-exports only:
```rust
use walletapi::*;
use opsapi::{Error, Identity};

walletapi::wallet_wallet_meta!(rpc_macro::generate_glue);
walletapi::wallet_player_meta!(rpc_macro::generate_glue);

pub use adminrpc::register_admin;            // Step 8
pub use adminrpc::register_admin_submit;     // Step 8
```
**No `remote_factories()` / `provide_factories()`** — zero callers: gateway-svc uses
`Stub::describe_peer`, admin-svc uses `adminrpc::admin_remote_factory`, cmd/server is local,
and no module consumes a wallet capability yet. Accounts has both only because it has both
kinds of consumer (`api/accounts/rpc/src/lib.rs:43-76`).

**(d) Dispatch.** `[opus]` — `core-implementer`, `model:"opus"`, effort **think hard**.

---

## Step 2 — `modules/wallet` (schema, store, service, module impl)

**(a) What.** New fortress: `lib.rs`, `store.rs`, `service.rs`, `conformance.rs`. No starter
grant (Step 6), no admin (Step 8), no tests (Step 5).

**(b) Why now.** The thing being built; Step 1 is its only prerequisite.

**(c) How.**

```sql
CREATE SCHEMA IF NOT EXISTS wallet;
CREATE TABLE IF NOT EXISTS wallet.currencies (
	code         text PRIMARY KEY
	             CONSTRAINT currencies_code_len_check CHECK (octet_length(code) <= 32),
	display_name text        NOT NULL,
	kind         text        NOT NULL DEFAULT 'soft',
	decimals     int         NOT NULL DEFAULT 0,
	created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS wallet.balances (
	player_id  uuid   NOT NULL,
	currency   text   NOT NULL REFERENCES wallet.currencies(code),
	amount     bigint NOT NULL DEFAULT 0
	           CONSTRAINT balances_amount_check
	           CHECK (amount >= 0 AND amount <= 1000000000000000),
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (player_id, currency)
);
CREATE TABLE IF NOT EXISTS wallet.ledger (
	seq             bigserial   NOT NULL,
	id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	idempotency_key text        NOT NULL,
	player_id       uuid        NOT NULL,
	currency        text        NOT NULL,
	delta           bigint      NOT NULL,
	balance_after   bigint      NOT NULL,
	reason          text        NOT NULL,
	at              timestamptz NOT NULL DEFAULT clock_timestamp(),
	UNIQUE (idempotency_key)
);
CREATE INDEX IF NOT EXISTS ledger_player_seq_idx ON wallet.ledger(player_id, seq DESC);
```

**Ordering is `seq`, and `seq` MUST be stamped while the balance row lock is held (corrected
after review).** `now()` is `transaction_timestamp()` — evaluated at tx start — so an
`at`-ordered read of an append-only ledger can show a non-monotonic running balance. But a
plain `bigserial` default does **not** fix that: it is assigned during the ledger INSERT
(D3 step 1), which is a whole round trip **before** the balance lock (D3 step 3). Two
concurrent credits can then interleave as: T1 inserts (`seq=1`), T2 inserts (`seq=2`), T2
takes the lock (`balance_after=100`), T1 updates (`balance_after=200`) — and a
`ORDER BY seq` read shows the running balance going *down* on a credit. So: stamp the
ordering value in the **same statement that runs under the lock**, i.e. in
`set_balance_after_tx` (`SET balance_after = $1, seq = nextval('wallet.ledger_seq_seq')`).
The insert-time default is then a placeholder that is always overwritten before commit; the
cost is one wasted sequence value per movement, which is nothing, and the gain is that
ledger order equals balance-application order per `(player, currency)`. Shipping the index
and this promise while implementing neither is the one thing that is not acceptable — the
admin drill-down is an auditor's view of money.

`store.rs` — house write/read split: **write methods take `&mut PgConnection`** (so the same
method serves the pool tx AND Step 6's delivery tx), reads take `&self.pool`. Methods:
`insert_ledger_tx`, `existing_ledger_tx`, `apply_balance_tx`, `set_balance_after_tx`,
`currency_exists_tx`, `list_balances`, `list_currencies`, `recent_ledger`,
`upsert_currency_tx`. Every statement casts `$n::uuid` for player ids (D2).

The three interpreted SQLSTATEs, matched narrowly:
```rust
fn is_insufficient(e: &sqlx::Error) -> bool {
    matches!(e.as_database_error(), Some(db)
        if db.code().as_deref() == Some("23514") && db.constraint() == Some("balances_amount_check"))
}
fn is_unknown_currency(e: &sqlx::Error) -> bool { /* 23503 */ }
fn is_invalid_uuid(e: &sqlx::Error) -> bool { /* 22P02 — cf. modules/characters/src/store.rs:28 */ }
```

`service.rs` — `validate_movement(&Movement) -> Result<(), Error>` that **every** writer
routes through (three byte caps, `1 ..= walletapi::MAX_MOVEMENT_AMOUNT`, non-empty key — **both**
bounds, not just the lower one; the published contract doc promises exactly this range and the
whole 22003 → 25P02 chain in D2 rests on the upper one), and the **`apply_on` authority
plus `Outcome` enum exactly as specified in D8** — written this way now, not refactored into
it in Step 6. `credit`/`debit` are the pool-path wrappers.

**`list_balances` returns EVERY row, including a zeroed one — and the contract doc must be
corrected to match in this same diff.** `walletapi`'s `Wallet::balances` currently promises
"every non-zero balance", which is a claim about code that does not exist yet: a debit down
to zero leaves the row. Filtering `amount > 0` on the wire would hide state the DB holds and
make `GET /wallet/me` disagree with the admin drill-down. So: no filter, and change that one
word in `api/wallet/api/src/lib.rs` (`Wallet::balances` doc) to say every balance row, a
player with no movements holding none.

`impl Player for Service`: `my_balances(identity)` reads `identity.player_id()` — never a
body field. The gateway verified the bearer and set `Identity::player(pid)` before dispatch
(`modules/gateway/src/lib.rs:840-853`).

`lib.rs` — `pub struct WalletModule { svc: OnceLock<Arc<Service>> }`:
- `name()` → `"wallet"`; **`requires()` → `vec!["config".into()]`** (D9's knobs; never
  declare metrics/DB/HTTP — process infrastructure).
- `register` (phase 1, no I/O): `Service::new(ctx.db()…, ctx.bus().clone())` — **the bus
  handle is required**, D3 step 5 emits through it and Step 5 #9 is unbuildable without it.
  Then `provide::<dyn Wallet>(key("wallet","wallet"), svc.clone())` and
  `provide::<dyn Player>(key("wallet","player"), svc)`. Resolve `WALLET_DEV_SEED` into a bool
  field here (env read once, like inventory's `dev_grant`).
- `migrate`: `sqlx::raw_sql(SCHEMA_DDL)`, then the dev seed iff the flag is on
  (`INSERT … ON CONFLICT (code) DO UPDATE`), plus the loud warn.
- `init` (wiring only, no I/O): resolve `require::<dyn Config>(&key("config","reader"))` into
  the service's `OnceLock` (**in `init`** — `requirecheck` observes only `register`+`init`,
  `tools/requirecheck/src/main.rs:28-34`); contribute the `Player` ops triple to
  `opsapi::{SLOT, BINDING_SLOT, LOCAL_SLOT}`; contribute `edge::EDGE_SLOT` **unconditionally**
  with `wallet_rpc::register_server` + `player_rpc::register_server`; contribute
  `opsapi::DESCRIBE_SLOT`.
- No `start`/`stop`.

`conformance.rs` — factual probes only: `conformance_idempotency_key_rejected(len)`,
`conformance_currency_code_rejected(len)`, `conformance_reason_rejected(len)`.

**Do NOT copy** (`docs/reference/module-reference.md:90-132`): inventory's three-autocommit
`grant`; an unbounded table with no retention answer — stated as known gap 1 rather than
left silent.

**(d) Dispatch.** `[opus]` — `core-implementer`, `model:"opus"`, effort **think hard**.

---

## Step 3 — audit consumes `wallet.changed` (7th ledger sink)

**(a) What.** THREE edits:
1. `modules/audit/src/lib.rs` — `"wallet.changed"` in the topic list (`:46-66`) with
   subscription id `audit.wallet-changed.v1`, `StartPosition::Genesis`, `on_tx_raw`.
2. `modules/audit/src/tests.rs:84-110` — `durable_topics_match_events` diffs `DURABLE_TOPICS`
   against a hardcoded `want` set of six producer contracts; a seventh **breaks this test**
   until `walletevents::CHANGED.topic()` joins `want`.
3. `modules/audit/Cargo.toml` `[dev-dependencies]` — add `walletevents`, or (2) does not compile.

**(b) Why now.** Before Step 9, which adds `wallet.changed` to `defined_topics()` — from that
moment `--durability-strict` requires a subscriber (D5).

**(c) How.** One entry in the existing zipped arrays; the handler is the generic
`RecordHandler`, which binds `delivery.event_id` **before** the downcast consumes
`delivery.tx` (`modules/audit/src/lib.rs:117-146`). Audit's non-dev deps are untouched — the
sink is raw JSON and stays zero-coupling.

**(d) Dispatch.** `[sonnet]` — `core-implementer`, `model:"sonnet"`, effort **think**.

---

## Step 4 — Process wiring: `cmd/wallet-svc`, monolith, gateway, admin-svc, fleets

**(a) What.** `cmd/wallet-svc/{Cargo.toml,src/lib.rs,src/main.rs}`; `cmd/server`;
`cmd/gateway-svc/src/{lib.rs,addrs.rs}` + `tests/boots.rs`; `cmd/admin-svc/src/lib.rs`;
`tools/checkmodules/src/lib.rs`; `tools/processctl/src/fleet.rs`; the `weles` fleet.

**(b) Why now.** The module is monolith-only until this lands — and `archcheck` **fails** the
moment `modules/wallet/` exists without `cmd/wallet-svc` (`SVC_EXEMPT_MODULES` empty,
`tools/archcheck/src/main.rs:81`, check at `:678-681`), and rule 17 fails the moment
`api/wallet/api` contains `#[http(` with no gateway stub (`:563, :769-778`).

**(c) How.**
- `cmd/wallet-svc/src/lib.rs` — **wallet consumes `config`, so the svc needs its stub**
  (without it, split boot dies in `init` on the `require`):
  ```rust
  vec![
      Box::new(metrics::Metrics::new()),
      Box::new(wallet::WalletModule::new()),
      Box::new(remote::Stub::new("config",
          wiring.peer_or("config", "127.0.0.1:9002"),
          configrpc::remote_factories())),
  ]
  ```
  `main.rs`: the characters-svc shape — `.with_peer("config", env_addr("CONFIG_EDGE_ADDR", "127.0.0.1:9002"))`,
  then `app::run(Config::from_env(), mods, Some(edge_server), None)`.
- `cmd/server/src/lib.rs`: `Box::new(wallet::WalletModule::new())` before the gateway entry
  (gateway stays last) + Cargo.toml dep.
- `cmd/gateway-svc/src/lib.rs`: `Stub::describe_peer("wallet", edge_peer(...))` —
  `describe_peer`, **not** `Stub::new`; the gateway consumes no wallet capability
  (`cmd/gateway-svc/src/lib.rs:74-104`).
- `cmd/gateway-svc/src/addrs.rs`: `AddrSpec{ env_key: "WALLET_EDGE_ADDR", provider: "wallet",
  class: Edge, env_default: "127.0.0.1:9010" }`; `"wallet"` added to the provider list in
  `cmd/gateway-svc/tests/boots.rs:41`.
- `cmd/admin-svc/src/lib.rs`: `admin_stub("wallet", wiring, "127.0.0.1:9010")` (Step 8's page).
- `tools/checkmodules/src/lib.rs`: `("wallet-svc", wallet_svc::modules(&w))` in the Split profile.
- `tools/processctl/src/fleet.rs`, **four edits**:
  1. `let mut wallet = service("wallet-svc", 8092, Some(9010), vec!["config-svc"]);` +
     `peer(&mut wallet.env, "CONFIG", 9002);` — the dependency is real, not cosmetic:
     `CachedConfig` is boot-fill-or-fail-startup, so wallet-svc cannot boot before config-svc.
  2. `wallet.env.insert("WALLET_DEV_SEED", "1")` + its `overrideable_env` entry, mirroring
     `apikeys` at `fleet.rs:576-583`.
  3. `game_backend_monolith` (`fleet.rs:628-648`) — `WALLET_DEV_SEED` in env, in the
     `overrideable_env` array, **and** in the `FleetFlavor::Proof` overlay. Without all three,
     `[WL1]` and its monolith parity re-run fail.
  4. Gateway's and admin's `dependencies` + peer loops (`peer(&mut gateway.env, "WALLET", 9010)`),
     and the stale `SPLIT_SERVICE_POOL_MAX` comment at `fleet.rs:79` corrected to 12.
- **`weles`.** `weles-managed-gateway` is a BLOCKING verify stage
  (`tools/verifyctl/src/stages/mod.rs:150-156`) booting a committed 12-process fixture:
  1. `weles/fleet.split.toml` — a `[[services]]` block for wallet-svc (name/pkg/provider,
     ports, `WALLET_DEV_SEED`, config peer), following `:52-54`.
  2. `weles/master/src/fleet_toml_tests.rs:868` — `assert_eq!(fleet.services.len(), 12, …)`
     becomes 13, message updated.
  3. `weles/master/src/manifest_tests.rs:100-140` — the `full_fleet_env_goldens` row.
  Skipping this leaves weles booting a fleet whose gateway's `WALLET_EDGE_ADDR` points at a
  dead port. **These three files must be edited — that is itself the proof they were
  load-bearing.**

**(d) Dispatch.** `[opus]` — `core-implementer`, `model:"opus"`, effort **think hard**.

---

## Step 5 — Unit tests for the core module (`[test-author]`)

**(a) What.** `modules/wallet/src/tests.rs`, `#[cfg(test)] mod tests;` at the bottom of
`lib.rs`. Covers landed Steps 2-4.

**(b) Why now.** Separate from and after the implementation it covers; before the starter
grant, so the movement authority is pinned before a second caller arrives.

**(c) How.** Fixture skeleton from `modules/characters/src/tests.rs:123-183`: `test_pool()`
(3s timeout, `eprintln!("SKIP: …")` + `return` when Postgres is unreachable),
`SCHEMA_READY: tokio::sync::OnceCell`, `unique_player()`, explicit `cleanup()` incl.
`asyncevents::testing::cleanup_events`. A `FakeConfig` implementing `configapi::Config` with
`Mutex` interior mutability, copied from `modules/inventory/src/tests.rs:31-61` (Step 7 needs
to mutate it mid-test). Dependencies injected through the **real registry key**.

1. `credit_then_debit_moves_balance_and_writes_ledger` — each ledger row's `balance_after`
   matches the balance at that point.
2. `duplicate_key_same_movement_returns_the_original_balance_after` — credit(K) → credit(other
   key) → credit(K); the third call returns the **first** call's value, not the current
   balance, and there is still one row for K. **Licenses `#[retry_safe]` (D4)**; a
   "re-read current balance" implementation fails it.
3. `duplicate_key_different_movement_is_409` — `Status::Conflict` + the message. **Includes a
   same-everything-but-`reason` case**, which pins rev 4's widened identity tuple: the narrow
   `(player, currency, delta)` comparison would return a silent success here.
4. `concurrent_same_key_credits_apply_once` — `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`,
   two spawned credits with one key; exactly one ledger row, single-application balance. The
   in-tx re-verify arm (D3 step 2) the sequential test never reaches.
**Neither 4b nor 4c may be dropped or "simplified" by the test lane — they are the only
things in the tree that will ever pin `41ebfa6`.** 4b must call `apply_on` DIRECTLY (driving
`credit`/`debit` cannot reach the new step 0, because the wrapper validates first), and 4c
must keep the two-connection interleaving.

4b. `apply_on_rejects_a_negative_or_zero_amount` — calls `Service::apply_on` **directly** on
   a pool connection with `Movement { amount: -500, .. }` and `sign = +1`; asserts
   `Status::Invalid`, zero `wallet.ledger` rows and an unchanged balance. A test that only
   drives `credit`/`debit` never reaches this branch — and the branch is what stops Step 8's
   admin `grant` form from debiting on a negative input.
4c. `ledger_seq_order_matches_balance_order` — two `apply_on` calls on one
   `(player, currency)` from two connections, interleaved so the second's ledger INSERT lands
   before the first's balance update; asserts `SELECT balance_after … ORDER BY seq` is
   non-decreasing for a credit-only sequence. Fails against an insert-time `bigserial`.
5. `debit_beyond_balance_is_409_and_consumes_no_key` — the CHECK arm; asserts the error is
   `Status::Conflict` and **not** `Internal` (the aborted-tx trap), then that the **same key
   succeeds after a top-up**.
6. `unknown_currency_is_400` — the 23503 arm.
7. `malformed_player_id_is_400` — the 22P02 arm; without the cast + arm this is a 500.
8. `credit_emits_wallet_changed_once` / `duplicate_credit_emits_no_second_event` — via
   `asyncevents::testing::events_count`.
9. `event_append_failure_rolls_back_the_balance` — `asyncevents::testing::failing_transport()`;
   balance unchanged AND no ledger row survives.
10. `validate_movement_rejects_oversized_fields` — no DB; the three byte caps **and the amount
    bounds** (`0`, negative, `i64::MIN`, `i64::MAX`, `MAX_MOVEMENT_AMOUNT + 1`), each asserting
    `Status::Invalid`.
    **Correction (rev 4 follow-up):** an earlier draft of this step claimed `i64::MAX` panics
    in debug on `sign * amount`. It does not — `-1 * i64::MAX` is representable. The value
    that panics on negation is **`i64::MIN`**, so that is the panic case; `i64::MAX` and
    `MAX_MOVEMENT_AMOUNT + 1` are ordinary bounds cases. Do not write a test asserting a panic
    for `i64::MAX`. The sign-independent defect the cap really closes is on the Postgres side:
    `amount + $delta` overflowing `bigint` → 22003 → the 25P02 poison chain (D2).

**(d) Dispatch.** `[test-author]`, `subagent_type: "test-author"`, `model:"sonnet"`,
effort **think hard**.

---

## Step 6 — Optional starter grant on `player.registered`

**(a) What.** `modules/wallet/src/projection.rs` + the subscription in `init`; deps
`accountsevents` (+ `configapi` already present from Step 2).

**(b) Why now.** After the movement authority (Step 2) exists and is pinned by tests (Step 5),
so the second caller reuses `apply_on` instead of growing a parallel path. Before the admin
page, which shares the same service.

**(c) How.** Per D9:

```rust
ctx.bus().on_tx(
    bus::SubscriptionSpec {
        id: "wallet.player-registered.v1",
        start: bus::StartPosition::AfterRegistration,
    },
    &accountsevents::PLAYER_REGISTERED,
    move |mut delivery, e: accountsevents::PlayerRegistered| {
        let granter = granter.clone();
        Box::pin(async move {
            let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
            granter.grant_starter(conn, &e.player_id).await
        })
    },
);
```

`grant_starter(conn, player_id)`:
1. `let (currency, amount) = self.starter_spec();` — `cfg.get_string("wallet","starter_currency","")`,
   `cfg.get_int("wallet","starter_amount",0)`, the shape of
   `modules/inventory/src/projection.rs:65-71`. **No wallet-owned second cache** — the
   injected reader is already a replica-local cache kept fresh by the invalidation plane.
2. `if currency.is_empty() || amount <= 0 { return Ok(()); }` — feature off, the default.
   `amount > MAX_MOVEMENT_AMOUNT` → `warn!` + `Ok(())`, never `Err`: the knob is an
   operator-editable string, so a fat-fingered `9223372036854775807` is a plausible input and
   must not poison the subscription (D2).
3. `if !store.currency_exists_tx(&mut *conn, &currency).await? { warn!(…); return Ok(()); }` —
   the pre-check that keeps the FK from ever firing inside the delivery tx (D9).
4. `apply_on(conn, &Movement{ idempotency_key: format!("starter:{player_id}"), player_id,
   currency, amount, reason: "starter-grant".into() }, +1)` — the same authority the HTTP
   path uses (D8), on the handed delivery connection so the credit, the ledger row, the
   `wallet.changed` append and the plane's checkpoint all commit together.
5. `Applied|Duplicate → Ok(())`; `Conflict → warn!(…) + Ok(())`. **Never `Err`** for a
   config- or data-quality problem (posture A).

The handler does not lock: `starter:{player_id}` is unique per player, and the balance
`UPDATE` is row-lock serialized. Unlike inventory there is no tombstone race — there is no
"player deleted" topic in this rollout.

**(d) Dispatch.** `[opus]` — `core-implementer`, `model:"opus"`, effort **think hard**.
Durable-handler posture and delivery-tx abort semantics are exactly the class that must not
go to a mechanical lane.

---

## Step 7 — Starter-grant tests (`[test-author]`)

**(a) What.** Added to `modules/wallet/src/tests.rs`. Covers landed Step 6.

**(b) Why now.** After Step 6 lands and compiles.

**(c) How.** The full-plane fixture from `modules/inventory/src/tests.rs:430-491`
(`asyncevents::Plane::new(pool, dsn)` → `Context::with_db_and_transport` → fakes provided →
`register`/`init` → `plane.start()` → `emit_tx` in a real tx → poll 50×100 ms → `plane.stop()`).
Run with `--test-threads=1` per [[asyncevents-single-invocation-parallelism-deadlocks]].

1. `starter_grant_credits_a_new_player` — config set, emit `player.registered`, poll for the
   balance; assert one ledger row with key `starter:{player_id}`.
2. `starter_grant_is_off_by_default` — the compiled defaults (`""`, `0`); assert **no** balance
   row and **no** ledger row. This is the "optional" half of the feature and the branch that
   would be wrong if the defaults ever became non-empty.
3a. `catalog_rejects_an_oversized_currency_code` — INSERT a 33-byte code into
   `wallet.currencies` and assert the DDL CHECK rejects it. This is the structural half of
   the by-construction argument: without it a catalog row exists that `validate_movement`
   would reject, and the grant handler `Err`s on the delivery path.
3b. `starter_grant_skips_an_absurd_configured_amount_without_poisoning` — `starter_amount`
   set to `i64::MAX`; assert no grant, no `Err`, and that a later registration still gets
   granted once the knob is corrected. The overflow arm from D2.
3. `starter_grant_skips_unknown_currency_without_poisoning` — config names a currency absent
   from `wallet.currencies`; assert no grant, **and** that a subsequent `player.registered`
   for a different player IS granted after the config is corrected on the mutable `FakeConfig`.
   The second half is the real proof: it fails if the handler returned `Err`, because the
   subscription would be backed off/paused.
4. `starter_grant_is_idempotent_across_redelivery` — call `grant_starter` twice on the same
   player (the redelivery shape); exactly one ledger row and a single-application balance.
5. `starter_grant_reflects_a_live_config_change` — mutate the `FakeConfig` between two
   registrations; the second player gets the new amount. Pins the "no second cache" decision.

**(d) Dispatch.** `[test-author]`, `subagent_type: "test-author"`, `model:"opus"`,
effort **think hard**. Escalated: the durable-plane fixture is not a copy-an-existing-pattern
job, and test #3's non-poisoning half is the subtle one.

---

## Step 8 — Admin page (`Wallet` under Economy) + extension entry

**(a) What.** `modules/wallet/src/admin.rs`: `impl adminapi::AdminData` +
`impl adminapi::AdminSubmit`, `adminapi::SLOT` contribution in `init`,
`register_admin`/`register_admin_submit` in the EDGE_SLOT closure, and an `ExtensionEntry`
into accounts' `PLAYERS_ROW_MENU`.

**(b) Why now.** After the service and after Step 4 (the remote path needs admin-svc's stub).
Before split-proof, which asserts the remote submit.

**(c) How.** Read view: currencies table + a player-scoped drill-down (`?player=<uuid>` →
balances + `recent_ledger`), modelled on `modules/characters/src/admin.rs`'s owner drill-down,
**not** apikeys' 6-action CRUD. Write view: one `Form` with an `_action` `Select` —
`create-currency` | `grant` | `revoke` — dispatched by a shared `apply_submit` used by BOTH
the local closure and the `AdminSubmit` impl, so the topologies cannot diverge
(`modules/apikeys/src/admin.rs:297-307`, `:479-511`).

**The idempotency key is minted at form RENDER time, in a hidden field** (alongside `_csrf`,
which `tools/splitproof/src/main.rs:222 extract_form_fields` already round-trips) — not at
submit time. A key minted per submit makes every retry a distinct key, so a double-click
double-grants: the exact defect the key exists to prevent.
`adminapi::AdminSubmit::admin_submit` is correctly **not** `#[retry_safe]`
(`api/admin/api/src/lib.rs:69-86`), so the wire never replays it — but a browser resubmit is
not a wire replay.

`ExtensionEntry` into `accountsapi::admin::PLAYERS_ROW_MENU` ("View Wallet"): the point
declares `context_keys: &["id", "name"]` (`api/accounts/api/src/lib.rs:118-122`), so the link
is **`?player={id}`**. `{player_id}` is not a declared key and `admincheck` would fail.

**There is NO mockup for this view — established 2026-07-30, and the dispatch lane changes
because of it.** `UILayout/GameOps Admin.dc.html` names `Economy & Store` in the sidebar
(line 68) but contains no designed content for it: the whole 753-line file renders one
screen (Players + character menu + inventory modal). Revisions 1-4 of this plan said "the UI
has an exact spec in UILayout; translate 1:1", which is simply false for this page.

Consequence: the page is **composed from the already-shipped `adminapi` widget vocabulary**
(itself the translation of that mockup's visual language), adding no new widget, CSS or
template construct. If the page needs a widget that does not exist, that is an additive
contract change and gets its own decision — not an invention inside this step.

Balances and ledger rows are **always real** — never invent money. This page has no data
gaps at all, since wallet owns everything it displays.

**(d) Dispatch.** `[opus]` — **`core-implementer`**, `model:"opus"`, effort **think hard**.
Revisions 1-4 tagged this `mockup-implementer`; with no mockup to implement against, this is
ordinary backend work (a module's `admin.rs`, a store read method, form dispatch) and the
mockup lane does not apply.

---

## Step 9 — Tool inventories, policies, baselines and docs

**(a) What.** The hand-maintained lists that gate the build:

| File | Edit |
|---|---|
| `tools/conformance/src/policy.rs` | `wallet()` entry + `entries()` row. `InputByteCaps` → `Stance::Applies` with the three real probes — **but do not overclaim**: the Step-8 review found `display_name` and `kind` (and an unbounded `decimals`) reach SQL from a remote `admin.adminSubmit` with no cap in Rust and no column CHECK, and `Params` is a `HashMap<String,String>` the input-inventory traversal never reaches. This is a class shared with `modules/apikeys`'s form strings, not a wallet regression — record it as a `KnownGap` beside the `Applies` stance rather than letting a green stance imply the admin-submit seam is capped. `EnvValidation` → NA ("WALLET_DEV_SEED is a boolean presence-gate, not a parsed value"). `ArgonParity` → NA. Plus one `input_policies()` row per wire input **leaf** — the traversal recurses request DTOs (`input_inventory.rs:116-158`). Add `wallet` to the tool's Cargo.toml. |
| **`tools/conformance/input-fields.golden.tsv`** | **Byte-compared, fails the BLOCKING conformance stage on any diff (`tools/conformance/src/main.rs:257-265`), and there is NO `--bless` writer — regenerate manually.** |
| `tools/topiccheck/src/main.rs` | `defined_topics()` += `of(walletevents::CHANGED.contract())` — the authority that arms `--durability-strict` (D5) |
| `tools/topiccheck/src/golden.rs` | `event_samples_by_crate()` += `("wallet", walletevents::golden_samples())`; `rpc_modules()` += the 5-tuple for `wallet_rpc` **and** `player_rpc` |
| `tools/opscatalog-gen/src/main.rs` | `rpc_modules()` += **two** entries, one per trait module (`:66-81`, completeness-gated by `rpc_modules_from_fs()` at `:162`) |
| `tools/csharp-client-gen/src/scrape.rs` | `PROVIDERS` += `"wallet"` and the `phase_a()` arm |
| `modules/apikeys/src/lib.rs` | `DEV_CLIENT_POLICY` += `wallet.myBalances,wallet.listCurrencies`. **Not** `wallet.credit`/`wallet.debit` — wire/admin only. |
| **`tools/conformance/src/tests.rs:294`** | `assert_eq!(discovered.len(), 18, …)` → **27**. Wallet adds 9 string leaves (`wallet.balances/player_id`, plus `movement.{idempotency_key,player_id,currency,reason}` on each of `wallet.credit`/`wallet.debit`). Missed in rev 3; it keeps the **blocking `test` stage** red. |
| **`tools/topiccheck/src/tests.rs`** | `defined_topics_matches_every_define_site_on_disk` FS-scans `api/*/events` for `define(` — satisfied by the `defined_topics()` row above, listed here so the red test is expected, not a surprise. |
| `tools/processctl/src/fleet_tests.rs` | assertion rows for wallet-svc's port/deps (incl. the `config-svc` dependency) |
| `CLAUDE.md` | "Domain modules (**11** fortresses + gateway)" → 12; the port roster gains `wallet :8092/:9010`; the accounts bullet gains "wallet grants starter currency on `player.registered` when configured". `docs-current` checks links/retired commands/package references (`tools/verifyctl/src/stages/docs_current.rs:6-31`), **not** this. |
| `docs/reference/public-api-baseline/` | `walletapi.txt`, `walletevents.txt` via `cargo run -p verifyctl -- --bless-public-api` |
| `docs/reference/contract-golden/contracts.txt` | re-bless via `cargo run -p verifyctl -- --bless-contract-golden` |

*Not needed:* `tools/archcheck/src/tests.rs` — `schema_set()` is a test fixture and the real
check derives the schema from the file path (`tools/archcheck/src/main.rs:928-992`); the
front-door list at `tests.rs:70-82` is a spot-check loop, not a registry.

**(b) Why now.** After every contract, route and subscription is final; before Step 10, whose
preflight fails on fleet drift and whose build depends on the C# fixture.

**(d) Dispatch.** `[sonnet]` — `core-implementer`, `model:"sonnet"`, effort **think**.

---

## Step 10 — Admin-path test + split-proof assertions (`[test-author]`)

**(a) What.** (i) A module test for the admin submit path; (ii) a `// --- Wallet ---` block in
`tools/splitproof/src/main.rs`.

**(b) Why now.** Last before verify: needs the real fleet, the admin page, the starter grant
and every tool inventory.

**(c) How.**

**Three landmines the Step-8 review found in this step's own instructions — read before writing:**
- `admin_render` uses `tokio::task::block_in_place`, which **panics** on a current-thread
  runtime. A plain `#[tokio::test]` that renders the form panics rather than fails. Use
  `#[tokio::test(flavor = "multi_thread")]` (the `modules/apikeys/src/admin_tests.rs:47`
  precedent) or call `admin_content_local` directly.
- **`[WL6]` cannot key on a status code.** `render_error` returns **200** and so does the
  internal-error path — 500 is unreachable from this page. Assert the exact message
  (`save failed: movement rejected: insufficient funds or balance ceiling exceeded`) and the
  unchanged balance. Do not add a "not 500" style check anywhere in the Wallet block; it
  would pass vacuously.
- `extract_form_fields` (`tools/splitproof/src/main.rs:224`) parses only `<input>`, so it
  captures `_idem_grant` but NOT the `_action`/`currency` `<select>`s — supply those by hand,
  as `[AD6b]` already does. `[WL4]` needs a **second GET** for a fresh key: reusing the first
  key with a different amount is a 409, not accumulation.

(i) `admin_double_submit_of_one_rendered_form_grants_once` in `modules/wallet/src/tests.rs` —
render the form, take its hidden idempotency field, submit the **same** values twice through
the shared `apply_submit`; assert one ledger row. A submit-time key fails it.

(ii) Split-proof, following the `[MT1]`-`[MT3]` shape (HTTP through gateway-svc `g`, DB assert
via sqlx, `poll_count(pool, sql, cid, want)` — helpers verified at
`tools/splitproof/src/main.rs:143, 222, 2372, 2480`):

- `[WL1] GET /wallet/currencies through gateway-svc -> 200 + seeded gold/gems`.
- `[WL2] GET /wallet/me with a real bearer -> 200` — `auth = "player"` end-to-end.
- `[WL3] admin grant in split credits the balance` — the admin portal's remote submit
  (admin-svc → wallet-svc over the internal edge), DB-asserted. **The cross-process mutating
  proof.**
- `[WL4] second grant with a fresh key accumulates`.
- `[WL5] wallet.changed reaches audit.log` — `poll_count` on
  `SELECT count(*) FROM audit.log WHERE topic='wallet.changed' AND payload->>'player_id'=$1`.
- `[WL6] revoke beyond balance is rejected, balance unchanged` — **corrected after Step 8.**
  The admin POST answers **200 with the verdict rendered** (`… insufficient funds or balance
  ceiling exceeded`), not 409. Reason: the portal's `render_conflict` hard-codes a
  reload-the-page message and drops the domain text, so mapping a balance problem there would
  give the operator the wrong remedy; `SubmitError::Conflict` is reserved for an idempotency
  conflict, where a fresh render minting a fresh key genuinely IS the remedy. Assert the
  message and the unchanged balance, not the status code.
- `[WL7] a newly registered player receives the configured starter grant` — **the config rows
  are written in the harness's pre-spawn setup**, before the fleet starts:
  `INSERT INTO config.settings (namespace,key,value) VALUES ('wallet','starter_currency','gold'),
  ('wallet','starter_amount','100') ON CONFLICT … DO UPDATE`. Pre-spawn, because `CachedConfig`
  is boot-fill-or-fail-startup — writing after boot would race the invalidation refresh against
  the registration, and this repo does not race clocks. Then register a fresh player through
  gateway-svc and `poll_count` the balance. This proves, in one assertion: cross-process durable
  delivery (accounts-svc emits → wallet-svc consumes), the config read, and the credit.
- Monolith parity: re-run `[WL1]`/`[WL2]`/`[WL7]` on `cmd/server`.

(iii) **The Step-8 follow-up (`d950791`) ships with NO execution path — five named module tests
in `modules/wallet/src/tests.rs` close that.** Nothing in the workspace calls `recent_ledger`,
constructs a `LedgerPage`, reads `truncated`, runs `ledger_note`/`player_content`, or takes the
`OnConflict::Skip` branch (`WALLET_DEV_SEED` is unset in the test env), and both catalog
statements are runtime-checked `sqlx::query`/`query_as` — so the tuple-arity change and the
`limit + 1` boundary are pinned by nothing today. Binding `limit` instead of `limit + 1`, or
`>=` instead of `>`, still passes 26/26.

1. Exactly `LEDGER_PAGE` (50) ledger rows for one player → `truncated == false` and
   `rows.len() == 50`. This is the arm that must NOT report truncation.
2. `LEDGER_PAGE + 1` rows → `truncated == true`, `rows.len() == 50`, the window is the NEWEST
   (`rows[0].seq > rows[49].seq`) and the dropped row is the OLDEST (its `seq` is absent from
   `rows`). Kills both the off-by-one and a `LIMIT` that happened to keep the wrong end.
3. `recent_ledger(pid, MAX_RECENT_LEDGER + 500)` against exactly `MAX_RECENT_LEDGER` rows →
   `truncated == false`. The clamp must not manufacture truncation out of its own ceiling.
4. **Seed idempotence — the branch F3 changed, which has no execution path today.**
   `write_currency_tx(…, OnConflict::Skip)` → `UPDATE wallet.currencies SET display_name = …`
   → run the seed write again → the edit SURVIVES; then `DELETE` the row and run the seed
   write again → the row is RECREATED. An `OnConflict::Overwrite` regression fails the first
   half; a seed reduced to a plain `INSERT` fails the second.
5. The rendered drill-down's `SEQ` column is strictly DECREASING, and deliberately NOT asserted
   contiguous — `seq` is monotonic-but-gapped by construction (see the comment at the table
   site). Pins P1's intent against a future "fix" that renumbers the column to look contiguous.

Every one of these mints its own currency via the existing `unique_currency` helper
(`modules/wallet/src/tests.rs:83`). After F3 the dev-seeded rows are no longer converging
fixture data — see the Step 8 follow-up errata.

(iv) **Added by the Step 9 review rollout** — two cases the landed code has no execution
path for:

6. **The dev key policy's NEGATIVE.** `modules/apikeys/src/lib.rs`'s `DEV_SEED_ROLES`
   `dev-client` policy must contain `wallet.myBalances`/`wallet.listCurrencies` and must NOT
   contain `wallet.credit` or `wallet.debit` — money must never be movable from a
   player-facing key. The sibling guard for `match.report` already exists at
   `modules/apikeys/src/tests.rs:26`; extend that test rather than writing a new one. A
   positive-only assertion passes even if someone pastes the full method list in.
7. **The catalog input caps** (closed in the same rollout — `admin::CATALOG_CAPS`,
   `currencies_display_name_len_check` / `currencies_kind_len_check` /
   `currencies_decimals_range_check`). Both levels, both directions:
   `apply_submit` with a `display_name` of `MAX_CURRENCY_DISPLAY_NAME_BYTES + 1` bytes (and
   the same for `kind`) → `Rejection::Rejected` whose message names THAT field, and no
   catalog row written; `decimals = 19` → `Rejected`, `decimals = 18` → accepted. Then the
   DB fail-safe on its own, in the shape of `catalog_rejects_an_oversized_currency_code`
   (`tests.rs:1259`): a direct `INSERT` past each cap is `23514` under the matching named
   constraint. Dropping either level must fail exactly one of these.

Idempotency (dup-same / dup-different) stays in Step 5 — a live fleet cannot deterministically
drive the conflict arm, and a test that cannot distinguish fixed from unfixed is not a proof.

**(d) Dispatch.** `[test-author]`, `subagent_type: "test-author"`, `model:"opus"`,
effort **think hard**.

---

## Step 11 — Verify, review, tracker update

**(c) How.**
1. **One rollout at a time** (MANDATORY): `pgrep -x cargo; pgrep -x rustc` → clear;
   `cargo run -p devctl -- status` → no active fleet; re-check, then exactly one
   `cargo run -p verifyctl -- --fast`.
2. **One independent adversarial pass** — `core-reviewer`, `model:"opus"`, effort
   **think hard**. Attack: D3's arms under concurrency, `apply_on`'s two callers and their
   differing transaction ownership, the delivery path's abort-freedom, posture A in
   `grant_starter`, the render-time idempotency key, the weles fixture.
3. `docs/roadmap/feature-tracker.md`: flip the wallet row to ✅ with module + commit sha, bump
   `Last update`, add the change-log line. ✅ requires both topologies **and** the named
   split-proof assertions.

**(d) Dispatch.** `[inline]` for verify and tracker; the review is its own subagent.

---

## Known gaps recorded deliberately

1. **No ledger retention.** Append-only and unbounded. Reads stay fast (the
   `(player_id, seq DESC)` index bounds them), so the cost is disk and backup, not latency —
   and "keep money history forever" may well be the right permanent answer. Follow-up if not:
   a `scheduler.fired{wallet-prune}` subscription with `WALLET_LEDGER_RETENTION_DAYS`.
2. **No pagination on `recent_ledger`.** A bounded `LIMIT` with no cursor — the shape
   `module-reference.md:115-122` says not to copy. Accepted because the only reader is the
   admin drill-down; a player-facing history op must add a cursor.
3. **Starter grant is single-currency and new-players-only.** Multi-currency is a later
   extension of the same knobs; back-filling existing players is the admin grant page (D9).
4. **Player-QUIC allow-list untouched.** `wallet.myBalances` is HTTP-only for now.
5. ~~**`reason` is not part of the dup-same comparison.**~~ **RETRACTED by revision 4**
   (item 2) and by the landed Step 2: `reason` IS part of the identity tuple, so a same-key
   resubmit with an edited `reason` is a 409, not a silent no-op. This entry survived the
   rev-3 → rev-4 edit describing the opposite of what ships; corrected when Step 2 landed.
6. **A successful admin grant redirects to the bare catalog view**, dropping `?player=`, so the
   operator never sees the balance or ledger row they just created — on the page that exists to
   show them. The authority is `render_after_submit` in `modules/admin` (`see_other("/admin/{slug}")`
   with no query), not wallet, so it is out of this rollout's scope. Recorded so it is not
   rediscovered as a wallet bug.
7. **A money movement made from the portal has no operator attribution.** `admin.action{form-submit}`
   records field NAMES only (by design), and `AdminSubmit::admin_submit` carries no operator
   identity across the edge by contract — so there is no join key between "who submitted a wallet
   form" and "what moved". "Which operator granted 1,000,000 gold to X" is unanswerable from the
   durable trail unless the operator types their name into `reason`. Contract-level; a decision,
   not an oversight.
8. **Postgres session headroom is 85/87** (D7). The 13th DB-backed split process breaks the
   budget and will need `SPLIT_SERVICE_POOL_MAX` re-tuned.

---

## Errata from execution

### Post-execution: the admin-submit cap gap is now a tracked, BLOCKING gap

Step 9's errata recorded `modules/apikeys`' uncapped admin-form strings as an
unrepresentable gap, on the grounds that the input-field traversal could not reach the
`adminSubmit` `Params` map. That traversal is now fail-closed and does reach it (the whole
`admin` domain was skipped by name; the skip is gone), so the gap has a real key:
`admin.adminSubmit  params.<value>  wire`, recorded as `InputPolicy::KnownGap` in
`tools/conformance/src/policy.rs`.

Consequence, deliberately not silenced: `--deny-gaps` rejects it, so the BLOCKING
`conformance` verify stage is RED until each owning module byte-checks its declared form
values before SQL. `modules/apikeys`' admin form is the one open case — role and key NAMES
reach SQL with no cap (only `store::MAX_POLICY_BYTES` guards the policy field); wallet's
catalog fields are already capped by `admin::CATALOG_CAPS` plus the `currencies_*_len_check`
constraints.


### Step 8 (admin page) — six declared deviations

Landed `bca2f30`. All six were reported rather than absorbed; the first three change what a
later step should expect.

1. **`Store::list_catalog` is a second backend addition.** The checklist sources the catalog's
   `created_at`, but `walletapi::Currency` has no such field. An admin-only projection keeps
   the published contract unchanged instead of widening it for one column.
2. **Balances render as KPI tiles, not a table** — `adminapi::Content` carries exactly ONE
   `table`, and the ledger is the row-shaped half. Both columns survive.
3. **Insufficient funds is 200 + the verdict card, not 409** — see `[WL6]` above.
4. `?player` accepts both `player:<uuid>` (what `{id}` actually interpolates to) and a bare
   uuid; the link template stays exactly `wallet?player={id}` for admincheck.
5. `decimals` is displayed, never applied — amounts render as raw minor units, per the
   contract's "display hint only".
6. Verdict classes flatten to `Error::invalid` on the wire, deliberately: `Error::conflict`
   would make admin-svc render the reload message while the monolith renders the real one —
   a topology divergence in operator-visible text.

The hidden idempotency fields are minted per render from `OsRng`, not a clock: two grants
rendered in the same tick must not collide into a silent "duplicate that already paid".

#### Step 8 follow-up — three review findings closed (`[opus]`, after `bca2f30`)

Three findings deferred from the adversarial review of `bca2f30`, closed at their authorities.

1. **The drill-down no longer truncates silently.** `Store::recent_ledger` selects
   `limit + 1` (still clamped to `MAX_RECENT_LEDGER`) and returns `LedgerPage { rows,
   truncated }`; the header note reads `newest N movement(s) — older rows not shown` or
   `N movement(s) — full history`. Known gap 2 still stands — there is still no cursor — but
   a partial page now says it is partial instead of reading as a complete audit trail.
2. **The table shows the value it is sorted by.** `LedgerEntry` carries `seq` and it renders
   as the leading mono column. `WHEN` stays, and the pair is now legible: `at` is
   `clock_timestamp()` at the ledger INSERT, `seq` is stamped under the balance row lock, so
   under concurrent movements the timestamps can disagree with the row order.
3. **SEMANTIC CHANGE — `WALLET_DEV_SEED` no longer reverts operator catalog edits.** The
   migrate path writes with `OnConflict::Skip` (`ON CONFLICT (code) DO NOTHING`); the ADMIN
   form keeps `OnConflict::Overwrite` (`DO UPDATE`). Before this change, renaming `Gold` on
   the admin page was silently undone by the next boot with the flag on. **A dev-seeded
   currency's `display_name`/`kind`/`decimals` are therefore no longer restored on boot** —
   the seed's job is that the dev codes EXIST so money can move, not that wallet owns their
   presentation. Recovery for a genuinely mangled dev row is the admin form (or dropping the
   row), not a restart. The doc comment claiming "Self-healing: a hand-edited dev row is
   restored on the next boot" was false after the split and is gone.

   **Fixture consequence.** That removed property is what made the local Postgres converge:
   the wallet catalog now drifts PERMANENTLY once anyone uses the admin form. Wallet tests and
   the `[WL*]` splitproof assertions must mint their own currency (the `unique_currency`
   pattern, `modules/wallet/src/tests.rs:83`) and must never lean on a dev-seeded row's
   `display_name`/`kind`/`decimals` — such an assertion passes on a fresh DB and fails on a
   developer box where the form was used once. Only the EXISTENCE of `gold`/`gems` is still
   guaranteed by the seed (`[WL1]` may keep asserting that).

Reviewer punch list over `d950791`, closed in the follow-up commit:

- **`seq` is monotonic but GAPPED** — the ledger INSERT's `bigserial` default is burned before
  `set_balance_after_tx` draws the ordering value, and a rolled-back movement burns both. Four
  movements can render `SEQ 4, 5, 9, 12` beside "4 movement(s) — full history", which reads as
  five deleted rows in an append-only ledger. The raw value stays (surfacing it is the point of
  F2); the comment at the table site now states that the guarantee is ordering, NOT contiguity.
- **The lock claim was too broad.** `seq` is drawn under the balance row lock of the movement's
  own `(player, currency)`, so it is the exact money order for one pair; the drill-down is
  cross-currency, where it is statement-execution order. Corrected in `admin.rs` and in
  `recent_ledger`'s doc.
- **One catalog writer, not two.** `insert_currency_if_absent_tx`/`upsert_currency_tx`
  duplicated the column list and bind order; they collapse into
  `Store::write_currency_tx(…, OnConflict::{Overwrite, Skip})`, so a new catalog column cannot
  reach one intent and miss the other.

No `api/wallet/*` change; 26 wallet tests unchanged and green. The follow-up ships with NO
execution path of its own — the five named cases in Step 10 (iii) are what pin it.

### Step 4 (`cmd/wallet-svc` + fleets) — four files the plan never listed, and one false command

Landed `ef5a940`; `archcheck` went 2 violations → **0**, `requirecheck --strict` OK.

1. **`cargo test -p weles fleet_toml` matches ZERO tests and reports green.** Both weles test
   files live in the `weles-master` crate, so `-p weles` filters everything out and prints a
   passing `0 passed` — a green SKIP wearing a PASS, the exact class this repo's taxonomy
   records from the cargo-audit stage. The real commands are
   **`cargo test -p weles-master fleet_toml`** and **`cargo test -p weles-master full_fleet_env_goldens`**.
2. **`cmd/gateway-svc/src/addrs_tests.rs` is load-bearing and was unlisted.** A 9th `AddrSpec`
   makes `FakeAgent::healthy()` panic on the unstubbed `("wallet", Edge)` question and breaks
   the 8-element `asked` assertion. Same class as the `conformance/src/tests.rs:294` count that
   rev 4 caught: a hardcoded fixture count that only the executing test can see.
3. **`cmd/admin-svc/src/main.rs` needs `.with_peer("wallet", env_addr("WALLET_EDGE_ADDR", …))`.**
   The plan listed only `lib.rs`; without the main edit the env var is inert and the process
   silently uses the compiled default — correct in this fleet by coincidence, wrong the moment
   a port moves.
4. **`weles/fleet.monolith.toml` needs `WALLET_DEV_SEED = "1"`** (+ its golden row), since that
   fixture is the faithful successor of processctl's monolith Development flavor.

### Step 9 gets one more row, with its exact shape (handed over from Step 4)

`tools/processctl/src/fleet_tests.rs::proof_fleet_is_the_canonical_twelve_service_snapshot`
now fails with exactly one diff. Step 9 must insert
`("wallet-svc", "wallet-svc", 8092, Some(9010), None, vec!["config-svc"])` after `inventory-svc`,
append `"wallet-svc"` to the gateway and admin dependency vectors, and rename the test
twelve → thirteen. `fleet_session_budget_is_enforced` passes, confirming D7's 85/87.


### Step 2 (`modules/wallet`) — two deviations, both deliberate

1. **`recent_ledger` deferred to Step 8.** Step 2's store-method list names it, but its
   only consumer is the admin drill-down and its row shape (which columns, what ordering
   window) is decided by that view. Shipping it now would be an unused method plus an
   unused row struct behind `#[allow(dead_code)]`, guessed against a view that does not
   exist. **Step 8 must ship it with a hard limit**: unlike `list_balances`/`list_currencies`,
   which are bounded by the operator-curated catalog, a player's ledger is caller-influenced
   and unbounded (known gap 1), so deferring the method also defers its bound.
   `currency_exists_tx` IS shipped in Step 2 as listed (with `#[allow(dead_code)]`
   until Step 6 calls it): unlike `recent_ledger` it has a fixed, zero-ambiguity shape and
   it is the statement that makes the delivery path abort-free, so it belongs beside the
   SQL it protects.
2. **The duplicate check compares player ids as UUIDs, not as bytes.** D3 step 2 says
   "same `(player_id, currency, delta, reason)`". Every statement binds `$n::uuid`, so a
   stored row and a resubmit that spell one id differently (uppercase / braced /
   unhyphenated) are the SAME player to Postgres while differing byte-wise. A byte
   comparison would answer 409 to a caller's own genuine replay — and a caller that reads
   409 as "this key is taken" mints a FRESH key, which double-moves money. So
   `service.rs`'s `player_id_eq` normalizes both sides the way `characters`/`inventory`
   already normalize their lock keys (32 ascii-hex digits, case/hyphen/brace insensitive;
   anything else falls back to a byte comparison). `currency`, `delta` and `reason` stay
   exact. Step 5 test #3 should therefore also cover a differently-spelled-but-equal
   player id resolving to `Duplicate`, not `Conflict`.

Verified against the real local Postgres while implementing (DDL executed, each arm
provoked): unknown currency = `23503` / `balances_currency_fkey`; a debit below zero AND
a credit past the 10^15 ceiling both = `23514` / `balances_amount_check` (the ceiling is
NOT 22003 — D2's premise confirmed); malformed player id = `22P02`; the second
`ON CONFLICT (idempotency_key) DO NOTHING … RETURNING` returns zero rows.

The `seq` correction was likewise reproduced before being applied: two ledger rows
INSERTed first and their balances applied in the OPPOSITE order (the interleaving a lost
race on the balance row lock produces) read back, under the insert-time `bigserial`, as
`(seq=2, balance_after=200), (seq=3, balance_after=100)` — a credit-only sequence whose
`ORDER BY seq` running balance DECREASES. Re-stamping `seq` in the under-lock statement
turns the same two rows into `(4, 100), (5, 200)`. `pg_get_serial_sequence('wallet.ledger',
'seq')` is `wallet.ledger_seq_seq`, so the hardcoded `nextval` name in
`set_balance_after_tx` is correct — it is a string the compiler cannot check, so Step 5's
4c is what keeps it honest.

The follow-up review's two structural findings were likewise reproduced before being
applied. **The `bigserial` sequence name is not guaranteed:** with a `ledger_seq_seq`
already present in the schema, `CREATE TABLE … seq bigserial` resolves
`pg_get_serial_sequence` to **`ledger_seq_seq1`**, and the squatted `ledger_seq_seq` stays
at its initial value forever — so a hardcoded `nextval('wallet.ledger_seq_seq')` would have
drawn the ledger's money ordering from an unrelated counter, with no error. The shipped
statement asks the catalog instead. **The catalog cap is `octet_length`, not
`char_length`:** `MAX_CURRENCY_CODE_BYTES` is a `str::len()` byte count, and a 20-character
multibyte code is 40 octets — verified that both `repeat('a',33)` and `repeat('ż',20)` are
rejected by `currencies_code_len_check`, so the by-construction argument holds for
non-ASCII codes too. The re-stamp's real cost is also recorded at the site: because `seq`
is covered by `ledger_player_seq_idx`, the UPDATE is now guaranteed NON-HOT (an extra index
tuple per movement plus a dead one for vacuum), not merely "one wasted sequence value".

One implementation detail beyond the punch list: with `validate_movement` inside
`apply_on`, `delta` is `sign * m.amount` with no `checked_mul` (the amount is bounded to
`1 ..= MAX_MOVEMENT_AMOUNT` one line above, so ±1 cannot overflow). `sign` stays `i64` per
D8, with a `debug_assert!(sign == 1 || sign == -1)` recording that it is a DIRECTION and
that both call sites are in-crate.

### Step 9 (tool inventories) — one impossible instruction, one unplanned generator change

1. **The `KnownGap` this step ordered cannot exist.** Step 9's table told the implementer to
   record the admin-submit gap "as a `KnownGap` beside the `Applies` stance". That is
   structurally impossible: `Entry::stance()` is a `.find()` over `Convention::ALL`, so a
   second `(InputByteCaps, KnownGap)` row for the same convention is dead code the tool never
   reads — and `--deny-gaps` plus two unit tests assert the gap sets are EMPTY, so a gap
   declared honestly would fail the blocking conformance stage. It landed as a comment on the
   `Applies` stance instead, which is the only place the fact could be recorded.
2. **The underlying gap is now CLOSED for wallet, so the question is moot here.** The review
   rollout capped `display_name` and `kind` at both levels — `admin::CATALOG_CAPS` in Rust
   (the message names the offending field) and the named
   `currencies_display_name_len_check` / `currencies_kind_len_check` column CHECKs as the
   class fail-safe — and bounded `decimals` to `0..=MAX_CURRENCY_DECIMALS` (18) in Rust with
   `currencies_decimals_range_check` behind it. The caps are module-private
   (`modules/wallet/src/service.rs`), NOT `walletapi`: no `#[rpc]` method carries these
   fields — `Movement` is the whole caller-facing input shape — so publishing them would
   advertise a caller obligation that does not exist, and no `--bless-public-api` was needed.
   `tools/conformance/src/policy.rs`'s comment was rewritten to say what is true now:
   the fields are capped but not REPRESENTABLE as an `InputKey`, because the traversal only
   walks `api/*/api` request DTOs.
   **The sibling is NOT closed:** `modules/apikeys`'s admin form strings are the identical
   uncovered surface (`Params` → SQL, no Rust cap, no column CHECK) with no record anywhere
   — an explicit known gap as of this rollout.
3. **Schema change — a dev box needs a wipe.** `CREATE TABLE IF NOT EXISTS` never adds a
   CHECK to a pre-existing table, so a box whose `wallet` schema predates this commit keeps a
   catalog with NO column-level fail-safe while the Rust caps still reject over-long input.
   The remedy is CLAUDE.md's wipe rule: `DROP SCHEMA wallet CASCADE` (or a full DB wipe) plus
   a fresh boot — never an `ALTER`.
4. **Unplanned generator change, recorded rather than smuggled.** Step 9's `csharp-client-gen`
   row named only `PROVIDERS` and the `phase_a()` arm, but wallet's `Currency::decimals` is an
   `i32`, which the type lattice did not model — so `TypeRef::I32` (+ its `cs_type` → `int`
   mapping and `map_type` arm) landed inside an inventory step. The review rollout added the
   diagnostic that omission cost: an unmodelled Rust scalar (`bool`, `u32`, `f64`, …) now
   bails in `map_type` naming the real remedy (add a `TypeRef` variant + its `cs_type`
   mapping) instead of falling through to `TypeRef::Struct` and dying much later in
   `collect_dtos` as `DTO "u32" referenced but no pub struct found`. Pinned by
   `unmodelled_scalar_bails_naming_the_lattice_not_a_missing_dto`.
5. **`CLAUDE.md` prose the step should have swept, corrected here.** The audit bullet still
   claimed "6 ledger topics … a 7th subscription for prune" after Step 3 made `wallet.changed`
   the seventh sink (prune is the eighth), and `WALLET_DEV_SEED` appeared in no bullet at all
   while every other dev opt-in is documented. Both fixed; the wallet bullet deliberately does
   NOT copy apikeys' "self-healing upsert" wording, which `b912fa1` made false for wallet (the
   seed is insert-if-absent, so an operator's catalog edit survives a restart).

## Revision history

### Revision 3 → 4 (`core-reviewer` pass over the landed Step 1, `9ccd243`)

| # | Correction | Severity |
|---|---|---|
| 1 | **Step 9 was missing `tools/conformance/src/tests.rs:294`** (hardcoded `18` → `27`), so the plan as written left the BLOCKING `test` stage red. Added, together with the topiccheck FS-drift test, so the expected-red set is declared rather than discovered. | blocking |
| 2 | **`reason` moved INTO the idempotency comparison** (D3 step 2). The contract doc already promised "the same movement"; the narrow `(player, currency, delta)` tuple would have made an edited-`reason` resubmit a silent success recording the original reason. Step 5 #3 grows the case that pins it. | high |
| 3 | **`MAX_MOVEMENT_AMOUNT` + an upper bound on the balance CHECK** (D2). `amount` was the one uncapped field in a crate that caps its three strings; `sign * amount` panics in debug for `i64::MIN` (**not** for `i64::MAX` — an earlier draft of
this row said otherwise and was wrong; retracted in `178206a`), and a `bigint` overflow (22003) is unmapped — on the delivery path it would abort the tx, fail the checkpoint with 25P02 and poison the starter-grant subscription, i.e. exactly what D9 claims is impossible. Both caps sit far below `i64::MAX`, so the overflow is now unreachable and the ceiling surfaces as an already-mapped 23514/409. Tests: Step 5 #10, Step 7 #3b. | high |
| 4 | `walletapi` **drops the `adminapi` dependency** — wallet consumes accounts' extension point from `modules/wallet/src/admin.rs`, so the contract crate never names an `adminapi` symbol. The rev-3 rationale ("same edge `charactersapi` carries") did not apply: `charactersapi` carries it because it *declares* a point. | low |
| 5 | Trait doc corrected: `credit`/`debit` are reachable from a peer process over the internal edge only. The admin portal reaches wallet through `admin.adminSubmit` → the local service, never through `wallet.credit`. | low |

### Revision 2 → 3 (starter grant, at the user's call)

The gap "no starter grant" was accepted as a real hole: a registered player owning literally
no balance row makes the economy inert the moment a store exists. Added as **optional by
data** — two `config` knobs whose compiled defaults leave it OFF — rather than an env flag or
hardcoded values. Folded into the design at source:

- **D8** — the movement logic becomes one `apply_on(conn, …)` authority with an `Outcome` enum,
  written that way in Step 2, because it now has two callers with different transaction
  ownership (pool tx vs handed delivery tx). Growing a second credit path in Step 6 would have
  been duplicated authority.
- **D9** — the subscription (`wallet.player-registered.v1`, `StartPosition::AfterRegistration`),
  the config knobs, posture A, and the pre-`SELECT` currency check that keeps an FK error from
  aborting the delivery tx and poisoning the subscription.
- **`StartPosition::AfterRegistration`, not `Genesis`** — `player.registered` retains 7 days,
  so Genesis would promise a retroactive grant it cannot deliver; back-fill is the admin page.
- Ripples: `requires(["config"])`, the `config` stub in `cmd/wallet-svc`, the `config-svc`
  fleet dependency, `accountsevents` dep, new Steps 6-7, `[WL7]`, later steps renumbered.

### Revision 1 → 2 (`core-reviewer` punch list)

| # | Correction | Severity |
|---|---|---|
| 1 | **weles fleet was missed entirely** — `weles/fleet.split.toml`, the `== 12` assertion at `fleet_toml_tests.rs:868`, the manifest env golden. A BLOCKING verify stage boots that fixture. | blocking |
| 2 | Admin idempotency key moved from **submit time to render time** — a per-submit UUID makes every retry a distinct key, i.e. it double-grants. | high |
| 3 | **D4's justification was wrong.** `match::report` returns `()`, wallet returns `i64`; `#[retry_safe]` is legal only because the duplicate arm returns the stored `balance_after`. | high |
| 4 | Extension link `?player={player_id}` → **`?player={id}`** — not in the point's `context_keys`. | high |
| 5 | Step 3 is **three edits** — audit's `durable_topics_match_events` test and a `walletevents` dev-dep. | high |
| 6 | `WALLET_DEV_SEED` **wired in three fleet sites**; revision 1 asserted it but wired it nowhere. | high |
| 7 | `tools/conformance/input-fields.golden.tsv` added — byte-compared, blocking, **no bless writer**. | high |
| 8 | **`$n::uuid` casts + a 22P02 arm** (three interpreted SQLSTATEs, not two), plus test #7. | high |
| 9 | Step 3's ordering rationale corrected: the authority is `defined_topics()`. | medium |
| 10 | `remote_factories()`/`provide_factories()` **dropped** — zero callers. | medium |
| 11 | Ledger ordering moved from `at` to **`bigserial seq`**. | medium |
| 12 | **Aborted-transaction rule** stated explicitly. | medium |
| 13 | **READ COMMITTED dependency** stated, plus the "row disappeared" arm. | medium |
| 14 | PG session budget recorded; the stale "11 DB-backed processes" comment fixed. | medium |
| 15 | `Service::new` takes the **bus**. | low |
| 16 | `tools/archcheck/src/tests.rs` demoted to optional. | low |
| 17 | opscatalog needs **two** entries. | low |
| 18 | `CLAUDE.md` (11→12 fortresses, port roster) added. | low |
| 19 | Currency `revision` column **dropped**; `reason`-not-compared added as a gap. | low |

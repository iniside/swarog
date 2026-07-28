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

`amount bigint NOT NULL CHECK (amount >= 0)` named `balances_amount_check`. Insufficient
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
**delivery** path: a 22003 there would abort the delivery tx, fail the plane's checkpoint
UPDATE with 25P02 and poison `wallet.player-registered.v1` — the exact "removed by
construction" claim D9 makes, and the exact overflow class that already bit inventory once
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
itself:

1. `INSERT INTO wallet.ledger (idempotency_key, player_id, currency, delta, reason, balance_after)
   VALUES ($1,$2::uuid,$3,$4,$5,0) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id::text`
2. `None` ⇒ the key is already used. Re-`SELECT player_id::text, currency, delta, reason,
   balance_after FROM wallet.ledger WHERE idempotency_key = $1` **on the same connection**, then:
   - same `(player_id, currency, delta, reason)` → `Outcome::Duplicate(existing.balance_after)`;
   - different → `Outcome::Conflict`;
   - **no row** → `Error::internal("conflicting ledger row disappeared")` — the arm `match`
     also has (`modules/match/src/lib.rs:210`). Unreachable under READ COMMITTED; must not
     be an `unwrap`.
3. `Some(id)` ⇒ `INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid,$2,$3)
   ON CONFLICT (player_id, currency) DO UPDATE SET amount = wallet.balances.amount + $3,
   updated_at = now() RETURNING amount` — a debit passes a negative `$3`.
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

**Posture A — the handler must never poison its subscription.** A bad config value or a
missing currency is a property of the *config*, not of the event; returning `Err` would
back off and, after 20 failures, pause `wallet.player-registered.v1` for **every**
subsequent player (`core/asyncevents/src/worker.rs:88`). So, mirroring
`modules/inventory/src/projection.rs:115-129`:

- amount ≤ 0 / empty currency → skip silently (feature off);
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
`serde_json`, plus `adminapi` (types-only, Step 8's extension entry). **Never**
`tokio`/`sqlx`/`edge`/`remote` — `FORBIDDEN_API_DEPS` (`tools/archcheck/src/main.rs:91-94`).

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
	code         text PRIMARY KEY,
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

**Ordering is `seq`, not `at`.** `now()` is `transaction_timestamp()` — evaluated at tx
start, while the balance row lock is taken later (D3 step 3). Two concurrent movements on
one `(player, currency)` can commit with `at` in one order and `balance_after` in the other,
so an `at`-ordered read of an append-only ledger would show a non-monotonic running balance.
`bigserial` is assigned at insert; `at` is descriptive.

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
routes through (three byte caps, `amount > 0`, non-empty key), and the **`apply_on` authority
plus `Outcome` enum exactly as specified in D8** — written this way now, not refactored into
it in Step 6. `credit`/`debit` are the pool-path wrappers.

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
    bounds** (`0`, negative, `i64::MAX`, `MAX_MOVEMENT_AMOUNT + 1`). The `i64::MAX` case is the
    one that would panic in debug on `sign * amount` without the cap.

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

**Fidelity:** the UI has an exact spec in `UILayout/GameOps Admin.dc.html`; translate 1:1
including data shape. Balances and ledger rows are **always real** — never invent money.

**(d) Dispatch.** `[opus]` — `mockup-implementer`, `model:"opus"`, effort **think hard**.

---

## Step 9 — Tool inventories, policies, baselines and docs

**(a) What.** The hand-maintained lists that gate the build:

| File | Edit |
|---|---|
| `tools/conformance/src/policy.rs` | `wallet()` entry + `entries()` row. `InputByteCaps` → `Stance::Applies` with the three real probes. `EnvValidation` → NA ("WALLET_DEV_SEED is a boolean presence-gate, not a parsed value"). `ArgonParity` → NA. Plus one `input_policies()` row per wire input **leaf** — the traversal recurses request DTOs (`input_inventory.rs:116-158`). Add `wallet` to the tool's Cargo.toml. |
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
- `[WL6] revoke beyond balance -> 409, balance unchanged`.
- `[WL7] a newly registered player receives the configured starter grant` — **the config rows
  are written in the harness's pre-spawn setup**, before the fleet starts:
  `INSERT INTO config.settings (namespace,key,value) VALUES ('wallet','starter_currency','gold'),
  ('wallet','starter_amount','100') ON CONFLICT … DO UPDATE`. Pre-spawn, because `CachedConfig`
  is boot-fill-or-fail-startup — writing after boot would race the invalidation refresh against
  the registration, and this repo does not race clocks. Then register a fresh player through
  gateway-svc and `poll_count` the balance. This proves, in one assertion: cross-process durable
  delivery (accounts-svc emits → wallet-svc consumes), the config read, and the credit.
- Monolith parity: re-run `[WL1]`/`[WL2]`/`[WL7]` on `cmd/server`.

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
5. **`reason` is not part of the dup-same comparison.** A replay with the same key and a
   different `reason` is a no-op recording the original reason. Deliberate — the key
   identifies the movement — but a real asymmetry.
6. **Postgres session headroom is 85/87** (D7). The 13th DB-backed split process breaks the
   budget and will need `SPLIT_SERVICE_POOL_MAX` re-tuned.

---

## Revision history

### Revision 3 → 4 (`core-reviewer` pass over the landed Step 1, `9ccd243`)

| # | Correction | Severity |
|---|---|---|
| 1 | **Step 9 was missing `tools/conformance/src/tests.rs:294`** (hardcoded `18` → `27`), so the plan as written left the BLOCKING `test` stage red. Added, together with the topiccheck FS-drift test, so the expected-red set is declared rather than discovered. | blocking |
| 2 | **`reason` moved INTO the idempotency comparison** (D3 step 2). The contract doc already promised "the same movement"; the narrow `(player, currency, delta)` tuple would have made an edited-`reason` resubmit a silent success recording the original reason. Step 5 #3 grows the case that pins it. | high |
| 3 | **`MAX_MOVEMENT_AMOUNT` + an upper bound on the balance CHECK** (D2). `amount` was the one uncapped field in a crate that caps its three strings; `sign * i64::MAX` panics in debug, and a `bigint` overflow (22003) is unmapped — on the delivery path it would abort the tx, fail the checkpoint with 25P02 and poison the starter-grant subscription, i.e. exactly what D9 claims is impossible. Both caps sit far below `i64::MAX`, so the overflow is now unreachable and the ceiling surfaces as an already-mapped 23514/409. Tests: Step 5 #10, Step 7 #3b. | high |
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

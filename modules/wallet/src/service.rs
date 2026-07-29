use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::{AnyTx, Bus};
use configapi::Config;
use opsapi::{Error, Identity};
use sqlx::{PgConnection, PgPool};
use walletapi::{
    Balance, Currency, Movement, Player, Wallet, MAX_CURRENCY_CODE_BYTES,
    MAX_IDEMPOTENCY_KEY_BYTES, MAX_MOVEMENT_AMOUNT, MAX_REASON_BYTES,
};

use crate::{internal, BalanceError, ExistingMovement, Store};

/// The 409 for a duplicate `idempotency_key` describing a DIFFERENT movement.
pub(crate) const IDEMPOTENCY_CONFLICT: &str =
    "idempotency_key already records a different movement";

/// The 409 for the balance CHECK, worded to cover BOTH ends because one constraint does:
/// a debit below zero and a credit past the ceiling are the same verdict.
pub(crate) const OUT_OF_RANGE: &str =
    "movement rejected: insufficient funds or balance ceiling exceeded";

pub(crate) const UNKNOWN_CURRENCY: &str = "unknown currency";

pub(crate) const MALFORMED_PLAYER_ID: &str = "player_id is not a valid uuid";

/// The INSERT lost the key but the winning row is not visible — unreachable under READ
/// COMMITTED, and an explicit arm rather than an `unwrap` so a changed isolation level
/// fails loudly.
pub(crate) const LEDGER_ROW_DISAPPEARED: &str = "conflicting ledger row disappeared";

pub(crate) fn idempotency_key_within_cap(key: &str) -> bool {
    key.len() <= MAX_IDEMPOTENCY_KEY_BYTES
}

pub(crate) fn currency_code_within_cap(code: &str) -> bool {
    code.len() <= MAX_CURRENCY_CODE_BYTES
}

pub(crate) fn reason_within_cap(reason: &str) -> bool {
    reason.len() <= MAX_REASON_BYTES
}

/// THE movement-input policy, enforced INSIDE the authority ([`Service::apply_on`]) so no
/// caller can route around it. The amount's lower bound is the contract's positivity
/// promise (a `-500` credit would DEBIT while publishing `reason = "grant"`, a `0` would
/// burn an idempotency key); the upper bound keeps a `bigint` overflow (22003, which
/// nothing maps) out of the balance update, leaving the mapped 23514/409.
pub(crate) fn validate_movement(m: &Movement) -> Result<(), Error> {
    if m.idempotency_key.is_empty() {
        return Err(Error::invalid("idempotency_key is required"));
    }
    if !idempotency_key_within_cap(&m.idempotency_key) {
        return Err(Error::invalid(format!(
            "idempotency_key exceeds {MAX_IDEMPOTENCY_KEY_BYTES} bytes"
        )));
    }
    if m.player_id.trim().is_empty() {
        return Err(Error::invalid("player_id is required"));
    }
    if m.currency.is_empty() {
        return Err(Error::invalid("currency is required"));
    }
    if !currency_code_within_cap(&m.currency) {
        return Err(Error::invalid(format!(
            "currency exceeds {MAX_CURRENCY_CODE_BYTES} bytes"
        )));
    }
    if !reason_within_cap(&m.reason) {
        return Err(Error::invalid(format!(
            "reason exceeds {MAX_REASON_BYTES} bytes"
        )));
    }
    if m.amount < 1 || m.amount > MAX_MOVEMENT_AMOUNT {
        return Err(Error::invalid(format!(
            "amount must be within 1..={MAX_MOVEMENT_AMOUNT}"
        )));
    }
    Ok(())
}

/// Narrow on purpose — constraint-named where a constraint exists — so a future CHECK or
/// FK never silently inherits a 409/400; anything else is a genuine 500.
fn movement_error(e: sqlx::Error) -> Error {
    if crate::is_out_of_range(&e) {
        Error::conflict(OUT_OF_RANGE)
    } else if crate::is_unknown_currency(&e) {
        Error::invalid(UNKNOWN_CURRENCY)
    } else if crate::is_invalid_uuid(&e) {
        Error::invalid(MALFORMED_PLAYER_ID)
    } else {
        internal(e)
    }
}

/// The same verdicts as [`movement_error`], for the two the balance authority decides
/// itself: the messages are shared, so a missing-row debit is indistinguishable from the
/// SQLSTATE-carried answer to the caller.
fn balance_error(e: BalanceError) -> Error {
    match e {
        BalanceError::UnknownCurrency => Error::invalid(UNKNOWN_CURRENCY),
        BalanceError::OutOfRange => Error::conflict(OUT_OF_RANGE),
        BalanceError::Sql(e) => movement_error(e),
    }
}

/// True iff two spellings are the SAME uuid to Postgres (every statement binds `$n::uuid`,
/// so uppercase / braced / unhyphenated forms are one player). A replay reported as a
/// conflict invites the caller to mint a FRESH key, which double-moves money — hence the
/// normalization: equal-to-`::uuid` inputs share 32 ascii-hex digits ignoring
/// case/hyphens/braces, anything else falls back to a byte comparison.
fn hex32(s: &str) -> Option<Vec<u8>> {
    let hex: Vec<u8> = s
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    (hex.len() == 32).then_some(hex)
}

/// True iff `$n::uuid` parses `id`: [`hex32`]'s 32 digits and NOTHING else but hyphens
/// where `uuid_in` tolerates one — on a two-byte boundary, never doubled, never trailing.
/// Deliberately narrower than `uuid_in` (a braced spelling is rejected), because a caller
/// that skips on `false` needs the accept set to be a SUBSET of what parses; `hex32` alone
/// is not, since it normalizes for EQUALITY and so ignores stray characters.
pub(crate) fn is_uuid_text(id: &str) -> bool {
    if hex32(id).is_none() {
        return false;
    }
    let mut digits = 0usize;
    let mut after_hyphen = false;
    for b in id.bytes() {
        if b.is_ascii_hexdigit() {
            digits += 1;
            after_hyphen = false;
        } else if b == b'-' && !after_hyphen && digits.is_multiple_of(4) && (1..32).contains(&digits) {
            after_hyphen = true;
        } else {
            return false;
        }
    }
    true
}

fn player_id_eq(a: &str, b: &str) -> bool {
    match (hex32(a), hex32(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// The replayed-movement identity is the WHOLE movement minus the key. `reason` is part of
/// it deliberately: on the narrower `(player, currency, delta)` triple a
/// `credit(K, 100, "promo")` then `credit(K, 100, "refund")` is a silent success recording
/// the FIRST reason — a balance for a movement the caller did not describe.
fn same_movement(existing: &ExistingMovement, m: &Movement, delta: i64) -> bool {
    existing.delta == delta
        && existing.currency == m.currency
        && existing.reason == m.reason
        && player_id_eq(&existing.player_id, &m.player_id)
}

/// The balance the caller observes; `Duplicate` carries the ORIGINAL movement's stored
/// `balance_after`, never a fresh read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Applied(i64),
    Duplicate(i64),
    Conflict,
}

pub struct Service {
    pub(crate) store: Store,
    pub(crate) bus: Arc<Bus>,
    /// The reader is itself a replica-local cache kept fresh by the invalidation plane, so
    /// wallet reads it directly and owns no second cache.
    pub(crate) config: OnceLock<Arc<dyn Config>>,
    /// `WALLET_DEV_SEED`, read ONCE in `register`.
    pub(crate) dev_seed: bool,
}

impl Service {
    pub(crate) fn new(pool: PgPool, bus: Arc<Bus>, dev_seed: bool) -> Service {
        Service {
            store: Store { pool },
            bus,
            config: OnceLock::new(),
            dev_seed,
        }
    }

    /// **THE movement authority**, on a CALLER-OWNED connection: it NEVER begins, commits
    /// or rolls back. That is what lets the pool paths ([`Service::credit`] /
    /// [`Service::debit`]) and a durable handler — whose credit, ledger row,
    /// `wallet.changed` append and subscription checkpoint must commit as one unit — share
    /// one implementation instead of two ways to move money.
    ///
    /// It validates the movement itself, so no caller can route around the sign/range
    /// guard. A DURABLE caller must therefore be unable to build a movement this rejects,
    /// BY CONSTRUCTION rather than by re-enumerating [`validate_movement`]'s branches: the
    /// catalog cannot hold an oversized (`currencies_code_len_check`) or absent
    /// (`currency_exists_tx`) currency code, and the handler clamps the configured amount
    /// with fixed `reason`/`idempotency_key` shapes. Ordering is a CORRECTNESS constraint:
    /// the clamp and the catalog probe run BEFORE this call, never after.
    ///
    /// On an `Err` the caller's transaction may be ABORTED (23514 / 23503) — the only legal
    /// next statement is the unwind, never a balance read to enrich the message (25P02 on
    /// that connection would turn a 409 into a 500). A durable handler must never return
    /// `Err` (it backs off and pauses its subscription), and its `Ok` on an aborted
    /// transaction fails the plane's checkpoint `UPDATE` with 25P02 — hence the two
    /// by-construction pre-checks above.
    pub(crate) async fn apply_on(
        &self,
        conn: &mut PgConnection,
        m: &Movement,
        sign: i64,
    ) -> Result<Outcome, Error> {
        validate_movement(m)?;
        // `amount` is validated to `1 ..= MAX_MOVEMENT_AMOUNT` above, so with a ±1 sign the
        // multiplication cannot overflow.
        debug_assert!(sign == 1 || sign == -1, "apply_on's sign is ±1");
        let delta = sign * m.amount;

        // The ledger insert is the dedup gate, so it claims the key BEFORE money moves.
        let claimed = self
            .store
            .insert_ledger_tx(
                conn,
                &m.idempotency_key,
                &m.player_id,
                &m.currency,
                delta,
                &m.reason,
            )
            .await
            .map_err(movement_error)?;

        // The key was already used: re-read the winning row on THIS connection.
        let Some((ledger_id, canonical_player_id)) = claimed else {
            let existing = self
                .store
                .existing_ledger_tx(conn, &m.idempotency_key)
                .await
                .map_err(movement_error)?
                .ok_or_else(|| Error::internal(LEDGER_ROW_DISAPPEARED))?;
            if same_movement(&existing, m, delta) {
                // The STORED `balance_after`, never a fresh read: a movement landing in
                // between must not make the replay answer differently from the original
                // call — that equivalence is the licence for `#[retry_safe]`.
                return Ok(Outcome::Duplicate(existing.balance_after));
            }
            return Ok(Outcome::Conflict);
        };

        let balance = self
            .store
            .apply_balance_tx(conn, &m.player_id, &m.currency, delta)
            .await
            .map_err(balance_error)?;

        self.store
            .set_balance_after_tx(conn, &ledger_id, balance)
            .await
            .map_err(movement_error)?;

        // Emitted ONLY on this branch (a replay moves no money) and on the caller's
        // connection, so the event is durable iff the movement is. The player id is the
        // DB-canonical spelling from `RETURNING`, not the caller's argument.
        let evt = walletevents::Changed {
            player_id: canonical_player_id,
            currency: m.currency.clone(),
            delta,
            balance_after: balance,
            reason: m.reason.clone(),
            ledger_id,
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *conn), &walletevents::CHANGED, &evt)
            .await
            .map_err(internal)?;

        Ok(Outcome::Applied(balance))
    }

    /// The POOL-path wrapper: it owns the transaction and nothing else. The pre-call
    /// [`validate_movement`] only avoids opening one for a bad request — the authority is
    /// what guards.
    ///
    /// A failed unwind is logged, not returned: nothing was committed and no COMMIT was
    /// issued on those arms, so the answer already in hand (a balance, a 409, a domain
    /// error) is true whether or not the ROLLBACK reached the server. Only the COMMIT arm
    /// maps its failure, because there the outcome genuinely is unknown.
    async fn apply(&self, m: Movement, sign: i64) -> Result<i64, Error> {
        validate_movement(&m)?;
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        // Answer AND disposition from one exhaustive match, so no arm can drift into
        // committing what it meant to unwind.
        let (applied, result) = match self.apply_on(&mut tx, &m, sign).await {
            Ok(Outcome::Applied(balance)) => (true, Ok(balance)),
            Ok(Outcome::Duplicate(balance)) => (false, Ok(balance)),
            Ok(Outcome::Conflict) => (false, Err(Error::conflict(IDEMPOTENCY_CONFLICT))),
            Err(e) => (false, Err(e)),
        };
        if applied {
            tx.commit().await.map_err(internal)?;
        } else if let Err(e) = tx.rollback().await {
            // EXPLICIT rollback: a dropped sqlx tx defers the ROLLBACK and holds the locks
            // the INSERT/SELECT took. And after 23514/23503 every further statement on this
            // connection fails with 25P02, so the unwind is the only legal move.
            tracing::warn!(error = %e, "wallet: rollback after a non-applied movement failed");
        }
        result
    }
}

#[async_trait]
impl Wallet for Service {
    async fn balances(&self, player_id: String) -> Result<Vec<Balance>, Error> {
        self.store.list_balances(&player_id).await.map_err(internal)
    }

    async fn currencies(&self) -> Result<Vec<Currency>, Error> {
        self.store.list_currencies().await.map_err(internal)
    }

    async fn credit(&self, movement: Movement) -> Result<i64, Error> {
        self.apply(movement, 1).await
    }

    async fn debit(&self, movement: Movement) -> Result<i64, Error> {
        self.apply(movement, -1).await
    }
}

#[async_trait]
impl Player for Service {
    /// The player id comes from `identity` (gateway-verified), NEVER from a body field —
    /// so a client cannot read another player's wallet.
    async fn my_balances(&self, identity: Identity) -> Result<Vec<Balance>, Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        self.store.list_balances(player_id).await.map_err(internal)
    }

    async fn list_currencies(&self, _identity: Identity) -> Result<Vec<Currency>, Error> {
        self.store.list_currencies().await.map_err(internal)
    }
}

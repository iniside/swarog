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

use crate::{internal, ExistingMovement, Store};

/// The 409 for a duplicate `idempotency_key` describing a DIFFERENT movement. A const so
/// a test can pin the branch rather than the wording.
pub(crate) const IDEMPOTENCY_CONFLICT: &str =
    "idempotency_key already records a different movement";

/// The 409 for the balance CHECK. It covers BOTH ends of the range, because one
/// constraint does: a debit below zero and a credit past the ceiling are the same
/// "the balance would leave its legal range" verdict.
pub(crate) const OUT_OF_RANGE: &str =
    "movement rejected: insufficient funds or balance ceiling exceeded";

pub(crate) const UNKNOWN_CURRENCY: &str = "unknown currency";

pub(crate) const MALFORMED_PLAYER_ID: &str = "player_id is not a valid uuid";

/// The unreachable-under-READ-COMMITTED arm of the duplicate branch (the arm `match`
/// also carries): the INSERT lost the key, but the winning row is not visible. Never an
/// `unwrap` — if the isolation assumption ever changes, this must fail loudly.
pub(crate) const LEDGER_ROW_DISAPPEARED: &str = "conflicting ledger row disappeared";

/// A programming-error belt, not a validation path: `apply_on`'s callers validate the
/// amount BEFORE calling (the wire wrappers through [`validate_movement`], the durable
/// grant through its own posture-A clamp), so the multiplication can only overflow if a
/// future caller skips both. `checked_mul` rather than `sign * amount` because negating
/// `i64::MIN` panics in a debug build.
pub(crate) const AMOUNT_OUT_OF_RANGE: &str =
    "movement amount out of range — the caller must validate before apply_on";

pub(crate) fn idempotency_key_within_cap(key: &str) -> bool {
    key.len() <= MAX_IDEMPOTENCY_KEY_BYTES
}

pub(crate) fn currency_code_within_cap(code: &str) -> bool {
    code.len() <= MAX_CURRENCY_CODE_BYTES
}

pub(crate) fn reason_within_cap(reason: &str) -> bool {
    reason.len() <= MAX_REASON_BYTES
}

/// THE single movement-input policy. Every CALLER-FACING writer routes through it, so
/// what a caller may ask for is decided in exactly one place.
///
/// The amount carries BOTH bounds (`1 ..= MAX_MOVEMENT_AMOUNT`), not just "> 0": the
/// contract publishes that range, and the upper bound is what keeps a `bigint` overflow
/// (22003, which nothing maps) out of the balance update — a credit past the ceiling
/// surfaces as the already-mapped 23514/409 instead.
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

/// Maps the three interpreted SQLSTATEs of a movement statement. Anything else is a
/// genuine 500 — the mapping is narrow on purpose (constraint-named where a constraint
/// exists), so a future CHECK or FK never silently inherits a 409/400.
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

/// True iff two uuid spellings are the SAME uuid to Postgres. Every statement binds
/// `$n::uuid`, so a stored row and a resubmit that spell one id differently
/// (uppercase / braced / unhyphenated) are the same player — and the replay of a
/// genuinely identical movement must not be reported as a conflict, because a caller
/// that gets a 409 for its own retry is invited to mint a FRESH key, which double-moves
/// money. Two inputs Postgres's `::uuid` treats as equal share the same 32 ascii-hex
/// digits ignoring case/hyphens/braces; anything else falls back to a byte comparison.
/// (The same normalization discipline as the `characters`/`inventory` lock keys; the
/// fortress rule is why it is written out rather than shared.)
fn player_id_eq(a: &str, b: &str) -> bool {
    fn hex32(s: &str) -> Option<Vec<u8>> {
        let hex: Vec<u8> = s
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|b| b.to_ascii_lowercase())
            .collect();
        (hex.len() == 32).then_some(hex)
    }
    match (hex32(a), hex32(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// The replayed-movement identity: the WHOLE movement minus the key —
/// `(player_id, currency, signed delta, reason)`. `reason` is included deliberately: with
/// the narrow `(player, currency, delta)` triple a `credit(K, 100, "promo")` followed by
/// `credit(K, 100, "refund")` would be a silent success recording the FIRST reason, i.e.
/// the caller gets a balance for a movement it did not describe. Only an EDITED resubmit
/// is a conflict, and it deserves its own key.
fn same_movement(existing: &ExistingMovement, m: &Movement, delta: i64) -> bool {
    existing.delta == delta
        && existing.currency == m.currency
        && existing.reason == m.reason
        && player_id_eq(&existing.player_id, &m.player_id)
}

/// What [`Service::apply_on`] decided. The value in `Applied`/`Duplicate` is the
/// balance the caller observes; `Duplicate` carries the ORIGINAL movement's stored
/// `balance_after`, never a fresh read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Applied(i64),
    Duplicate(i64),
    Conflict,
}

// ============================================================================
// Service — backs Wallet + Player (the registry capabilities + the generated edge
// faces + the gateway's in-process invokers).
// ============================================================================

pub struct Service {
    pub(crate) store: Store,
    pub(crate) bus: Arc<Bus>,
    /// The `config` reader backing the config-driven starter grant; resolved in `init`
    /// (phase 2), never in `start` — `requirecheck` observes `register` + `init` only.
    /// It is a replica-local cache kept fresh by the app-owned invalidation plane, so
    /// wallet reads it directly and owns no second cache.
    pub(crate) config: OnceLock<Arc<dyn Config>>,
    /// `WALLET_DEV_SEED`, resolved ONCE in `register` so the env read is the single
    /// source of truth for every path that consults it.
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

    /// **THE movement authority.** Runs the whole movement — dedup gate, balance update,
    /// ledger stamp, durable append — on a CALLER-OWNED connection, and NEVER begins,
    /// commits or rolls back: transaction control belongs to the caller.
    ///
    /// That ownership split is the point. The caller-facing paths ([`Service::credit`] /
    /// [`Service::debit`]) own a pool transaction; the durable starter grant runs on the
    /// event plane's HANDED delivery connection, where the credit, the ledger row, the
    /// `wallet.changed` append AND the subscription checkpoint must commit as one unit —
    /// a function that opened its own transaction could not serve that caller at all. So
    /// there is ONE authority with two callers, not two code paths for one money
    /// movement.
    ///
    /// Callers MUST validate the movement first: the wire wrappers through
    /// [`validate_movement`] (out of range ⇒ 400), the durable grant through its own
    /// warn-and-skip clamp (out of range ⇒ no grant, never an `Err`, because an `Err`
    /// inside a delivery transaction backs off and eventually pauses the subscription).
    ///
    /// On an `Err` the caller's transaction may be ABORTED (23514 / 23503): the only
    /// legal next statement is the unwind. Do not read the balance to enrich the message
    /// — on that connection it would fail with 25P02 and turn a 409 into a 500.
    pub(crate) async fn apply_on(
        &self,
        conn: &mut PgConnection,
        m: &Movement,
        sign: i64,
    ) -> Result<Outcome, Error> {
        let delta = m
            .amount
            .checked_mul(sign)
            .ok_or_else(|| Error::internal(AMOUNT_OUT_OF_RANGE))?;

        // 1. Claim the key. The ledger insert is the dedup gate, so it runs BEFORE any
        //    money moves.
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

        // 2. The key was already used: re-read the winning row on THIS connection and
        //    decide replay-vs-conflict from the whole movement.
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
                // call. That equivalence is the whole licence for `#[retry_safe]`.
                return Ok(Outcome::Duplicate(existing.balance_after));
            }
            return Ok(Outcome::Conflict);
        };

        // 3. Move the money (a debit passes a negative delta).
        let balance = self
            .store
            .apply_balance_tx(conn, &m.player_id, &m.currency, delta)
            .await
            .map_err(movement_error)?;

        // 4. Stamp the ledger row with what the movement produced.
        self.store
            .set_balance_after_tx(conn, &ledger_id, balance)
            .await
            .map_err(movement_error)?;

        // 5. The durable append — ONLY on this branch. A replay moves no money, so it
        //    publishes nothing. Emitted on the caller's connection, so the event is
        //    durable iff the movement is. The player id is the DB-canonical spelling
        //    from `RETURNING`, not the caller's argument.
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

    /// The POOL-path wrapper around [`Service::apply_on`]: it owns the transaction and
    /// nothing else. `credit` and `debit` differ ONLY in the sign they pass.
    async fn apply(&self, m: Movement, sign: i64) -> Result<i64, Error> {
        validate_movement(&m)?;
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        match self.apply_on(&mut tx, &m, sign).await {
            Ok(Outcome::Applied(balance)) => {
                tx.commit().await.map_err(internal)?;
                Ok(balance)
            }
            // A replay wrote nothing; roll back EXPLICITLY rather than letting the drop
            // defer the ROLLBACK and hold the locks the INSERT/SELECT took.
            Ok(Outcome::Duplicate(balance)) => {
                tx.rollback().await.map_err(internal)?;
                Ok(balance)
            }
            Ok(Outcome::Conflict) => {
                tx.rollback().await.map_err(internal)?;
                Err(Error::conflict(IDEMPOTENCY_CONFLICT))
            }
            Err(e) => {
                // Aborted-transaction rule: after 23514/23503 EVERY further statement on
                // this connection fails with 25P02, so the unwind is the only legal move
                // — no balance read to enrich the message. The rollback's own failure is
                // logged, not returned: the domain verdict already in hand (409/400) is
                // the more useful answer.
                if let Err(re) = tx.rollback().await {
                    tracing::warn!(error = %re, "wallet: rollback after a rejected movement failed");
                }
                Err(e)
            }
        }
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
    /// The caller's OWN balances: the player id comes from `identity` (the gateway set
    /// it after verifying the bearer), NEVER from a body field — so a client cannot read
    /// another player's wallet.
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

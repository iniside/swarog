use sqlx::{PgConnection, PgPool};
use walletapi::{Balance, Currency};

/// True iff a store error is the balance CHECK firing (SQLSTATE 23514 on
/// `balances_amount_check`). The constraint carries BOTH bounds, so this one predicate
/// covers the two ways a movement can leave the legal range: a debit below zero
/// (insufficient funds) and a credit past the 10^15 ceiling. Matched narrowly on the
/// constraint name — the same discipline as inventory's `holdings_quantity_check` — so
/// no unrelated future CHECK ever rides this 409 mapping.
pub(crate) fn is_out_of_range(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23514") && db.constraint() == Some("balances_amount_check")
    })
}

/// True iff a store error is the in-module FK from `balances.currency` to the currency
/// catalog (SQLSTATE 23503 on the EXPLICITLY named `balances_currency_fkey`): the caller
/// named a currency the catalog does not hold. Named in the DDL rather than left to
/// Postgres's auto-naming so this match is on a constraint WE own.
pub(crate) fn is_unknown_currency(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23503") && db.constraint() == Some("balances_currency_fkey")
    })
}

/// True for a Postgres "invalid text representation" (22P02) — the contract carries
/// `player_id: String` while the columns are `uuid`, so every statement casts `$n::uuid`
/// and a malformed id arrives as this SQLSTATE. Without the arm it would be a 500
/// instead of a 400 (cf. `modules/characters/src/store.rs`'s `is_invalid_uuid`).
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// The ledger row a duplicate `idempotency_key` collides with, re-read on the SAME
/// connection that lost the `ON CONFLICT DO NOTHING` race. `player_id` is the
/// DB-canonical text of the stored uuid.
pub(crate) struct ExistingMovement {
    pub(crate) player_id: String,
    pub(crate) currency: String,
    pub(crate) delta: i64,
    pub(crate) reason: String,
    pub(crate) balance_after: i64,
}

// ============================================================================
// Store — the SQL layer. Every write takes `&mut PgConnection` (never the pool) so the
// ONE movement authority runs identically under a pool-owned transaction and under the
// event plane's HANDED delivery transaction; reads use the pool.
// ============================================================================

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    /// Step 1 of a movement: claim the idempotency key by appending the ledger row.
    /// `balance_after` is written as a placeholder `0` and corrected by
    /// [`Store::set_balance_after_tx`] once the balance update returns the real value.
    ///
    /// `None` means the key was already used — the caller must NOT treat that as an
    /// error; it is the dedup gate firing. Returns `(ledger_id, canonical_player_id)`:
    /// the player id comes back through `RETURNING player_id::text` so the emitted
    /// `wallet.changed` carries the DB-canonical spelling rather than the caller's
    /// (the same discipline as `characters`' create/delete emits).
    ///
    /// The ledger INSERT is deliberately FIRST. If the balance moved first, a duplicate
    /// key would be detected only after the money had already moved twice.
    pub(crate) async fn insert_ledger_tx(
        &self,
        conn: &mut PgConnection,
        idempotency_key: &str,
        player_id: &str,
        currency: &str,
        delta: i64,
        reason: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as(
            "INSERT INTO wallet.ledger \
                 (idempotency_key, player_id, currency, delta, reason, balance_after) \
             VALUES ($1, $2::uuid, $3, $4, $5, 0) \
             ON CONFLICT (idempotency_key) DO NOTHING \
             RETURNING id::text, player_id::text",
        )
        .bind(idempotency_key)
        .bind(player_id)
        .bind(currency)
        .bind(delta)
        .bind(reason)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Re-reads the row that won the key, ON THE SAME CONNECTION as the losing INSERT.
    /// Under READ COMMITTED (Postgres's default, and nothing here changes it) the INSERT
    /// waited for the conflicting transaction to finish, and this statement takes a fresh
    /// snapshot afterwards — so the committed row is visible. Under REPEATABLE READ it
    /// would come back `None`, which is why the caller has an explicit arm for that
    /// instead of an `unwrap`.
    pub(crate) async fn existing_ledger_tx(
        &self,
        conn: &mut PgConnection,
        idempotency_key: &str,
    ) -> Result<Option<ExistingMovement>, sqlx::Error> {
        let row: Option<(String, String, i64, String, i64)> = sqlx::query_as(
            "SELECT player_id::text, currency, delta, reason, balance_after \
               FROM wallet.ledger WHERE idempotency_key = $1",
        )
        .bind(idempotency_key)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(
            |(player_id, currency, delta, reason, balance_after)| ExistingMovement {
                player_id,
                currency,
                delta,
                reason,
                balance_after,
            },
        ))
    }

    /// Step 3: moves the balance by the SIGNED `delta` and returns the resulting amount.
    /// A debit passes a negative `delta`; a debit against a player with no row at all
    /// attempts the plain INSERT and is rejected by the CHECK exactly like an
    /// over-drawn existing row (both are 23514 → 409).
    ///
    /// No advisory lock: this is a single `amount + $delta`, so the row lock serializes
    /// concurrent movements on one `(player, currency)` and the CHECK rejects the loser
    /// — unlike characters' count-then-write cap gate, which needs one.
    pub(crate) async fn apply_balance_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        currency: &str,
        delta: i64,
    ) -> Result<i64, sqlx::Error> {
        let (amount,): (i64,) = sqlx::query_as(
            "INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid, $2, $3) \
             ON CONFLICT (player_id, currency) DO UPDATE \
                SET amount = wallet.balances.amount + $3, updated_at = now() \
             RETURNING amount",
        )
        .bind(player_id)
        .bind(currency)
        .bind(delta)
        .fetch_one(&mut *conn)
        .await?;
        Ok(amount)
    }

    /// Step 4: stamps the ledger row with the balance the movement produced, so a replay
    /// of the same key can answer with the ORIGINAL movement's result instead of a live
    /// balance read (which is what would break `#[retry_safe]`).
    ///
    /// It ALSO stamps the ordering value `seq`, and that is the whole reason this
    /// statement is where it is: it runs AFTER [`Store::apply_balance_tx`], i.e. while
    /// the balance row lock is held, so `nextval` is drawn in balance-application order.
    /// The `bigserial` default is assigned during the ledger INSERT — a full round trip
    /// BEFORE the lock — so leaving `seq` at its default lets two concurrent credits
    /// order as (`seq=1`, balance 200), (`seq=2`, balance 100), i.e. an `ORDER BY seq`
    /// read of an append-only ledger showing the running balance going DOWN on a credit.
    /// The insert-time value is therefore a placeholder, always overwritten before
    /// commit; the cost is one wasted sequence value per movement.
    pub(crate) async fn set_balance_after_tx(
        &self,
        conn: &mut PgConnection,
        ledger_id: &str,
        balance_after: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE wallet.ledger \
                SET balance_after = $1, seq = nextval('wallet.ledger_seq_seq') \
              WHERE id = $2::uuid",
        )
        .bind(balance_after)
        .bind(ledger_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Catalog membership check on a HANDED connection. Its caller is the durable
    /// starter-grant handler, and the pre-check is REQUIRED there rather than defensive
    /// (same construction as inventory's `item_exists_exec`). Letting the FK fire instead
    /// aborts the DELIVERY transaction, and then neither thing the handler may do is
    /// safe: posture A mandates swallowing a data-quality problem and returning `Ok`, but
    /// an `Ok` on an aborted transaction makes the plane's checkpoint `UPDATE` fail with
    /// 25P02 (`core/asyncevents/src/worker.rs:268`); returning `Err` instead is recovered
    /// by the plane's `ROLLBACK TO SAVEPOINT deliver` (`:257`/`:288` — no 25P02) but backs
    /// the subscription off and pauses it after 20 failures, for every subsequent player.
    /// Not firing the FK at all is the only way to satisfy both.
    // The grant handler lands in the next step of this rollout; the probe is placed with
    // the rest of the SQL layer because it is what makes that path abort-free.
    #[allow(dead_code)]
    pub(crate) async fn currency_exists_tx(
        &self,
        conn: &mut PgConnection,
        code: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<i32> = sqlx::query_scalar("SELECT 1 FROM wallet.currencies WHERE code = $1")
            .bind(code)
            .fetch_optional(&mut *conn)
            .await?;
        Ok(row.is_some())
    }

    /// EVERY balance row the player holds, including one debited back to zero — the row
    /// survives a debit to zero and hiding it would make `GET /wallet/me` disagree with
    /// the ledger and with the admin drill-down. A malformed id is a genuine miss (an
    /// empty list), matching the contract's "an unknown player holds nothing".
    ///
    /// No cursor and no hard LIMIT, deliberately: unlike inventory's per-owner item set
    /// this list is bounded by the operator-curated currency catalog, not by anything a
    /// caller controls — so there is no surplus to silently truncate.
    pub(crate) async fn list_balances(&self, player_id: &str) -> Result<Vec<Balance>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, i64)>(
            "SELECT currency, amount FROM wallet.balances WHERE player_id = $1::uuid \
              ORDER BY currency",
        )
        .bind(player_id)
        .fetch_all(&self.pool)
        .await;
        match res {
            Ok(rows) => Ok(rows
                .into_iter()
                .map(|(currency, amount)| Balance { currency, amount })
                .collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The whole currency catalog. Bounded by the operator-curated table, so no cursor
    /// (see [`Store::list_balances`]).
    pub(crate) async fn list_currencies(&self) -> Result<Vec<Currency>, sqlx::Error> {
        let rows: Vec<(String, String, String, i32)> = sqlx::query_as(
            "SELECT code, display_name, kind, decimals FROM wallet.currencies ORDER BY code",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(code, display_name, kind, decimals)| Currency {
                code,
                display_name,
                kind,
                decimals,
            })
            .collect())
    }

    /// Upserts one catalog row. Used by the `WALLET_DEV_SEED` upsert (self-healing: a
    /// hand-edited dev row is restored on the next boot).
    pub(crate) async fn upsert_currency_tx(
        &self,
        conn: &mut PgConnection,
        code: &str,
        display_name: &str,
        kind: &str,
        decimals: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO wallet.currencies (code, display_name, kind, decimals) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (code) DO UPDATE \
                SET display_name = EXCLUDED.display_name, \
                    kind = EXCLUDED.kind, \
                    decimals = EXCLUDED.decimals",
        )
        .bind(code)
        .bind(display_name)
        .bind(kind)
        .bind(decimals)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
}

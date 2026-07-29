use sqlx::{PgConnection, PgPool};
use walletapi::{Balance, Currency};

/// The balance CHECK firing: one constraint carries BOTH bounds, so this covers a debit
/// below zero and a credit past the ceiling. Matched on the constraint NAME so no
/// unrelated future CHECK rides this 409 mapping.
pub(crate) fn is_out_of_range(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23514") && db.constraint() == Some("balances_amount_check")
    })
}

/// The caller named a currency the catalog does not hold. The FK is named EXPLICITLY in
/// the DDL, rather than left to Postgres's auto-naming, so this matches a constraint we own.
pub(crate) fn is_unknown_currency(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23503") && db.constraint() == Some("balances_currency_fkey")
    })
}

/// "Invalid text representation": the contract carries `player_id: String` while the
/// columns are `uuid`, so a malformed id arrives as this SQLSTATE from the `$n::uuid` cast
/// — without the arm it is a 500 instead of a 400.
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// Why [`Store::apply_balance_tx`] refused the movement. The two named variants are the
/// verdicts it decides ITSELF, on the missing-row debit path where no statement that could
/// carry the right SQLSTATE ever runs; everything else is the DB's own answer.
#[derive(Debug)]
pub(crate) enum BalanceError {
    /// The catalog does not hold the currency — the `is_unknown_currency` verdict (400).
    UnknownCurrency,
    /// The movement lands outside `balances_amount_check` — the `is_out_of_range`
    /// verdict (409).
    OutOfRange,
    Sql(sqlx::Error),
}

/// The ledger row a duplicate `idempotency_key` collides with; `player_id` is the
/// DB-canonical text of the stored uuid.
pub(crate) struct ExistingMovement {
    pub(crate) player_id: String,
    pub(crate) currency: String,
    pub(crate) delta: i64,
    pub(crate) reason: String,
    pub(crate) balance_after: i64,
}

/// Every write takes `&mut PgConnection`, never the pool, so the one movement authority
/// runs identically under a pool-owned transaction and under the event plane's HANDED
/// delivery transaction; reads use the pool.
pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    /// The dedup gate: it runs BEFORE the balance moves, because a duplicate key detected
    /// afterwards is money already moved twice. `None` is that gate firing (the key was
    /// used), not an error.
    ///
    /// `balance_after` is a placeholder `0` until [`Store::set_balance_after_tx`], and the
    /// returned player id is the DB-canonical spelling, so the emitted `wallet.changed`
    /// carries it rather than the caller's.
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
    /// Under READ COMMITTED (the default, unchanged here) that INSERT waited for the
    /// conflicting transaction and this statement takes a fresh snapshot afterwards, so the
    /// row is visible; under REPEATABLE READ it would be `None` — hence the caller's
    /// explicit arm instead of an `unwrap`.
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

    /// Applies the SIGNED `delta`, UPDATE first: Postgres evaluates a table CHECK against the
    /// TENTATIVE INSERT row before it detects the conflict and routes to `DO UPDATE`, so an
    /// upsert alone trips `balances_amount_check` on a negative `$3` whatever the balance is.
    ///
    /// The upsert survives as the zero-rows-matched fallback for a CREDIT, keeping the
    /// missing-row behaviour: a first-ever credit lands (two concurrent ones both miss the
    /// UPDATE, one INSERTs and the other takes `DO UPDATE`, so neither is lost) and an unknown
    /// currency is 23503 → 400.
    ///
    /// A DEBIT never reaches that fallback: with no row the outcome is already decided, and
    /// the tentative negative row trips `balances_amount_check` BEFORE the FK trigger runs, so
    /// an unknown currency would answer 409 where the contract (and the credit direction)
    /// promises 400. The catalog probe therefore decides both verdicts explicitly, and it runs
    /// HERE — after an UPDATE that matched nothing and so left the caller's transaction
    /// usable, never after a statement that could have aborted it into 25P02. A debit racing
    /// the FIRST-EVER credit for its `(player, currency)` is still [`BalanceError::OutOfRange`]
    /// (409) even if that credit commits in between — no key is consumed, so the caller
    /// retries.
    ///
    /// No advisory lock: a single `amount + $delta` means the row lock serializes concurrent
    /// movements on one `(player, currency)` and the CHECK rejects the loser.
    pub(crate) async fn apply_balance_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        currency: &str,
        delta: i64,
    ) -> Result<i64, BalanceError> {
        let updated: Option<(i64,)> = sqlx::query_as(
            "UPDATE wallet.balances SET amount = amount + $3, updated_at = now() \
              WHERE player_id = $1::uuid AND currency = $2 \
             RETURNING amount",
        )
        .bind(player_id)
        .bind(currency)
        .bind(delta)
        .fetch_optional(&mut *conn)
        .await
        .map_err(BalanceError::Sql)?;
        if let Some((amount,)) = updated {
            return Ok(amount);
        }

        if delta < 0 {
            let known = self
                .currency_exists_tx(conn, currency)
                .await
                .map_err(BalanceError::Sql)?;
            return Err(if known {
                BalanceError::OutOfRange
            } else {
                BalanceError::UnknownCurrency
            });
        }

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
        .await
        .map_err(BalanceError::Sql)?;
        Ok(amount)
    }

    /// Stamps the balance the movement produced, so a replay of the key answers with the
    /// ORIGINAL result instead of a live read — the latter is what would break
    /// `#[retry_safe]`.
    ///
    /// It ALSO stamps `seq`, and that is why the statement sits HERE: it runs after
    /// [`Store::apply_balance_tx`], i.e. under the balance row lock, so `nextval` is drawn
    /// in balance-application order. The `bigserial` default is assigned during the ledger
    /// INSERT, a full round trip before that lock, so two concurrent credits could order as
    /// (`seq=1`, balance 200), (`seq=2`, balance 100) — a running balance going DOWN on a
    /// credit.
    ///
    /// The sequence is resolved through `pg_get_serial_sequence`, never a literal: the name
    /// a `bigserial` derives is not guaranteed (Postgres deconflicts it, so an existing
    /// `ledger_seq_seq` leaves the column defaulting from `ledger_seq_seq1`) and a hardcoded
    /// name would silently draw from an unrelated counter. It also survives a rename.
    pub(crate) async fn set_balance_after_tx(
        &self,
        conn: &mut PgConnection,
        ledger_id: &str,
        balance_after: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE wallet.ledger \
                SET balance_after = $1, \
                    seq = nextval(pg_get_serial_sequence('wallet.ledger', 'seq')) \
              WHERE id = $2::uuid",
        )
        .bind(balance_after)
        .bind(ledger_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Catalog membership check on a HANDED connection, used by [`Store::apply_balance_tx`]'s
    /// missing-row debit and by the durable starter-grant handler: the pre-check is REQUIRED
    /// there, not defensive. Letting the FK fire aborts the DELIVERY transaction, and then
    /// neither posture is safe — an `Ok` on an aborted transaction fails the plane's
    /// checkpoint `UPDATE` with 25P02, and an `Err` (which the plane's `ROLLBACK TO SAVEPOINT
    /// deliver` does recover) backs the subscription off and pauses it for every subsequent
    /// player. Not firing the FK is the only way to satisfy both; the handler itself lands
    /// with wallet's grant path.
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

    /// EVERY row, including a currency debited back to zero: the row survives, and hiding
    /// it would make `GET /wallet/me` disagree with the ledger. A malformed id is a miss
    /// (empty list), matching the contract's "an unknown player holds nothing".
    ///
    /// No cursor: the list is bounded by the operator-curated catalog, not by anything a
    /// caller controls, so there is no surplus to silently truncate.
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

    /// No cursor, for the reason in [`Store::list_balances`].
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

    /// Self-healing: a hand-edited dev row is restored on the next boot.
    ///
    /// An over-long code is `currencies_code_len_check` as 23514 — a boot failure here,
    /// which is right for a hardcoded dev code, but a CALLER-facing writer must map it to a
    /// 400. It cannot be mistaken for insufficient funds: `is_out_of_range` is
    /// constraint-named to `balances_amount_check`.
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

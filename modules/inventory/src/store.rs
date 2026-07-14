use inventoryapi::Holding;
use sqlx::{PgConnection, PgPool};

use crate::Owner;

/// Hard safety-belt ceiling on a single holdings-list response (same rationale as
/// characters' LIST_HARD_LIMIT): unbounded fetch_all diverges monolith vs split frame caps.
/// KNOWN GAP (recorded, not fixed here): there is no per-owner distinct-item cap, so an owner
/// holding more than HOLDINGS_HARD_LIMIT distinct items has the surplus silently truncated
/// from list views. Acceptable because the item catalogue is small; revisit if item variety grows.
pub(crate) const HOLDINGS_HARD_LIMIT: i64 = 1000;

/// True iff a store error is the holdings-quantity DB CHECK firing (SQLSTATE 23514
/// on `holdings_quantity_check`): a LEGAL single grant pushed the accumulated
/// `ON CONFLICT` sum past the 2_000_000 state ceiling. Matched narrowly on the
/// constraint name so no unrelated future CHECK ever rides this mapping.
pub(crate) fn is_holdings_cap_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23514") && db.constraint() == Some("holdings_quantity_check")
    })
}

// ============================================================================
// Store — the SQL layer. Grant/clear have a `&mut PgConnection` variant so the
// event-driven effect runs INSIDE the durable subscription delivery tx; reads use the pool.
// ============================================================================

/// Gameplay sanity cap on a SINGLE grant — a million-item stack is never a
/// legitimate grant. The DB CHECK sits higher (2_000_000) because it bounds
/// accumulated STATE (the `ON CONFLICT` sum of repeated grants), not one grant.
pub(crate) const MAX_HOLDING_QTY: i64 = 1_000_000;

/// Rejection from the one quantity-policy authority: the offending value.
#[derive(Debug)]
pub(crate) struct QuantityError(i64);

impl std::fmt::Display for QuantityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "quantity {} out of range (must be 1..={MAX_HOLDING_QTY})", self.0)
    }
}
impl std::error::Error for QuantityError {}

/// THE single quantity-policy authority. Every writer path routes its quantity
/// through here (`grant_starter`'s config-driven grant, `Holdings::grant`'s HTTP
/// IAP grant, and the `grant_exec` belt), so the decision of what is grantable
/// lives in exactly one place — never re-derived at a call site. Enforces
/// `0 < qty <= MAX_HOLDING_QTY`; the DB CHECK (`0..=2_000_000`) is the deeper
/// authority that also covers a raw `psql` writer bypassing this function.
pub(crate) fn validate_quantity(qty: i64) -> Result<i64, QuantityError> {
    if qty > 0 && qty <= MAX_HOLDING_QTY {
        Ok(qty)
    } else {
        Err(QuantityError(qty))
    }
}

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

/// One row of the admin owners list: an owner with its holding count + total qty.
pub(crate) struct OwnerStat {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) items: i64,
    pub(crate) qty: i64,
}

impl Store {
    /// Grants `qty` of `item_id` to `owner` on the given connection (a tx, so the
    /// grant + the checkpoint commit together in the delivery tx). ON CONFLICT ADDS to the existing
    /// stack (`quantity = quantity + EXCLUDED.quantity`) — the exact Go math.
    pub(crate) async fn grant_exec(
        &self,
        conn: &mut PgConnection,
        owner: &Owner,
        item_id: &str,
        qty: i64,
    ) -> Result<(), sqlx::Error> {
        // Posture C (belt): the ONE shared writer refuses an out-of-policy quantity
        // even if a future ingress path forgets to validate first. Both current
        // callers (grant_starter via posture A, grant_pool via posture B) already
        // validate, so on today's paths this never fires — it surfaces a programming
        // error as an internal error, never a raw 22003/23514 from the DB.
        // WARNING to future durable callers: on a delivery tx a belt failure maps to
        // a bus transport error and PAUSES the subscription — which is exactly why
        // posture A must degrade a bad value BEFORE it can ever reach this belt.
        validate_quantity(qty).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        sqlx::query(
            "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
             VALUES ($1, $2::uuid, $3, $4) \
             ON CONFLICT (owner_type, owner_id, item_id) \
             DO UPDATE SET quantity = inventory.holdings.quantity + EXCLUDED.quantity",
        )
        .bind(&owner.otype)
        .bind(&owner.id)
        .bind(item_id)
        .bind(qty)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// The pool-backed grant (the player IAP path): acquires a connection and runs
    /// `grant_exec` against it. Not the durable-event path — that hands its own tx.
    pub(crate) async fn grant_pool(&self, owner: &Owner, item_id: &str, qty: i64) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.grant_exec(&mut conn, owner, item_id, qty).await
    }

    pub(crate) async fn list(&self, owner: &Owner) -> Result<Vec<Holding>, sqlx::Error> {
        let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(&format!(
            "SELECT h.owner_type, h.owner_id::text, h.item_id, i.name, h.quantity::bigint \
               FROM inventory.holdings h \
               JOIN inventory.items i ON i.id = h.item_id \
              WHERE h.owner_type = $1 AND h.owner_id = $2::uuid \
              ORDER BY h.item_id LIMIT {HOLDINGS_HARD_LIMIT}"
        ))
        .bind(&owner.otype)
        .bind(&owner.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(owner_type, owner_id, item_id, item_name, quantity)| Holding {
                owner_type,
                owner_id,
                item_id,
                item_name,
                quantity,
            })
            .collect())
    }

    /// Removes every holding of an owner — the event-driven cleanup when a character
    /// (or later a player) is deleted. Runs on the sink's tx (`&mut PgConnection`).
    pub(crate) async fn clear_owner_exec(&self, conn: &mut PgConnection, owner: &Owner) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM inventory.holdings WHERE owner_type = $1 AND owner_id = $2::uuid")
            .bind(&owner.otype)
            .bind(&owner.id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }

    /// `item_exists` on a HANDED connection — the durable-delivery variant (same
    /// shape as `grant_exec`): `grant_starter` validates the configured starter
    /// item on the SAME delivery tx it inserts on, so the check and the insert
    /// are one atomic unit (a pool-backed check would be a different connection —
    /// TOCTOU against the tx's snapshot).
    pub(crate) async fn item_exists_exec(&self, conn: &mut PgConnection, item_id: &str) -> Result<bool, sqlx::Error> {
        let row: Option<i32> = sqlx::query_scalar("SELECT 1 FROM inventory.items WHERE id = $1")
            .bind(item_id)
            .fetch_optional(&mut *conn)
            .await?;
        Ok(row.is_some())
    }

    /// The pool-backed item check (the player IAP path): acquires a connection and
    /// runs `item_exists_exec` against it. Not the durable-event path.
    pub(crate) async fn item_exists(&self, item_id: &str) -> Result<bool, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.item_exists_exec(&mut conn, item_id).await
    }

    pub(crate) async fn stats(&self) -> Result<(i64, i64), sqlx::Error> {
        let row: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM inventory.holdings), \
                    (SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub(crate) async fn list_owners(&self, limit: i64) -> Result<Vec<OwnerStat>, sqlx::Error> {
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT owner_type, owner_id::text, count(*), coalesce(sum(quantity),0)::bigint \
               FROM inventory.holdings \
              GROUP BY owner_type, owner_id \
              ORDER BY owner_type, owner_id \
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(owner_type, owner_id, items, qty)| OwnerStat { owner_type, owner_id, items, qty })
            .collect())
    }
}

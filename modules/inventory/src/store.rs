use inventoryapi::Holding;
use sqlx::{PgConnection, PgPool};

use crate::Owner;

/// Hard safety-belt ceiling on a single holdings-list response (same rationale as
/// characters' LIST_HARD_LIMIT): unbounded fetch_all diverges monolith vs split frame caps.
/// KNOWN GAP (recorded, not fixed here): there is no per-owner distinct-item cap, so an owner
/// holding more than HOLDINGS_HARD_LIMIT distinct items has the surplus silently truncated
/// from list views. Acceptable because the item catalogue is small; revisit if item variety grows.
pub(crate) const HOLDINGS_HARD_LIMIT: i64 = 1000;

pub(crate) fn player_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = player_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { player_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"inventory.player/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

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

/// The admin Inventory tab's page size, as wallet's, mail's and accounts' tables.
pub(crate) const ADMIN_PAGE_SIZE: usize = 25;

/// The narrowing of a player's admin holdings page; `None` narrows nothing.
pub(crate) struct HoldingsFilter<'a> {
    /// `"player"` or `"character"`.
    pub(crate) owner_type: Option<&'a str>,
    /// An `items.kind`.
    pub(crate) kind: Option<&'a str>,
    /// A case-insensitive substring of the item id or name.
    pub(crate) needle: Option<&'a str>,
}

/// A holding's primary key, the admin page's keyset position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HoldingKey {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) item_id: String,
}

/// One holding of the admin Inventory tab: the key, the item's name and its REAL `kind`
/// (the in-module `items` join). Admin-only; the player-facing `inventoryapi::Holding`
/// carries no `kind`.
pub(crate) struct AdminHolding {
    pub(crate) key: HoldingKey,
    pub(crate) item_name: String,
    pub(crate) kind: String,
    pub(crate) quantity: i64,
}

/// One page of a player's admin holdings and the exact count of every holding the filter
/// keeps, cursor aside.
pub(crate) struct HoldingsPage {
    pub(crate) rows: Vec<AdminHolding>,
    pub(crate) total: i64,
}

/// `needle` as an `ILIKE ... ESCAPE '\'` substring pattern: `%`, `_` and `\` match literally.
fn substring_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
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
        // callers (grant_starter via posture A, Holdings::grant via posture B) already
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

    pub(crate) async fn lock_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(player_lock_key(player_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    pub(crate) async fn is_player_purged_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let found: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM inventory.purged_players WHERE player_id = $1::uuid")
                .bind(player_id)
                .fetch_optional(&mut *conn)
                .await?;
        Ok(found.is_some())
    }

    /// The purge backlog: tombstones the sweep still owes work
    /// on, and the oldest one's UTC day `YYYY-MM-DD` (`""` when nothing is outstanding).
    /// `purged_players_pending_idx` serves the predicate, so the cost tracks
    /// outstanding work rather than the permanent row per deleted player.
    pub(crate) async fn pending_purges(&self) -> Result<(i64, String), sqlx::Error> {
        let (count, oldest) = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT count(*)::bigint, to_char(min(created_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD') \
             FROM inventory.purged_players WHERE swept_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((count, oldest.unwrap_or_default()))
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

    /// One keyset page, in primary-key order after `after`, of the holdings a player owns
    /// directly and through its `character_owners` characters, with the exact count of every
    /// holding `filter` keeps. The count rides a one-row join, so a page past the end still
    /// answers it; it is a `count(*)` because no index covers `kind` or the name.
    pub(crate) async fn player_holdings_page(
        &self,
        player_id: &str,
        filter: &HoldingsFilter<'_>,
        after: Option<&HoldingKey>,
        limit: i64,
    ) -> Result<HoldingsPage, sqlx::Error> {
        type PageRow = (i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>);
        let rows: Vec<PageRow> = sqlx::query_as(
            "WITH mine AS ( \
               SELECT h.owner_type, h.owner_id, h.item_id, i.name, i.kind, h.quantity \
                 FROM inventory.holdings h \
                 JOIN inventory.items i ON i.id = h.item_id \
                WHERE ((h.owner_type = 'player' AND h.owner_id = $1::uuid) \
                    OR (h.owner_type = 'character' AND h.owner_id IN \
                          (SELECT character_id FROM inventory.character_owners WHERE player_id = $1::uuid))) \
                  AND ($2::text IS NULL OR h.owner_type = $2::text) \
                  AND ($3::text IS NULL OR i.kind = $3::text) \
                  AND ($4::text IS NULL OR h.item_id ILIKE $4::text ESCAPE '\\' OR i.name ILIKE $4::text ESCAPE '\\') \
             ) \
             SELECT t.total, p.owner_type, p.owner_id::text, p.item_id, p.name, p.kind, p.quantity::bigint \
               FROM (SELECT count(*)::bigint AS total FROM mine) t \
               LEFT JOIN LATERAL ( \
                 SELECT * FROM mine \
                  WHERE $5::text IS NULL OR (owner_type, owner_id, item_id) > ($5::text, $6::uuid, $7::text) \
                  ORDER BY owner_type, owner_id, item_id \
                  LIMIT $8 \
               ) p ON true \
              ORDER BY p.owner_type, p.owner_id, p.item_id",
        )
        .bind(player_id)
        .bind(filter.owner_type)
        .bind(filter.kind)
        .bind(filter.needle.map(substring_pattern))
        .bind(after.map(|k| k.owner_type.as_str()))
        .bind(after.map(|k| k.owner_id.as_str()))
        .bind(after.map(|k| k.item_id.as_str()))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let total = rows.first().map_or(0, |r| r.0);
        let rows = rows
            .into_iter()
            .filter_map(|(_, owner_type, owner_id, item_id, item_name, kind, quantity)| {
                Some(AdminHolding {
                    key: HoldingKey { owner_type: owner_type?, owner_id: owner_id?, item_id: item_id? },
                    item_name: item_name?,
                    kind: kind?,
                    quantity: quantity?,
                })
            })
            .collect();
        Ok(HoldingsPage { rows, total })
    }

    /// The catalog's item kinds, in order.
    pub(crate) async fn item_kinds(&self) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT DISTINCT kind FROM inventory.items ORDER BY kind")
            .fetch_all(&self.pool)
            .await
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

    /// The item check on a HANDED connection (same shape as `grant_exec`):
    /// `grant_starter` validates the configured starter
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

    /// The catalog's item ids, in order: the operator grant form's Item choices.
    pub(crate) async fn item_ids(&self) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT id FROM inventory.items ORDER BY id")
            .fetch_all(&self.pool)
            .await
    }

    /// The player's characters in `inventory.character_owners`, in id order: the operator
    /// grant form's Character choices.
    pub(crate) async fn player_characters(&self, player_id: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT character_id::text FROM inventory.character_owners \
              WHERE player_id = $1::uuid ORDER BY character_id",
        )
        .bind(player_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Records an operator grant's request id on the grant's own transaction; `false` when
    /// it is already recorded, so the grant was applied before.
    pub(crate) async fn claim_operator_grant_exec(
        &self,
        conn: &mut PgConnection,
        request_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("INSERT INTO inventory.operator_grants (request_id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(request_id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Whether `character_owners` records `character_id` as `player_id`'s, read on the
    /// caller's transaction.
    pub(crate) async fn is_character_of_exec(
        &self,
        conn: &mut PgConnection,
        character_id: &str,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM inventory.character_owners WHERE character_id = $1::uuid AND player_id = $2::uuid",
        )
        .bind(character_id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.is_some())
    }

    /// One holding's item name and quantity, or `None` when the row is gone.
    pub(crate) async fn holding(&self, key: &HoldingKey) -> Result<Option<(String, i64)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT i.name, h.quantity::bigint \
               FROM inventory.holdings h \
               JOIN inventory.items i ON i.id = h.item_id \
              WHERE h.owner_type = $1 AND h.owner_id = $2::uuid AND h.item_id = $3",
        )
        .bind(&key.owner_type)
        .bind(&key.owner_id)
        .bind(&key.item_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Deletes one holding row whole; the rows removed, 0 or 1.
    pub(crate) async fn delete_holding_exec(&self, conn: &mut PgConnection, key: &HoldingKey) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM inventory.holdings WHERE owner_type = $1 AND owner_id = $2::uuid AND item_id = $3",
        )
        .bind(&key.owner_type)
        .bind(&key.owner_id)
        .bind(&key.item_id)
        .execute(&mut *conn)
        .await?;
        Ok(res.rows_affected())
    }
}

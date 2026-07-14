use charactersapi::Character;
use sqlx::{PgConnection, PgPool};

/// Hard safety-belt ceiling on a single list response so it is never unbounded across
/// topologies (monolith direct call has no frame; split has 16 MiB internal / 1 MiB player
/// frame caps). A belt, not the policy limit: the configurable per-player cap in `create()`
/// (added by the P2 cap step, and clamped to this ceiling so `create` can never admit more
/// characters than `list` can return) is the primary bound. KNOWN GAP (recorded, not fixed
/// here, same shape as inventory's `HOLDINGS_HARD_LIMIT` at `modules/inventory/src/store.rs:6-10`):
/// there is no cursor / `has_more` on this list, so until the per-player cap lands this ceiling
/// is also the de-facto per-player limit and silently truncates any surplus beyond it.
pub(crate) const LIST_HARD_LIMIT: i64 = 1000;

/// The column list every read/insert projects, `created_at` rendered as text so it
/// flows through as the `Character::created_at` String. Kept in one place so the
/// tuple shape below matches every query.
const COLS: &str = "id::text, player_id::text, name, class, created_at::text";

/// One scanned row — the five text columns of [`COLS`], in order.
type Row = (String, String, String, String, String);

fn to_character((id, player_id, name, class, created_at): Row) -> Character {
    Character { id, player_id, name, class, created_at }
}

/// `true` for a Postgres "invalid text representation" (22P02) — a malformed uuid in
/// the request — so callers treat it as not-found rather than a 500 (Go's `invalidUUID`).
fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

// ============================================================================
// Store — the SQL layer. Write paths take `&mut PgConnection` so the domain row
// and its durable event append commit in ONE tx (create/delete); reads use the pool.
// ============================================================================

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    /// Inserts a character on the given connection (a tx, so the row + its durable
    /// event append commit together) and returns it (id/created_at from `INSERT ... RETURNING`).
    pub(crate) async fn create_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        name: &str,
        class: &str,
    ) -> Result<Character, sqlx::Error> {
        let row: Row = sqlx::query_as(&format!(
            "INSERT INTO characters.characters (player_id, name, class) \
             VALUES ($1::uuid, $2, $3) RETURNING {COLS}"
        ))
        .bind(player_id)
        .bind(name)
        .bind(class)
        .fetch_one(&mut *conn)
        .await?;
        Ok(to_character(row))
    }

    pub(crate) async fn list_by_player(&self, player_id: &str) -> Result<Vec<Character>, sqlx::Error> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM characters.characters WHERE player_id = $1::uuid \
             ORDER BY created_at, id LIMIT {LIST_HARD_LIMIT}"
        ))
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(to_character).collect())
    }

    /// Fetches one character. A malformed id (22P02) is treated as `Ok(None)`, like a
    /// genuine miss — a real DB error propagates.
    pub(crate) async fn get(&self, id: &str) -> Result<Option<Character>, sqlx::Error> {
        let res = sqlx::query_as::<_, Row>(&format!(
            "SELECT {COLS} FROM characters.characters WHERE id = $1::uuid"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(row.map(to_character)),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Deletes a character only if it belongs to `player_id`; returns the removed
    /// row's canonical `(id, player_id)`, or `None` if nothing matched. A malformed id
    /// is "nothing deleted" (Go's behaviour). The `RETURNING id::text, player_id::text`
    /// yields BOTH DB-canonical uuids (lowercase, unbraced), so the caller emits the
    /// event with the canonical form of BOTH fields regardless of how the client
    /// spelled the id/player_id arguments — at full parity with `create_tx` (which
    /// emits `c.id`/`c.player_id` from its own `RETURNING`).
    pub(crate) async fn delete_owned_tx(
        &self,
        conn: &mut PgConnection,
        id: &str,
        player_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(
            "DELETE FROM characters.characters WHERE id = $1::uuid AND player_id = $2::uuid \
             RETURNING id::text, player_id::text",
        )
        .bind(id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(Some(row)) => Ok(Some(row)),
            Ok(None) => Ok(None),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn count(&self) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Counts how many characters a player owns, ON THE GIVEN connection (the create
    /// tx) so the count runs AFTER and UNDER the per-player advisory lock, within the
    /// same snapshot as the subsequent insert — the cap gate is only race-safe if this
    /// count and the `create_tx` insert share one serialized transaction.
    pub(crate) async fn count_owned_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM characters.characters WHERE player_id = $1::uuid")
                .bind(player_id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(n)
    }

    pub(crate) async fn list_all(&self, limit: i64) -> Result<Vec<Character>, sqlx::Error> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM characters.characters ORDER BY created_at DESC LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(to_character).collect())
    }
}

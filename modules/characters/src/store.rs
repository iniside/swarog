use std::collections::BTreeMap;

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

/// One row of the Characters tab: what the panel draws, not the wire `Character`, whose
/// `created_at` is the `timestamptz::text` rendering.
pub(crate) struct PanelCharacter {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) class: String,
    /// UTC RFC3339 instant, `YYYY-MM-DDTHH:MM:SSZ`.
    pub(crate) created: String,
}

fn to_character((id, player_id, name, class, created_at): Row) -> Character {
    Character { id, player_id, name, class, created_at }
}

/// `true` for a Postgres "invalid text representation" (22P02) — a malformed uuid in
/// the request — so callers treat it as not-found rather than a 500 (Go's `invalidUUID`).
fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// `true` for the 8-4-4-4-12 hex shape `::uuid` accepts, so a batch read can drop a
/// malformed ref before the bind: one bad element fails the whole cast with 22P02.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Per-player advisory lock key: FNV-1a over a `characters.player/` namespace plus the uuid
/// normalized to Postgres's uuid-equality form, so every spelling the `$1::uuid` row SQL
/// treats as one player takes the SAME lock.
fn player_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = player_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { player_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"characters.player/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
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

    /// The Characters tab's rows, ordered like list_by_player, with created_at as the UTC
    /// RFC3339 instant `YYYY-MM-DDTHH:MM:SSZ` the accounts panels draw.
    pub(crate) async fn list_for_panel(&self, player_id: &str) -> Result<Vec<PanelCharacter>, sqlx::Error> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(&format!(
            "SELECT id::text, name, class, \
             to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM characters.characters WHERE player_id = $1::uuid \
             ORDER BY created_at, id LIMIT {LIST_HARD_LIMIT}"
        ))
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, class, created)| PanelCharacter { id, name, class, created })
            .collect())
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

    /// Each player ref's character count, keyed by the ref exactly as given, 0 for a player
    /// with none. The refs bind as one `text[]` parameter; `characters_player_idx` serves the
    /// join. A malformed ref fails the whole read with 22P02. The caller bounds `refs`.
    pub(crate) async fn counts_by_player(
        &self,
        refs: &[String],
    ) -> Result<BTreeMap<String, i64>, sqlx::Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT r.id, count(c.id)::bigint \
             FROM unnest($1::text[]) AS r(id) \
             LEFT JOIN characters.characters c ON c.player_id = r.id::uuid \
             GROUP BY r.id",
        )
        .bind(refs)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Each character ref's name, keyed by the ref exactly as given. A ref that is not a
    /// uuid, or names no character, is left out rather than failing the batch. The caller
    /// bounds `refs`.
    pub(crate) async fn names_by_id(
        &self,
        refs: &[String],
    ) -> Result<BTreeMap<String, String>, sqlx::Error> {
        let refs: Vec<&str> = refs.iter().map(String::as_str).filter(|r| is_uuid(r)).collect();
        if refs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT r.id, c.name \
             FROM unnest($1::text[]) AS r(id) \
             JOIN characters.characters c ON c.id = r.id::uuid",
        )
        .bind(refs)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
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

    /// Taken ON THE TX CONNECTION — a separate pool connection would lose the
    /// serialization — and released at commit/rollback.
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

    /// `true` once this player's deletion tombstone is committed. A plain read: its caller
    /// holds [`Store::lock_player_tx`] first, so the sweep either committed before the read
    /// or waits for the caller's transaction.
    pub(crate) async fn is_player_purged_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let found: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM characters.purged_players WHERE player_id = $1::uuid")
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
             FROM characters.purged_players WHERE swept_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((count, oldest.unwrap_or_default()))
    }

    pub(crate) async fn plant_purged_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        handle: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO characters.purged_players (player_id, handle) VALUES ($1::uuid, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(player_id)
        .bind(handle)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
}

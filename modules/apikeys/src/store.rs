//! The SQL layer for the `apikeys` schema — pure persistence, no capability/edge
//! knowledge. All reads and writes use the pool; keys are stored in plaintext (the
//! same trust model as `accounts.sessions.token`), so the admin page can display them
//! and lookup is a plain equality match. Hashing at rest is future hardening.

use apikeysapi::KeyRecord;
use sqlx::PgPool;

/// One key as the admin table shows it (Step 6): the record plus its secret, creation
/// time and revoked flag. `list` returns these; the wire [`KeyRecord`] carries only
/// `name`/`policy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyRow {
    pub name: String,
    pub key: String,
    pub policy: String,
    /// Pre-rendered "Mon DD, HH24:MI" (Postgres `to_char`, matching the accounts admin
    /// table's shape).
    pub created_at: String,
    /// `true` when `revoked_at IS NOT NULL` — the key resolves as unknown.
    pub revoked: bool,
}

pub(crate) struct Store {
    pub pool: PgPool,
}

impl Store {
    /// Resolves a key string to its [`KeyRecord`], ignoring revoked keys. `Ok(None)` is
    /// a genuine unknown/revoked key; an `Err` is a store failure the caller surfaces as
    /// infrastructure trouble, never a silent deny.
    pub async fn lookup(&self, key: &str) -> Result<Option<KeyRecord>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT name, policy FROM apikeys.keys WHERE key = $1 AND revoked_at IS NULL",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(name, policy)| KeyRecord { name, policy }))
    }

    /// Every key, newest first, for the admin table.
    pub async fn list(&self) -> Result<Vec<KeyRow>, sqlx::Error> {
        let rows: Vec<(String, String, String, String, bool)> = sqlx::query_as(
            "SELECT name, key, policy, to_char(created_at, 'Mon DD, HH24:MI'), \
                    (revoked_at IS NOT NULL) AS revoked \
               FROM apikeys.keys \
              ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(name, key, policy, created_at, revoked)| KeyRow {
                name,
                key,
                policy,
                created_at,
                revoked,
            })
            .collect())
    }

    /// Creates a new key. A duplicate `name` (primary key) or `key` (unique) surfaces as
    /// the underlying sqlx error for the caller to map.
    pub async fn insert(&self, name: &str, key: &str, policy: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO apikeys.keys (name, key, policy) VALUES ($1, $2, $3)")
            .bind(name)
            .bind(key)
            .bind(policy)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Replaces a key's policy string. A missing name is a no-op (0 rows).
    pub async fn set_policy(&self, name: &str, policy: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE apikeys.keys SET policy = $1 WHERE name = $2")
            .bind(policy)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Revokes a key by name (`revoked_at = now()`), after which `lookup` treats it as
    /// unknown. A missing name is a no-op.
    pub async fn revoke(&self, name: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE apikeys.keys SET revoked_at = now() WHERE name = $1 AND revoked_at IS NULL")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Self-healing dev-seed upsert (Decision 7): inserts the well-known dev key, or —
    /// if a row with this `name` already exists — resets its `key`/`policy` and clears
    /// any stray `revoked_at`, so a revoke on a shared dev DB can't permanently poison
    /// the harness.
    pub async fn upsert_seed(&self, name: &str, key: &str, policy: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO apikeys.keys (name, key, policy) VALUES ($1, $2, $3) \
             ON CONFLICT (name) DO UPDATE \
               SET key = EXCLUDED.key, policy = EXCLUDED.policy, revoked_at = NULL",
        )
        .bind(name)
        .bind(key)
        .bind(policy)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

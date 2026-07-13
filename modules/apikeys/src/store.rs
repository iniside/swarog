//! The SQL layer for the `apikeys` schema — pure persistence, no capability/edge
//! knowledge. All reads and writes use the pool; keys are stored in plaintext (the
//! same trust model as `accounts.sessions.token`), so the admin page can display them
//! and lookup is a plain equality match. Hashing at rest is future hardening.

use apikeysapi::KeyRecord;
use sqlx::{PgConnection, PgPool};

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

    // --- Transactional writes (`*_tx`) --------------------------------------
    // The admin write API takes a caller-supplied connection so `apply_edit` can batch
    // a whole posted form into ONE transaction that commits (or rolls back) as a unit.

    /// Inserts a new key over a caller's connection. A duplicate `name` (primary key) or
    /// `key` (unique) surfaces as the underlying sqlx error for the caller to map. A
    /// `_`-prefixed `name` is rejected here too (not just in the admin `apply_edit`
    /// guard) so every insertion path — admin or otherwise — stays safe: the admin
    /// form's per-key policy fields use the key's own `name` as the field name, and a
    /// `_`-prefixed one would collide with the form's `_new_*`/`_revoke_name` control
    /// fields.
    pub async fn insert_tx(
        &self,
        conn: &mut PgConnection,
        name: &str,
        key: &str,
        policy: &str,
    ) -> Result<(), sqlx::Error> {
        if name.starts_with('_') {
            return Err(sqlx::Error::Configuration(
                format!("apikeys: key name {name:?} must not start with '_' (reserved for admin form control fields)")
                    .into(),
            ));
        }
        // Byte length, matching the gateway's `RealKeyVerifier::lookup` check
        // (`modules/gateway/src/keys.rs`) — both sides import the single contract
        // constant `apikeysapi::MAX_KEY_BYTES` so a key can never be created longer
        // than the gateway will ever accept it (the DDL CHECK on `apikeys.keys.key`
        // is the belt-and-suspenders twin of this guard).
        if key.len() > apikeysapi::MAX_KEY_BYTES {
            return Err(sqlx::Error::Configuration(
                format!(
                    "apikeys: key for {name:?} is {} bytes, exceeding apikeysapi::MAX_KEY_BYTES \
                     ({} bytes) — a key longer than this limit would always be rejected by the \
                     gateway's key verifier",
                    key.len(),
                    apikeysapi::MAX_KEY_BYTES,
                )
                .into(),
            ));
        }
        sqlx::query("INSERT INTO apikeys.keys (name, key, policy) VALUES ($1, $2, $3)")
            .bind(name)
            .bind(key)
            .bind(policy)
            .execute(conn)
            .await?;
        Ok(())
    }

    pub async fn lock_admin_state_tx(
        conn: &mut PgConnection,
        name: &str,
    ) -> Result<Option<(String, bool)>, sqlx::Error> {
        sqlx::query_as::<_, (String, bool)>(
            r#"
            SELECT policy, (revoked_at IS NOT NULL) AS revoked
            FROM apikeys.keys
            WHERE name = $1
            FOR UPDATE
            "#,
        )
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
    }

    pub async fn update_admin_state_tx(
        conn: &mut PgConnection,
        name: &str,
        expected_policy: &str,
        expected_revoked: bool,
        policy: &str,
        revoked: bool,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE apikeys.keys
            SET policy = $1,
                revoked_at = CASE
                    WHEN $2 THEN COALESCE(revoked_at, now())
                    ELSE NULL
                END
            WHERE name = $3
              AND policy = $4
              AND (revoked_at IS NOT NULL) = $5
            "#,
        )
        .bind(policy)
        .bind(revoked)
        .bind(name)
        .bind(expected_policy)
        .bind(expected_revoked)
        .execute(&mut *conn)
        .await?;

        Ok(result.rows_affected())
    }

    // --- Pool-based convenience wrappers (test-only) ------------------------
    // Terse setup helpers for this crate's live-Postgres tests. Gated `#[cfg(test)]`
    // because an ungated pool-based method here would be dead code (`Store` is private).

    /// [`Store::insert_tx`] over a freshly acquired pooled connection.
    #[cfg(test)]
    pub async fn insert(&self, name: &str, key: &str, policy: &str) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.insert_tx(&mut conn, name, key, policy).await
    }

    /// Replaces a key's policy for test setup/assertions.
    #[cfg(test)]
    pub async fn set_policy(&self, name: &str, policy: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE apikeys.keys SET policy = $1 WHERE name = $2")
            .bind(policy)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Revokes a key for test setup/assertions.
    #[cfg(test)]
    pub async fn revoke(&self, name: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE apikeys.keys SET revoked_at = now() WHERE name = $1 AND revoked_at IS NULL",
        )
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
        // Same byte-length guard as `insert_tx` — this is a write path too (self-healing
        // upsert), so it must not become a back door around the shared
        // `apikeysapi::MAX_KEY_BYTES` contract.
        if key.len() > apikeysapi::MAX_KEY_BYTES {
            return Err(sqlx::Error::Configuration(
                format!(
                    "apikeys: seed key for {name:?} is {} bytes, exceeding apikeysapi::MAX_KEY_BYTES \
                     ({} bytes)",
                    key.len(),
                    apikeysapi::MAX_KEY_BYTES,
                )
                .into(),
            ));
        }
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

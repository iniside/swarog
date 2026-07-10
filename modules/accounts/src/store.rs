//! The SQL layer for the `accounts` schema — pure persistence, no event/bus
//! knowledge (port of Go's `modules/accounts/store.go`). Write paths that must be
//! atomic with the `player.registered` outbox row take `&mut PgConnection` so the
//! caller (the service) owns the transaction; reads use the pool.

use base64::Engine as _;
use rand::RngCore as _;
use sqlx::{PgConnection, PgPool};

/// Session lifetime — Go's `sessionTTL = 30 * 24 * time.Hour`, applied in SQL.
pub(crate) const SESSION_TTL_DAYS: i32 = 30;

/// The product-scoped identity row (`accounts.players`). Module-private: the wire
/// types (`Session`/`MeView`) live in `accountsapi`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Player {
    pub id: String,
    pub display_name: String,
}

/// Typed store outcomes the service maps onto `opsapi::Status` (Go's ErrEmailTaken /
/// ErrInvalidCredentials / ErrIdentityLinked, as enums instead of sentinel errors).
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// A `(provider, subject)` unique violation on registration/linking.
    #[error("identity already registered")]
    Taken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// `true` for a Postgres unique violation (23505) — a duplicate email / an already
/// linked external identity (Go's `isUniqueViolation`).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// `true` for a Postgres "invalid text representation" (22P02) — a malformed uuid in
/// the request — treated as not-found rather than a 500.
fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// A fresh opaque bearer token: 32 random bytes, base64url without padding — Go's
/// `newToken` byte-for-byte (43 chars).
pub(crate) fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// A player plus the read-only bits the admin portal shows (Go's `PlayerRow`).
pub(crate) struct PlayerRow {
    pub id: String,
    pub display_name: String,
    pub providers: Vec<String>,
    /// Has a non-expired session.
    pub online: bool,
    /// Pre-rendered "Mon DD, HH24:MI" (Go formatted "Jan 2, 15:04" in code; here
    /// Postgres `to_char` renders the same shape).
    pub created_at: String,
}

pub(crate) struct Store {
    pub pool: PgPool,
}

impl Store {
    /// Creates a player and its first identity ON THE GIVEN CONNECTION (the caller's
    /// tx), so a failed identity insert rolls back the orphaned player AND the
    /// caller can ride its `player.registered` outbox row on the same tx. A
    /// `(provider, subject)` collision is [`StoreError::Taken`].
    pub async fn insert_player_with_identity_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
        display_name: &str,
        secret_hash: Option<&str>,
    ) -> Result<Player, StoreError> {
        let (id, display_name): (String, String) = sqlx::query_as(
            "INSERT INTO accounts.players (display_name) VALUES ($1) RETURNING id::text, display_name",
        )
        .bind(display_name)
        .fetch_one(&mut *conn)
        .await?;
        let res = sqlx::query(
            "INSERT INTO accounts.identities (provider, subject, player_id, secret_hash) \
             VALUES ($1, $2, $3::uuid, $4)",
        )
        .bind(provider)
        .bind(subject)
        .bind(&id)
        .bind(secret_hash)
        .execute(&mut *conn)
        .await;
        match res {
            Ok(_) => Ok(Player { id, display_name }),
            Err(e) if is_unique_violation(&e) => Err(StoreError::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// The player and stored hash for a dev identity, or `Ok(None)` when there is no
    /// such identity OR no stored hash — the same "invalid credentials" answer as a
    /// bad password, so the endpoint doesn't leak which emails exist.
    pub async fn password_identity(
        &self,
        email: &str,
    ) -> Result<Option<(Player, String)>, sqlx::Error> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name, i.secret_hash \
               FROM accounts.identities i \
               JOIN accounts.players p ON p.id = i.player_id \
              WHERE i.provider = 'dev' AND i.subject = $1",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some((id, display_name, Some(hash))) => Some((Player { id, display_name }, hash)),
            _ => None,
        })
    }

    /// The player an external identity maps to, or `Ok(None)` on a genuine miss.
    pub async fn player_by_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<Player>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name \
               FROM accounts.identities i \
               JOIN accounts.players p ON p.id = i.player_id \
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, display_name)| Player { id, display_name }))
    }

    /// Attaches an already-verified external identity to an existing player. A taken
    /// `(provider, subject)` is [`StoreError::Taken`] (Go's `ErrIdentityLinked`).
    pub async fn link_identity(
        &self,
        player_id: &str,
        provider: &str,
        subject: &str,
    ) -> Result<(), StoreError> {
        let res = sqlx::query(
            "INSERT INTO accounts.identities (provider, subject, player_id) VALUES ($1, $2, $3::uuid)",
        )
        .bind(provider)
        .bind(subject)
        .bind(player_id)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => Err(StoreError::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// Mints a fresh session for `player_id`: a 32-byte base64url token with the
    /// 30-day TTL applied in SQL.
    pub async fn new_session(&self, player_id: &str) -> Result<String, sqlx::Error> {
        let token = new_token();
        sqlx::query(
            "INSERT INTO accounts.sessions (token, player_id, expires_at) \
             VALUES ($1, $2::uuid, now() + make_interval(days => $3))",
        )
        .bind(&token)
        .bind(player_id)
        .bind(SESSION_TTL_DAYS)
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    /// Resolves a bearer token to its player, ignoring expired sessions. `Ok(None)`
    /// is a genuine unknown/expired token; an `Err` is a store failure the caller
    /// surfaces as infrastructure trouble (503), never a 401.
    pub async fn player_by_session(&self, token: &str) -> Result<Option<Player>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name \
               FROM accounts.sessions s \
               JOIN accounts.players p ON p.id = s.player_id \
              WHERE s.token = $1 AND s.expires_at > now()",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, display_name)| Player { id, display_name }))
    }

    /// Deletes every expired session ON THE GIVEN CONNECTION (the delivery tx), so the
    /// prune commits atomically with the durable subscription's checkpoint advance. The
    /// returned count is the number of rows removed. Idempotent — a redelivered tick
    /// simply deletes nothing the second time.
    pub async fn prune_expired_sessions(
        &self,
        conn: &mut PgConnection,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM accounts.sessions WHERE expires_at <= now()")
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }

    /// One player by id. A malformed id (22P02) is `Ok(None)`, like a genuine miss.
    pub async fn get_player(&self, id: &str) -> Result<Option<Player>, sqlx::Error> {
        let res: Result<Option<(String, String)>, sqlx::Error> = sqlx::query_as(
            "SELECT id::text, display_name FROM accounts.players WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(row.map(|(id, display_name)| Player { id, display_name })),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every credential mapping of a player, ordered for a stable `me` body.
    pub async fn identities_of(
        &self,
        player_id: &str,
    ) -> Result<Vec<accountsapi::IdentityRef>, sqlx::Error> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT provider, subject FROM accounts.identities \
              WHERE player_id = $1::uuid ORDER BY provider, subject",
        )
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(provider, subject)| accountsapi::IdentityRef { provider, subject })
            .collect())
    }

    /// The admin KPI triple: players, identities, non-expired sessions.
    pub async fn stats(&self) -> Result<(i64, i64, i64), sqlx::Error> {
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM accounts.players), \
                    (SELECT count(*) FROM accounts.identities), \
                    (SELECT count(*) FROM accounts.sessions WHERE expires_at > now())",
        )
        .fetch_one(&self.pool)
        .await
    }

    /// The newest `limit` players with their linked providers + online flag, for the
    /// admin table (Go's `listPlayers`).
    pub async fn list_players(&self, limit: i64) -> Result<Vec<PlayerRow>, sqlx::Error> {
        let rows: Vec<(String, String, String, String, bool)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name, to_char(p.created_at, 'Mon DD, HH24:MI'), \
                    coalesce(string_agg(DISTINCT i.provider, ','), '') AS providers, \
                    EXISTS(SELECT 1 FROM accounts.sessions s \
                            WHERE s.player_id = p.id AND s.expires_at > now()) AS online \
               FROM accounts.players p \
               LEFT JOIN accounts.identities i ON i.player_id = p.id \
              GROUP BY p.id, p.display_name, p.created_at \
              ORDER BY p.created_at DESC \
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, display_name, created_at, providers, online)| PlayerRow {
                id,
                display_name,
                providers: if providers.is_empty() {
                    Vec::new()
                } else {
                    providers.split(',').map(str::to_string).collect()
                },
                online,
                created_at,
            })
            .collect())
    }
}

//! The SQL layer for the `accounts` schema — pure persistence, no event/bus
//! knowledge (port of Go's `modules/accounts/store.go`). Write paths that must be
//! atomic with the `player.registered` durable event append take `&mut PgConnection` so the
//! caller (the service) owns the transaction; reads use the pool.

use base64::Engine as _;
use rand::RngCore as _;
use sqlx::{PgConnection, PgPool};

/// Access-session lifetime, applied in SQL. Short on purpose: a leaked bearer dies
/// within the hour and the client renews through its rotating refresh token.
pub(crate) const ACCESS_TTL_MINUTES: i32 = 60;

/// The HARD life of a refresh family, applied in SQL when a family is born. A rotation
/// never extends it — see [`Store::insert_refresh_successor_tx`].
pub(crate) const REFRESH_TTL_DAYS: i32 = 30;

/// How long after its rotation a consumed refresh token still answers with the
/// successor it recorded instead of being treated as theft. The failure this covers is
/// a client that rotated and lost the RESPONSE (mobile handoff, a 429 from the
/// gateway's rate limiter), not two racing dials. `f64` because Postgres'
/// `make_interval(secs => …)` takes double precision.
pub(crate) const REFRESH_GRACE_SECONDS: f64 = 30.0;

/// The lifetime the API reports for a freshly minted access token, derived from the
/// SAME constant the INSERT applies so the number a client schedules against cannot
/// drift from the number the row expires by.
pub(crate) fn access_expires_in_secs() -> i64 {
    i64::from(ACCESS_TTL_MINUTES) * 60
}

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

/// Domain-separated stable FNV-1a key for serializing every writer of one external
/// `(provider, subject)` identity. Hash collisions only add serialization.
pub(crate) fn identity_lock_key(provider: &str, subject: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in b"accounts.identity.v1\0"
        .iter()
        .chain(provider.as_bytes())
        .chain([0].iter())
        .chain(subject.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as i64
}

/// Domain-separated stable FNV-1a key for serializing every writer of one PLAYER's
/// identity set. Distinct from [`identity_lock_key`] on purpose: that one serializes
/// writers of a single `(provider, subject)`, which two links of DIFFERENT providers
/// to the same player never share — only this key makes them mutually exclusive.
pub(crate) fn player_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in b"accounts.player.v1\0".iter().chain(player_id.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as i64
}

/// What a link attempt did to `accounts.identities`, so the caller decides whether
/// anything happened without asking the database a second time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkOutcome {
    /// The identity row was inserted by this call.
    Linked,
    /// The identity already belonged to this player; nothing was written.
    AlreadyLinked,
}

/// A fresh opaque bearer token: 32 random bytes, base64url without padding — Go's
/// `newToken` byte-for-byte (43 chars).
pub(crate) fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The stored facts about one presented refresh token, read after a rotation attempt
/// declined it. Booleans are evaluated by Postgres against the SAME `now()` the failed
/// UPDATE used, so the classification cannot disagree with the predicate that produced
/// it.
pub(crate) struct RefreshRow {
    pub player_id: String,
    pub family_id: String,
    /// The successor recorded when this token was consumed.
    pub replaced_by: Option<String>,
    pub used: bool,
    pub used_within_grace: bool,
    pub expired: bool,
}

/// What a presented refresh token earns. The ONE decision every refresh outcome comes
/// from — deliberately pure, so the theft branch is executable without a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefreshVerdict {
    /// The token was live and unused: it is now consumed in favour of `successor`.
    Rotate { player_id: String, family_id: String },
    /// A consumed token presented within the grace window — the lost-response case.
    /// The recorded successor is handed back; nothing is revoked.
    GraceReplay {
        player_id: String,
        family_id: String,
        successor: String,
    },
    /// A consumed token presented too late to be a lost response: a stolen credential.
    /// The family dies and the caller still gets the plain 401.
    Revoke { family_id: String },
    /// Unknown or expired — 401, and nothing to revoke.
    Deny,
}

/// Classifies one presentation from the rotation attempt's outcome and the row behind
/// it. `rotated` is [`Store::rotate_refresh_tx`]'s return, `row` is
/// [`Store::refresh_row_tx`]'s, read on the same transaction.
///
/// Every ambiguous state falls through to [`RefreshVerdict::Deny`]: a row that is
/// neither used nor expired yet refused to rotate, and a consumed row with no recorded
/// successor, are both states a single-transaction rotation cannot produce, so the
/// answer is the fail-closed one rather than a guess that hands out a session.
pub(crate) fn classify_presentation(
    rotated: Option<(String, String)>,
    row: Option<RefreshRow>,
) -> RefreshVerdict {
    if let Some((player_id, family_id)) = rotated {
        return RefreshVerdict::Rotate {
            player_id,
            family_id,
        };
    }
    let Some(row) = row else {
        return RefreshVerdict::Deny;
    };
    if row.expired || !row.used {
        return RefreshVerdict::Deny;
    }
    match row.replaced_by {
        Some(successor) if row.used_within_grace => RefreshVerdict::GraceReplay {
            player_id: row.player_id,
            family_id: row.family_id,
            successor,
        },
        Some(_) => RefreshVerdict::Revoke {
            family_id: row.family_id,
        },
        None => RefreshVerdict::Deny,
    }
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
    /// caller can ride its `player.registered` durable event append on the same tx. A
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
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(crate::providers::DEV)
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some((id, display_name, Some(hash))) => Some((Player { id, display_name }, hash)),
            _ => None,
        })
    }

    /// Whether `subject` names a guest identity whose stored digest is exactly
    /// `secret_hash`. Subject and digest are matched in ONE predicate on purpose: an
    /// unknown subject and a wrong secret must be the same answer, and a lookup that
    /// returned the row first would hand its caller the oracle. A SIBLING of
    /// [`Store::password_identity`] rather than a widening of it — that one resolves a
    /// dev/password identity by EMAIL, and a guest subject must never match it.
    pub async fn guest_identity_matches(
        &self,
        subject: &str,
        secret_hash: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.identities \
              WHERE provider = $1 AND subject = $2 AND secret_hash = $3",
        )
        .bind(crate::providers::GUEST)
        .bind(subject)
        .bind(secret_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Takes the transaction-scoped writer lock for one external identity. Callers
    /// make this the first statement after BEGIN, then re-read under the lock.
    pub async fn lock_identity_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(identity_lock_key(provider, subject))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// The player an external identity maps to on the caller's locked transaction.
    pub async fn player_by_identity_tx(
        &self,
        conn: &mut PgConnection,
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
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id, display_name)| Player { id, display_name }))
    }

    /// Takes the transaction-scoped writer lock for one PLAYER's identity set. A
    /// caller that takes this AND [`Store::lock_identity_tx`] takes this one FIRST.
    pub async fn lock_player_tx(
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

    /// Whether `player_id` already holds an identity from a provider other than
    /// `guest`, on the caller's locked transaction. Read BEFORE the link insert:
    /// after it, the answer would include the row being written.
    pub async fn has_real_identity_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.identities \
              WHERE player_id = $1::uuid AND provider <> $2 LIMIT 1",
        )
        .bind(player_id)
        .bind(crate::providers::GUEST)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.is_some())
    }

    /// Attaches an already-verified external identity ON THE CALLER'S transaction,
    /// under the locks the caller took. Same-owner re-link writes nothing
    /// ([`LinkOutcome::AlreadyLinked`]); another owner is [`StoreError::Taken`].
    pub async fn link_identity_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        provider: &str,
        subject: &str,
    ) -> Result<LinkOutcome, StoreError> {
        if let Some(owner) = self
            .player_by_identity_tx(&mut *conn, provider, subject)
            .await?
        {
            if owner.id == player_id {
                return Ok(LinkOutcome::AlreadyLinked);
            }
            return Err(StoreError::Taken);
        }
        let res = sqlx::query(
            "INSERT INTO accounts.identities (provider, subject, player_id) VALUES ($1, $2, $3::uuid)",
        )
        .bind(provider)
        .bind(subject)
        .bind(player_id)
        .execute(&mut *conn)
        .await;
        match res {
            Ok(_) => Ok(LinkOutcome::Linked),
            Err(e) if is_unique_violation(&e) => Err(StoreError::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// Inserts a caller-provided access token ON THE GIVEN CONNECTION, so session
    /// issuance can commit atomically with registration or stand alone in a thin tx.
    /// `family_id` ties the session to the refresh family it was minted under: it is
    /// what makes revocation scopeable to one compromised login instead of every
    /// device the player owns.
    pub async fn insert_session_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        token: &str,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO accounts.sessions (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, $3::uuid, now() + make_interval(mins => $4))",
        )
        .bind(token)
        .bind(player_id)
        .bind(family_id)
        .bind(ACCESS_TTL_MINUTES)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Starts a NEW refresh family on the caller's tx: the row Postgres mints the
    /// `family_id` for, and the only place a 30-day expiry is ever written.
    pub async fn insert_refresh_family_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        player_id: &str,
    ) -> Result<String, sqlx::Error> {
        let (family_id,): (String,) = sqlx::query_as(
            "INSERT INTO accounts.refresh_tokens (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, gen_random_uuid(), now() + make_interval(days => $3)) \
             RETURNING family_id::text",
        )
        .bind(token)
        .bind(player_id)
        .bind(REFRESH_TTL_DAYS)
        .fetch_one(&mut *conn)
        .await?;
        Ok(family_id)
    }

    /// Consumes `token` and records `successor` as its replacement, returning the
    /// `(player_id, family_id)` it belonged to. Zero rows — an absent, expired or
    /// ALREADY CONSUMED token — is `Ok(None)`, which the caller resolves by re-reading
    /// the row in this same transaction ([`Store::refresh_row_tx`]).
    ///
    /// The predicate is the whole concurrency story: two dials with the same token
    /// serialize on this row, and only the one that finds `used_at IS NULL` rotates.
    pub async fn rotate_refresh_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        successor: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as(
            "UPDATE accounts.refresh_tokens \
                SET used_at = now(), replaced_by = $2 \
              WHERE token = $1 AND used_at IS NULL AND expires_at > now() \
             RETURNING player_id::text, family_id::text",
        )
        .bind(token)
        .bind(successor)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Inserts the successor of an already-consumed `parent` on the caller's tx. The
    /// player, the family AND the expiry are copied from the parent row rather than
    /// passed in, so a rotation cannot slide the family's hard 30-day bound forward —
    /// a family that renewed its own expiry would never expire at all. A vanished
    /// parent inserts nothing and surfaces as `RowNotFound`.
    pub async fn insert_refresh_successor_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        parent: &str,
    ) -> Result<(), sqlx::Error> {
        let _: (String,) = sqlx::query_as(
            "INSERT INTO accounts.refresh_tokens (token, player_id, family_id, expires_at) \
             SELECT $1, player_id, family_id, expires_at \
               FROM accounts.refresh_tokens WHERE token = $2 \
             RETURNING token",
        )
        .bind(token)
        .bind(parent)
        .fetch_one(&mut *conn)
        .await?;
        Ok(())
    }

    /// The facts a failed rotation is classified from, read on the SAME transaction as
    /// the failed UPDATE so no other writer can move the row in between.
    pub async fn refresh_row_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
    ) -> Result<Option<RefreshRow>, sqlx::Error> {
        let row: Option<(String, String, Option<String>, bool, bool, bool)> = sqlx::query_as(
            "SELECT player_id::text, family_id::text, replaced_by, \
                    used_at IS NOT NULL, \
                    coalesce(used_at > now() - make_interval(secs => $2), false), \
                    expires_at <= now() \
               FROM accounts.refresh_tokens WHERE token = $1",
        )
        .bind(token)
        .bind(REFRESH_GRACE_SECONDS)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(
            |(player_id, family_id, replaced_by, used, used_within_grace, expired)| RefreshRow {
                player_id,
                family_id,
                replaced_by,
                used,
                used_within_grace,
                expired,
            },
        ))
    }

    /// Revokes one refresh FAMILY on the caller's tx: every access session and every
    /// refresh token descended from that login, and nothing belonging to the player's
    /// other logins. Deletion, not a flag: with the rows gone every token of the family
    /// answers as unknown, so a replay can never resurrect the family through the grace
    /// window.
    pub async fn kill_family_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM accounts.refresh_tokens WHERE family_id = $1::uuid")
            .bind(family_id)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM accounts.sessions WHERE family_id = $1::uuid")
            .bind(family_id)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Resolves a bearer token to its player, ignoring expired sessions. `Ok(None)`
    /// is a genuine unknown/expired token; an `Err` is a store failure the caller
    /// surfaces as infrastructure trouble (503), never a 401.
    pub async fn player_by_session(&self, token: &str) -> Result<Option<Player>, sqlx::Error> {
        if !crate::session_token_within_cap(token) {
            return Ok(None);
        }
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

    /// Deletes every EXPIRED refresh token on the delivery tx, the sibling retention of
    /// [`Store::prune_expired_sessions`] for the table rotation grows. Only
    /// `expires_at <= now()` — a CONSUMED row is the reuse detector and is retained for
    /// its family's whole life; an expired one can no longer produce any verdict but
    /// `Deny`, so removing it changes no answer.
    pub async fn prune_expired_refresh_tokens(
        &self,
        conn: &mut PgConnection,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM accounts.refresh_tokens WHERE expires_at <= now()")
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

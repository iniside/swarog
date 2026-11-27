//! `accounts.oidc_codes`: the authorization codes `/accounts/oidc/authorize` issues and
//! `/accounts/oidc/token` redeems.

use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use crate::store::{is_foreign_key_violation, Revocation, Store};

/// How long an issued code can be redeemed.
pub(crate) const CODE_TTL_SECS: i32 = 60;
/// How long a code row is kept after its issue, redeemed or not, so a replay still finds it.
pub(crate) const REPLAY_WINDOW_SECS: i32 = 600;

/// What `/authorize` binds a code to besides the signed-in player.
pub(crate) struct NewCode<'a> {
    pub(crate) client_id: &'a str,
    pub(crate) redirect_uri: &'a str,
    pub(crate) code_challenge: &'a str,
    /// The granted scopes, space-separated in `scopes::ALL` order; empty when none.
    pub(crate) scope: &'a str,
    pub(crate) nonce: Option<&'a str>,
}

/// A code a redemption consumed, with the id of the token family it mints.
pub(crate) struct RedeemedCode {
    pub(crate) client_id: String,
    pub(crate) player_id: String,
    pub(crate) redirect_uri: String,
    pub(crate) code_challenge: String,
    pub(crate) scope: String,
    pub(crate) nonce: Option<String>,
    /// Unix seconds of the browser sign-in the code was issued under.
    pub(crate) auth_time: i64,
    pub(crate) family_id: String,
}

fn code_digest(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// Domain-separated stable FNV-1a key serializing a code's mint with the revocation of its
/// replay. Hash collisions only add serialization.
fn code_family_lock_key(family_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in b"accounts.code-family.v1\0".iter().chain(family_id.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as i64
}

impl Store {
    /// Deletes every code issued [`REPLAY_WINDOW_SECS`] or more ago, answering how many.
    pub(crate) async fn prune_oidc_codes<'e, E: sqlx::PgExecutor<'e>>(&self, db: E) -> sqlx::Result<u64> {
        // SKIP LOCKED: the prune never waits on a row a player's code delete holds, so the
        // two cannot deadlock over rows they take in different orders.
        let res = sqlx::query(
            "DELETE FROM accounts.oidc_codes WHERE code_digest IN ( \
               SELECT code_digest FROM accounts.oidc_codes \
                WHERE created_at <= now() - make_interval(secs => $1) \
                  FOR UPDATE SKIP LOCKED)",
        )
        .bind(f64::from(REPLAY_WINDOW_SECS))
        .execute(db)
        .await?;
        Ok(res.rows_affected())
    }

    /// Issues `code` under the live browser session whose token hashes to `session_digest`:
    /// to that session's player, with its sign-in time as `auth_time`, bound to the session
    /// by a foreign key that a session delete sets NULL. `false` when that session is gone,
    /// or its player, the client or the session itself was deleted before the insert. The
    /// insert reads the session without a lock; its foreign-key check on the session row is
    /// what refuses a session a racing reset, Revoke all or logout deleted, and what makes
    /// that delete wait for, then unbind, a code inserted first.
    pub(crate) async fn insert_oidc_code(
        &self,
        code: &str,
        session_digest: &[u8],
        new: &NewCode<'_>,
    ) -> sqlx::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO accounts.oidc_codes \
               (code_digest, client_id, player_id, session_digest, redirect_uri, code_challenge, \
                scope, nonce, auth_time) \
             SELECT $1, $2, s.player_id, s.token_digest, $3, $4, $5, $6, s.created_at \
               FROM accounts.browser_sessions s \
              WHERE s.token_digest = $7 AND s.expires_at > now()",
        )
        .bind(code_digest(code))
        .bind(new.client_id)
        .bind(new.redirect_uri)
        .bind(new.code_challenge)
        .bind(new.scope)
        .bind(new.nonce)
        .bind(session_digest)
        .execute(&self.pool)
        .await;
        match res {
            Ok(done) => Ok(done.rows_affected() == 1),
            Err(e) if is_foreign_key_violation(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Consumes `code` when it is unredeemed, was issued less than [`CODE_TTL_SECS`] ago and
    /// its browser session is still live, stamping a fresh family id on its row; any other
    /// code is `None`. The caller commits it before the mint, so this row lock is never held
    /// across the player lock.
    ///
    /// A session delete sets the row's `session_digest` NULL instead of deleting it, so an
    /// unredeemed code of an ended session answers like an unknown code, while a redeemed row
    /// stays, `redeemed_at` set, for replay detection whatever its `session_digest`. A
    /// redemption racing the session delete either commits first, or waits on the row lock
    /// and then finds `session_digest` NULL.
    pub(crate) async fn redeem_oidc_code<'e, E: sqlx::PgExecutor<'e>>(
        &self,
        db: E,
        code: &str,
    ) -> sqlx::Result<Option<RedeemedCode>> {
        if !crate::opaque_token_within_cap(code) {
            return Ok(None);
        }
        type Row = (String, String, String, String, String, Option<String>, i64, String);
        let row: Option<Row> = sqlx::query_as(
            "UPDATE accounts.oidc_codes SET redeemed_at = now(), family_id = gen_random_uuid() \
              WHERE code_digest = $1 AND redeemed_at IS NULL AND session_digest IS NOT NULL \
                AND created_at > now() - make_interval(secs => $2) \
             RETURNING client_id, player_id::text, redirect_uri, code_challenge, scope, nonce, \
                       floor(EXTRACT(EPOCH FROM auth_time))::int8, family_id::text",
        )
        .bind(code_digest(code))
        .bind(f64::from(CODE_TTL_SECS))
        .fetch_optional(db)
        .await?;
        Ok(row.map(
            |(client_id, player_id, redirect_uri, code_challenge, scope, nonce, auth_time, family_id)| RedeemedCode {
                client_id,
                player_id,
                redirect_uri,
                code_challenge,
                scope,
                nonce,
                auth_time,
                family_id,
            },
        ))
    }

    /// The family id the first redemption of `code` stamped, while its row is kept for replay
    /// detection ([`REPLAY_WINDOW_SECS`]); `None` for a code never redeemed or past that window.
    pub(crate) async fn replayed_code_family<'e, E: sqlx::PgExecutor<'e>>(
        &self,
        db: E,
        code: &str,
    ) -> sqlx::Result<Option<String>> {
        if !crate::opaque_token_within_cap(code) {
            return Ok(None);
        }
        sqlx::query_scalar(
            "SELECT family_id::text FROM accounts.oidc_codes \
              WHERE code_digest = $1 AND redeemed_at IS NOT NULL \
                AND created_at > now() - make_interval(secs => $2)",
        )
        .bind(code_digest(code))
        .bind(f64::from(REPLAY_WINDOW_SECS))
        .fetch_optional(db)
        .await
    }

    /// Takes the advisory lock on a code's family id, the FIRST lock of both the code's mint
    /// and the revocation of its replay, so the two serialize.
    pub(crate) async fn lock_code_family_tx(&self, conn: &mut PgConnection, family_id: &str) -> sqlx::Result<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(code_family_lock_key(family_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Revokes the tokens a replayed code's first redemption minted, on the caller's
    /// transaction: under [`Store::lock_code_family_tx`], a family revocation, which a mint
    /// still waiting on that lock then refuses to mint under, and the family's delete when
    /// it was already minted.
    pub(crate) async fn revoke_replayed_family_tx(&self, conn: &mut PgConnection, family_id: &str) -> sqlx::Result<()> {
        self.lock_code_family_tx(conn, family_id).await?;
        self.delete_family_tx(conn, family_id).await?;
        self.insert_revocation_tx(conn, Revocation::Family(family_id)).await
    }

    /// Whether a family revocation for `family_id` exists, on the caller's transaction.
    pub(crate) async fn family_revoked_tx(&self, conn: &mut PgConnection, family_id: &str) -> sqlx::Result<bool> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts.token_revocations WHERE family_id = $1::uuid)")
            .bind(family_id)
            .fetch_one(&mut *conn)
            .await
    }

    /// Whether `player_id` exists, taking its row FOR KEY SHARE on the caller's transaction,
    /// the lock the family insert's foreign key takes, so a code's mint answers a player
    /// deleted after its redemption instead of failing that insert.
    pub(crate) async fn key_share_player_tx(&self, conn: &mut PgConnection, player_id: &str) -> sqlx::Result<bool> {
        let row: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM accounts.players WHERE id = $1::uuid FOR KEY SHARE")
            .bind(player_id)
            .fetch_optional(&mut *conn)
            .await?;
        Ok(row.is_some())
    }

    /// Whether the row of the code redeemed under `family_id` is still kept, on the caller's
    /// transaction. A reset or Revoke all deletes the player's codes under the player row lock,
    /// so read after [`Store::key_share_player_tx`] it answers `false` for a code whose player
    /// was reset before the mint, and no later reset can commit before the mint does.
    pub(crate) async fn redeemed_code_kept_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        family_id: &str,
    ) -> sqlx::Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM accounts.oidc_codes \
                            WHERE player_id = $1::uuid AND family_id = $2::uuid)",
        )
        .bind(player_id)
        .bind(family_id)
        .fetch_one(&mut *conn)
        .await
    }

    /// Deletes every code of a player on the caller's transaction, answering how many.
    pub(crate) async fn delete_player_oidc_codes_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> sqlx::Result<u64> {
        let res = sqlx::query("DELETE FROM accounts.oidc_codes WHERE player_id = $1::uuid")
            .bind(player_id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }
}

//! `accounts.oidc_codes`: the authorization codes `/accounts/oidc/authorize` issues and
//! `/accounts/oidc/token` redeems.

use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use crate::store::{is_foreign_key_violation, Store};

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
#[cfg_attr(not(test), expect(dead_code, reason = "the /token endpoint is its only redeemer"))]
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
    /// to that session's player, with its sign-in time as `auth_time`. `false` when that
    /// session is gone, or its player or the client was deleted before the insert.
    pub(crate) async fn insert_oidc_code(
        &self,
        code: &str,
        session_digest: &[u8],
        new: &NewCode<'_>,
    ) -> sqlx::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO accounts.oidc_codes \
               (code_digest, client_id, player_id, redirect_uri, code_challenge, scope, nonce, auth_time) \
             SELECT $1, $2, s.player_id, $3, $4, $5, $6, s.created_at \
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

    /// Consumes `code` when it is unredeemed and was issued less than [`CODE_TTL_SECS`] ago,
    /// stamping a fresh family id on its row; any other code is `None`. The caller commits
    /// it before the mint, so this row lock is never held across the player lock.
    #[cfg_attr(not(test), expect(dead_code, reason = "the /token endpoint is its only redeemer"))]
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
              WHERE code_digest = $1 AND redeemed_at IS NULL \
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

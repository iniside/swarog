//! Browser sessions of the hosted login pages. The cookie carries an opaque token;
//! `accounts.browser_sessions` keys on its SHA-256 digest and binds the CSRF token the
//! page's forms echo. The bearer sessions `Store::player_by_session` reads are a
//! separate table, so a browser cookie never authenticates an operation.

use sha2::{Digest, Sha256};

use crate::store::{is_canonical_uuid, is_foreign_key_violation, is_invalid_uuid, new_token, Store};

/// How long a signed-in browser stays signed in.
pub(crate) const BROWSER_SESSION_TTL_HOURS: i32 = 12;

/// A freshly minted session: the cookie value and the CSRF token, both shown only here.
pub(crate) struct NewBrowserSession {
    pub token: String,
    pub csrf_token: String,
}

/// A live session as the operator's Sessions panel shows it: never its token or digest.
pub(crate) struct BrowserSessionRow {
    /// `YYYY-MM-DD HH:MI` UTC, the refresh families' spelling.
    pub started: String,
    pub expires_in_secs: i64,
}

/// A live session read back by its cookie token.
pub(crate) struct BrowserSession {
    pub player_id: String,
    pub csrf_token: String,
}

pub(crate) fn digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

impl Store {
    /// Mints a browser session for `player_id`. `None` when there is no such player,
    /// including one deleted while this insert waited on its row: the foreign key check
    /// then fails instead of leaving a session behind the deletion.
    pub async fn create_browser_session(
        &self,
        player_id: &str,
    ) -> Result<Option<NewBrowserSession>, sqlx::Error> {
        let session = NewBrowserSession {
            token: new_token(),
            csrf_token: new_token(),
        };
        let res = sqlx::query(
            "INSERT INTO accounts.browser_sessions (token_digest, player_id, csrf_token, expires_at) \
             SELECT $1, p.id, $3, now() + make_interval(hours => $4) \
               FROM accounts.players p WHERE p.id = $2::uuid",
        )
        .bind(digest(&session.token))
        .bind(player_id)
        .bind(&session.csrf_token)
        .bind(BROWSER_SESSION_TTL_HOURS)
        .execute(&self.pool)
        .await;
        match res {
            Ok(done) if done.rows_affected() == 1 => Ok(Some(session)),
            Ok(_) => Ok(None),
            Err(e) if is_foreign_key_violation(&e) || is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The live session `token` names; an expired, unknown or over-cap token is `None`.
    pub async fn browser_session(
        &self,
        token: &str,
    ) -> Result<Option<BrowserSession>, sqlx::Error> {
        if !crate::session_token_within_cap(token) {
            return Ok(None);
        }
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT player_id::text, csrf_token FROM accounts.browser_sessions \
              WHERE token_digest = $1 AND expires_at > now()",
        )
        .bind(digest(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(player_id, csrf_token)| BrowserSession { player_id, csrf_token }))
    }

    /// The newest `limit` live browser sessions of a player, plus how many it holds.
    pub async fn live_browser_session_rows(
        &self,
        player_id: &str,
        limit: i64,
    ) -> Result<(Vec<BrowserSessionRow>, i64), sqlx::Error> {
        if !is_canonical_uuid(player_id) {
            return Ok((Vec::new(), 0));
        }
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI'), \
                    floor(EXTRACT(EPOCH FROM (expires_at - now())))::int8, \
                    count(*) OVER () \
               FROM accounts.browser_sessions \
              WHERE player_id = $1::uuid AND expires_at > now() \
              ORDER BY created_at DESC, token_digest LIMIT $2",
        )
        .bind(player_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let live = rows.first().map_or(0, |row| row.2);
        let sessions = rows
            .into_iter()
            .map(|(started, expires_in_secs, _)| BrowserSessionRow { started, expires_in_secs })
            .collect();
        Ok((sessions, live))
    }

    /// Ends the session `token` names, answering whether there was one.
    pub async fn delete_browser_session(&self, token: &str) -> Result<bool, sqlx::Error> {
        if !crate::session_token_within_cap(token) {
            return Ok(false);
        }
        self.delete_browser_session_digest(&digest(token)).await
    }

    /// Ends the session whose token hashes to `token_digest`, answering whether there was one.
    pub async fn delete_browser_session_digest(&self, token_digest: &[u8]) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("DELETE FROM accounts.browser_sessions WHERE token_digest = $1")
            .bind(token_digest)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

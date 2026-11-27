//! `accounts.device_codes`: the device authorization grant (RFC 8628). A device asks for a code
//! pair, the player approves or denies the short `user_code` in the browser, and the device polls
//! `/accounts/oidc/token` with the long `device_code` until the grant settles.

use std::net::IpAddr;

use rand::Rng as _;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::codes::REPLAY_WINDOW_SECS;
use crate::store::{is_foreign_key_violation, is_unique_violation, Store};

/// The `user_code` alphabet: consonants without vowels or lookalikes (RFC 8628 §6.1).
pub(crate) const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
/// Characters in a `user_code`, shown as two groups of four.
pub(crate) const USER_CODE_LEN: usize = 8;
/// The polling interval a new code starts with, in seconds.
pub(crate) const POLL_INTERVAL_SECS: i32 = 5;
/// What a poll sooner than the interval adds to it, in seconds (RFC 8628 §3.5 `slow_down`).
pub(crate) const SLOW_DOWN_STEP_SECS: i32 = 5;
/// The longest stored User-Agent, in bytes; a longer one is cut at a character boundary.
pub(crate) const MAX_USER_AGENT_BYTES: usize = 256;
/// The longest stored requesting IP in text form, in bytes (an IPv6 address is at most 45).
pub(crate) const MAX_REQUESTED_IP_BYTES: usize = 64;
/// Inserts tried before a run of digest collisions is answered as an error.
const MAX_INSERT_TRIES: usize = 5;
/// The longest raw `user_code` input that is normalised at all.
const MAX_USER_CODE_INPUT_BYTES: usize = 64;

/// What `/device_authorization` binds a new code pair to.
pub(crate) struct NewDeviceCode<'a> {
    pub(crate) client_id: &'a str,
    /// The granted scopes, space-separated in `scopes::ALL` order; empty when none.
    pub(crate) scope: &'a str,
    pub(crate) requested_ip: IpAddr,
    pub(crate) user_agent: Option<&'a str>,
}

/// A freshly issued code pair.
pub(crate) struct IssuedDeviceCode {
    pub(crate) device_code: String,
    /// Formatted `XXXX-XXXX`.
    pub(crate) user_code: String,
    /// Seconds until the pair expires: the client's `device_code_ttl_secs`.
    pub(crate) expires_in: i64,
}

/// A pending, unexpired code as the approval page shows it.
pub(crate) struct PendingDevice {
    pub(crate) client_id: String,
    pub(crate) scope: String,
    pub(crate) requested_ip: String,
    pub(crate) user_agent: Option<String>,
    /// Unix seconds of the code's issue.
    pub(crate) created_at: i64,
    /// Unix seconds of the code's expiry.
    pub(crate) expires_at: i64,
}

/// An approved code a poll redeemed, with the id of the token family it mints.
pub(crate) struct RedeemedDevice {
    pub(crate) client_id: String,
    pub(crate) player_id: String,
    pub(crate) scope: String,
    /// Unix seconds of the browser sign-in the code was approved under.
    pub(crate) auth_time: i64,
    pub(crate) family_id: String,
}

/// The answer to one poll, in the order its checks run.
pub(crate) enum DevicePoll {
    /// An unknown `device_code`, or another client's: `invalid_grant`.
    Unknown,
    /// A `device_code` redeemed before: the first redemption's family is revoked; `invalid_grant`.
    Replayed { family_id: String },
    /// Past its expiry, or approved under a browser session that has since ended: `expired_token`.
    Expired,
    /// Polled sooner than its interval, which grew by [`SLOW_DOWN_STEP_SECS`]; also the answer to
    /// a poll that lost a race to another poll, a redemption or the expiry, so the client's next
    /// poll reads the settled row: `slow_down`.
    SlowDown,
    /// Not approved or denied yet: `authorization_pending`.
    Pending,
    /// Denied by the player: `access_denied`.
    Denied,
    /// Approved and now redeemed; the caller mints under its family id.
    Redeemed(RedeemedDevice),
}

fn digest(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// A random `user_code`, unformatted: [`USER_CODE_LEN`] characters of [`USER_CODE_ALPHABET`].
fn new_user_code() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..USER_CODE_LEN)
        .map(|_| char::from(USER_CODE_ALPHABET[rng.gen_range(0..USER_CODE_ALPHABET.len())]))
        .collect()
}

/// The stored form of a typed `user_code`: upper-cased, without `-` and spaces; `None` when
/// the rest is not [`USER_CODE_LEN`] characters of [`USER_CODE_ALPHABET`].
pub(crate) fn normalize_user_code(input: &str) -> Option<String> {
    if input.len() > MAX_USER_CODE_INPUT_BYTES {
        return None;
    }
    let code: String = input.chars().filter(|c| *c != '-' && *c != ' ').map(|c| c.to_ascii_uppercase()).collect();
    (code.len() == USER_CODE_LEN && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then_some(code)
}

/// A normalised `user_code` as shown: `XXXX-XXXX`.
pub(crate) fn format_user_code(code: &str) -> String {
    let (head, tail) = code.split_at(code.len() / 2);
    format!("{head}-{tail}")
}

/// `text` cut to at most `max` bytes at a character boundary.
fn bounded(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Store {
    /// Issues a code pair to `new.client_id`, expiring after that client's
    /// `device_code_ttl_secs`; `None` when the client does not exist. A digest collision
    /// retries with a fresh pair, up to [`MAX_INSERT_TRIES`] times.
    pub(crate) async fn insert_device_code(&self, new: &NewDeviceCode<'_>) -> sqlx::Result<Option<IssuedDeviceCode>> {
        let requested_ip = new.requested_ip.to_string();
        let user_agent = new.user_agent.map(|ua| bounded(ua, MAX_USER_AGENT_BYTES));
        let mut tries = 0;
        loop {
            tries += 1;
            let device_code = crate::store::new_token();
            let user_code = new_user_code();
            let res: sqlx::Result<Option<i64>> = sqlx::query_scalar(
                "INSERT INTO accounts.device_codes \
                   (device_code_digest, user_code_digest, client_id, scope, interval_secs, expires_at, \
                    requested_ip, user_agent) \
                 SELECT $1, $2, c.client_id, $4, $5, now() + make_interval(secs => c.device_code_ttl_secs), \
                        $6, $7 \
                   FROM accounts.oidc_clients c \
                  WHERE c.client_id = $3 \
                 RETURNING floor(EXTRACT(EPOCH FROM expires_at - created_at))::int8",
            )
            .bind(digest(&device_code))
            .bind(digest(&user_code))
            .bind(new.client_id)
            .bind(new.scope)
            .bind(POLL_INTERVAL_SECS)
            .bind(&requested_ip)
            .bind(user_agent)
            .fetch_optional(&self.pool)
            .await;
            match res {
                Ok(Some(expires_in)) => {
                    return Ok(Some(IssuedDeviceCode {
                        device_code,
                        user_code: format_user_code(&user_code),
                        expires_in,
                    }))
                }
                Ok(None) => return Ok(None),
                Err(e) if is_foreign_key_violation(&e) => return Ok(None),
                Err(e) if is_unique_violation(&e) && tries < MAX_INSERT_TRIES => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// The pending, unexpired code a typed `user_code` names; `None` for any other.
    pub(crate) async fn pending_device_code(&self, user_code: &str) -> sqlx::Result<Option<PendingDevice>> {
        let Some(code) = normalize_user_code(user_code) else { return Ok(None) };
        type Row = (String, String, String, Option<String>, i64, i64);
        let row: Option<Row> = sqlx::query_as(
            "SELECT client_id, scope, requested_ip, user_agent, \
                    floor(EXTRACT(EPOCH FROM created_at))::int8, floor(EXTRACT(EPOCH FROM expires_at))::int8 \
               FROM accounts.device_codes \
              WHERE user_code_digest = $1 AND status = 'pending' AND expires_at > now()",
        )
        .bind(digest(&code))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(client_id, scope, requested_ip, user_agent, created_at, expires_at)| PendingDevice {
            client_id,
            scope,
            requested_ip,
            user_agent,
            created_at,
            expires_at,
        }))
    }

    /// Approves the pending, unexpired code a typed `user_code` names under the live browser
    /// session whose token hashes to `session_digest`: its player, the session itself (a
    /// foreign key that a session delete sets NULL) and its sign-in time as `auth_time`.
    /// `false` for any other code, or when the session is gone or its player or the session
    /// was deleted before the update, which its foreign-key checks refuse as
    /// [`Store::insert_oidc_code`]'s insert does.
    pub(crate) async fn approve_device_code(&self, user_code: &str, session_digest: &[u8]) -> sqlx::Result<bool> {
        let Some(code) = normalize_user_code(user_code) else { return Ok(false) };
        let res = sqlx::query(
            "UPDATE accounts.device_codes d \
                SET status = 'approved', player_id = s.player_id, session_digest = s.token_digest, \
                    auth_time = s.created_at \
               FROM accounts.browser_sessions s \
              WHERE d.user_code_digest = $1 AND d.status = 'pending' AND d.expires_at > now() \
                AND s.token_digest = $2 AND s.expires_at > now()",
        )
        .bind(digest(&code))
        .bind(session_digest)
        .execute(&self.pool)
        .await;
        match res {
            Ok(done) => Ok(done.rows_affected() == 1),
            Err(e) if is_foreign_key_violation(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Denies the pending, unexpired code a typed `user_code` names; `false` for any other.
    pub(crate) async fn deny_device_code(&self, user_code: &str) -> sqlx::Result<bool> {
        let Some(code) = normalize_user_code(user_code) else { return Ok(false) };
        let res = sqlx::query(
            "UPDATE accounts.device_codes SET status = 'denied' \
              WHERE user_code_digest = $1 AND status = 'pending' AND expires_at > now()",
        )
        .bind(digest(&code))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Answers one poll of `device_code` by `client_id`. Every check is its own statement
    /// that commits on its own, so the device row is never locked across a client or player
    /// lock; a check that loses a race to another statement answers [`DevicePoll::SlowDown`].
    pub(crate) async fn poll_device_code(&self, device_code: &str, client_id: &str) -> sqlx::Result<DevicePoll> {
        if !crate::opaque_token_within_cap(device_code) {
            return Ok(DevicePoll::Unknown);
        }
        let digest = digest(device_code);
        let pool = &self.pool;

        let row: Option<(String, Option<String>, bool)> = sqlx::query_as(
            "SELECT client_id, family_id::text, expires_at <= now() \
               FROM accounts.device_codes WHERE device_code_digest = $1",
        )
        .bind(&digest)
        .fetch_optional(pool)
        .await?;
        let Some((owner, family_id, expired)) = row else { return Ok(DevicePoll::Unknown) };
        if owner != client_id {
            return Ok(DevicePoll::Unknown);
        }
        if let Some(family_id) = family_id {
            self.revoke_device_replay(pool, &family_id).await?;
            return Ok(DevicePoll::Replayed { family_id });
        }
        if expired {
            return Ok(DevicePoll::Expired);
        }

        let too_soon = sqlx::query(
            "UPDATE accounts.device_codes SET interval_secs = interval_secs + $2 \
              WHERE device_code_digest = $1 AND redeemed_at IS NULL \
                AND last_polled_at > now() - make_interval(secs => interval_secs)",
        )
        .bind(&digest)
        .bind(SLOW_DOWN_STEP_SECS)
        .execute(pool)
        .await?;
        if too_soon.rows_affected() == 1 {
            return Ok(DevicePoll::SlowDown);
        }

        let stamped: Option<(String, bool)> = sqlx::query_as(
            "UPDATE accounts.device_codes SET last_polled_at = now() \
              WHERE device_code_digest = $1 AND redeemed_at IS NULL AND expires_at > now() \
                AND (last_polled_at IS NULL \
                     OR last_polled_at <= now() - make_interval(secs => interval_secs)) \
             RETURNING status, session_digest IS NULL",
        )
        .bind(&digest)
        .fetch_optional(pool)
        .await?;
        match stamped.as_ref().map(|(status, signed_out)| (status.as_str(), *signed_out)) {
            None => return Ok(DevicePoll::SlowDown),
            Some(("pending", _)) => return Ok(DevicePoll::Pending),
            Some(("denied", _)) => return Ok(DevicePoll::Denied),
            Some((_, true)) => return Ok(DevicePoll::Expired),
            Some(_) => {}
        }

        let redeemed: Option<(String, String, String, i64, String)> = sqlx::query_as(
            "UPDATE accounts.device_codes SET redeemed_at = now(), family_id = gen_random_uuid() \
              WHERE device_code_digest = $1 AND status = 'approved' AND redeemed_at IS NULL \
                AND session_digest IS NOT NULL AND expires_at > now() \
             RETURNING client_id, player_id::text, scope, floor(EXTRACT(EPOCH FROM auth_time))::int8, \
                       family_id::text",
        )
        .bind(&digest)
        .fetch_optional(pool)
        .await?;
        Ok(match redeemed {
            Some((client_id, player_id, scope, auth_time, family_id)) => {
                DevicePoll::Redeemed(RedeemedDevice { client_id, player_id, scope, auth_time, family_id })
            }
            None => DevicePoll::SlowDown,
        })
    }

    /// Revokes the tokens a replayed `device_code`'s first redemption minted, in its own
    /// transaction, as a replayed authorization code's are.
    async fn revoke_device_replay(&self, pool: &PgPool, family_id: &str) -> sqlx::Result<()> {
        tracing::warn!(%family_id, "device code replayed — revoking the tokens of its first redemption");
        let mut tx = pool.begin().await?;
        self.revoke_replayed_family_tx(&mut tx, family_id).await?;
        tx.commit().await
    }

    /// Deletes every code whose expiry is [`REPLAY_WINDOW_SECS`] or more past, answering how
    /// many; rows another transaction holds are skipped, never waited on.
    pub(crate) async fn prune_device_codes<'e, E: sqlx::PgExecutor<'e>>(&self, db: E) -> sqlx::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM accounts.device_codes WHERE device_code_digest IN ( \
               SELECT device_code_digest FROM accounts.device_codes \
                WHERE expires_at <= now() - make_interval(secs => $1) \
                  FOR UPDATE SKIP LOCKED)",
        )
        .bind(f64::from(REPLAY_WINDOW_SECS))
        .execute(db)
        .await?;
        Ok(res.rows_affected())
    }

    /// Deletes every code approved by a player on the caller's transaction, answering how many.
    pub(crate) async fn delete_player_device_codes_tx(
        &self,
        conn: &mut sqlx::PgConnection,
        player_id: &str,
    ) -> sqlx::Result<u64> {
        let res = sqlx::query("DELETE FROM accounts.device_codes WHERE player_id = $1::uuid")
            .bind(player_id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }
}

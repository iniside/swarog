//! `accounts.device_codes` against the live database. A child of `tests` so it reuses that
//! module's live-DB harness. Expiry, interval and age are set by backdating the row, never by
//! waiting.

use std::net::{IpAddr, Ipv4Addr};

use accountsapi::grant_types::{AUTHORIZATION_CODE, DEVICE_CODE, REFRESH_TOKEN};
use sha2::{Digest as _, Sha256};
use sqlx::PgConnection;

use super::hosted_login::registered;
use super::*;
use crate::codes::REPLAY_WINDOW_SECS;
use crate::device_codes::{
    format_user_code, normalize_user_code, DevicePoll, IssuedDeviceCode, NewDeviceCode, PollAttempt, PollCheck,
    MAX_USER_AGENT_BYTES, POLL_INTERVAL_SECS, SLOW_DOWN_STEP_SECS,
};
use crate::web;
use crate::web::token::mint_code;

/// A public client holding the device grant.
async fn device_client(pool: &PgPool) -> String {
    client_of(pool, &[DEVICE_CODE]).await
}

/// A public client holding `grants`.
async fn client_of(pool: &PgPool, grants: &[&str]) -> String {
    let client_id = format!("t-{}", suffix());
    sqlx::query(
        "INSERT INTO accounts.oidc_clients (client_id, display_name, grant_types, audience, scopes) \
         VALUES ($1, 'device test client', $2, $3, $4)",
    )
    .bind(&client_id)
    .bind(grants)
    .bind(accountsapi::SWAROG_AUDIENCE)
    .bind(&["openid", "profile"][..])
    .execute(pool)
    .await
    .unwrap();
    client_id
}

async fn issue(svc: &Service, client_id: &str) -> IssuedDeviceCode {
    let new = NewDeviceCode {
        client_id,
        scope: "openid",
        requested_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        user_agent: Some("lore/0.14"),
    };
    svc.store.insert_device_code(&new).await.unwrap().expect("a known client")
}

/// A fresh browser session of `pid`, as the digest approval binds to.
async fn session(svc: &Service, pid: &str) -> Vec<u8> {
    let session = svc.store.create_browser_session(pid).await.unwrap().unwrap();
    web::session::digest(&session.token)
}

/// Runs `set` (an assignment list) on the row of `device_code`.
async fn backdate(conn: &mut PgConnection, device_code: &str, set: &str) {
    sqlx::query(&format!(
        "UPDATE accounts.device_codes SET {set} WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))"
    ))
    .bind(device_code)
    .execute(&mut *conn)
    .await
    .unwrap();
}

/// Moves `device_code`'s last poll out of its interval, so the next poll is on time.
async fn on_time(pool: &PgPool, device_code: &str) {
    let mut conn = pool.acquire().await.unwrap();
    backdate(&mut conn, device_code, "last_polled_at = now() - make_interval(secs => interval_secs + 1)").await;
}

async fn interval_of(pool: &PgPool, device_code: &str) -> i32 {
    sqlx::query_scalar("SELECT interval_secs FROM accounts.device_codes WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))")
        .bind(device_code)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn rows_of(conn: &mut PgConnection, device_code: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.device_codes WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))")
        .bind(device_code)
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

async fn device_codes_of(pool: &PgPool, pid: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.device_codes WHERE player_id = $1::uuid")
        .bind(pid)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn poll(svc: &Service, device_code: &str, client_id: &str) -> DevicePoll {
    svc.store.poll_device_code(device_code, client_id).await.unwrap()
}

fn name(poll: &DevicePoll) -> &'static str {
    match poll {
        DevicePoll::Unknown => "unknown",
        DevicePoll::Replayed { .. } => "replayed",
        DevicePoll::Expired => "expired",
        DevicePoll::SlowDown => "slow_down",
        DevicePoll::Pending => "pending",
        DevicePoll::Denied => "denied",
        DevicePoll::Redeemed(_) => "redeemed",
    }
}

/// Polls an approved code to its redemption, answering the family id it stamped.
async fn redeem(svc: &Service, device_code: &str, client_id: &str) -> String {
    on_time(&svc.store.pool, device_code).await;
    match poll(svc, device_code, client_id).await {
        DevicePoll::Redeemed(redeemed) => redeemed.family_id,
        other => panic!("an approved code answered {}", name(&other)),
    }
}

/// Asserts `device_code` answers a replay of `family_id` and that family is revoked.
async fn assert_replay(svc: &Service, device_code: &str, client_id: &str, family_id: &str, case: &str) {
    match poll(svc, device_code, client_id).await {
        DevicePoll::Replayed { family_id: replayed } => assert_eq!(replayed, family_id, "{case}"),
        other => panic!("{case}: answered {}", name(&other)),
    }
    let mut conn = svc.store.pool.acquire().await.unwrap();
    assert!(svc.store.family_revoked_tx(&mut conn, family_id).await.unwrap(), "{case}: family not revoked");
}

#[test]
fn a_typed_user_code_is_upper_cased_and_stripped_of_dashes_and_spaces() {
    assert_eq!(normalize_user_code("bcdf-ghjk").as_deref(), Some("BCDFGHJK"));
    assert_eq!(normalize_user_code(" BCDF GHJK ").as_deref(), Some("BCDFGHJK"));
    for bad in ["BCDF-GHJ", "BCDF-GHJKL", "ABCD-GHJK", "BCDF-GHJ1", "BCDF_GHJK", "ŻCDF-GHJK"] {
        assert_eq!(normalize_user_code(bad), None, "{bad}");
    }
    assert_eq!(format_user_code("BCDFGHJK"), "BCDF-GHJK");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issued_pair_carries_the_clients_lifetime_and_is_found_by_its_typed_user_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let issued = issue(&svc, &client_id).await;
    assert_eq!(issued.expires_in, i64::from(clients::DEFAULT_DEVICE_CODE_TTL_SECS));
    assert_eq!(normalize_user_code(&issued.user_code).map(|c| format_user_code(&c)), Some(issued.user_code.clone()));

    let typed = issued.user_code.to_ascii_lowercase().replace('-', " ");
    let pending = svc.store.pending_device_code(&typed).await.unwrap().expect("found by its typed code");
    assert_eq!(pending.client_id, client_id);
    assert_eq!(pending.scope, "openid");
    assert_eq!(pending.requested_ip, "203.0.113.7");
    assert_eq!(pending.user_agent.as_deref(), Some("lore/0.14"));
    assert_eq!(pending.expires_at - pending.created_at, issued.expires_in);

    let long_ua = "é".repeat(MAX_USER_AGENT_BYTES);
    let new = NewDeviceCode {
        client_id: &client_id,
        scope: "",
        requested_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        user_agent: Some(&long_ua),
    };
    let cut = svc.store.insert_device_code(&new).await.unwrap().unwrap();
    let stored = svc.store.pending_device_code(&cut.user_code).await.unwrap().unwrap().user_agent.unwrap();
    assert_eq!(stored, "é".repeat(MAX_USER_AGENT_BYTES / 2), "a User-Agent past the bound");

    let gone = NewDeviceCode { client_id: "t-no-such-client", ..new };
    assert!(svc.store.insert_device_code(&gone).await.unwrap().is_none(), "an unknown client");
    delete_client(&pool, &client_id).await;
}

/// Every poll answer, in the order the checks run: an unknown or foreign code, pending, too
/// soon (interval grows), denied, approved and redeemed, then replayed inside the interval and
/// after expiry (both revoke), expired, and approved under a session that ended since.
#[tokio::test(flavor = "multi_thread")]
async fn a_poll_answers_every_branch_in_order() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let other_client = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-poll").await;
    let mut conn = pool.acquire().await.unwrap();

    let code = issue(&svc, &client_id).await;
    assert_eq!(name(&poll(&svc, &store::new_token(), &client_id).await), "unknown", "an unknown code");
    assert_eq!(name(&poll(&svc, &code.device_code, &other_client).await), "unknown", "another client's code");
    assert_eq!(name(&poll(&svc, &code.device_code, &client_id).await), "pending", "a first poll");

    // A long interval, so the poll right after is inside it whatever the test's pace.
    backdate(&mut conn, &code.device_code, "interval_secs = 600").await;
    assert_eq!(name(&poll(&svc, &code.device_code, &client_id).await), "slow_down", "inside the interval");
    assert_eq!(interval_of(&pool, &code.device_code).await, 600 + SLOW_DOWN_STEP_SECS);
    on_time(&pool, &code.device_code).await;
    assert_eq!(name(&poll(&svc, &code.device_code, &client_id).await), "pending", "an on-time poll");
    assert_eq!(interval_of(&pool, &code.device_code).await, 600 + SLOW_DOWN_STEP_SECS, "an on-time poll grew it");

    let denied = issue(&svc, &client_id).await;
    assert!(svc.store.deny_device_code(&denied.user_code).await.unwrap());
    assert_eq!(name(&poll(&svc, &denied.device_code, &client_id).await), "denied");

    let approved = issue(&svc, &client_id).await;
    let digest = session(&svc, &pid).await;
    assert!(svc.store.approve_device_code(&approved.user_code, &digest).await.unwrap());
    on_time(&pool, &approved.device_code).await;
    let redeemed = match poll(&svc, &approved.device_code, &client_id).await {
        DevicePoll::Redeemed(redeemed) => redeemed,
        other => panic!("an approved code answered {}", name(&other)),
    };
    let signed_in: i64 = sqlx::query_scalar(
        "SELECT floor(EXTRACT(EPOCH FROM created_at))::int8 FROM accounts.browser_sessions WHERE token_digest = $1",
    )
    .bind(&digest)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((redeemed.client_id.as_str(), redeemed.player_id.as_str()), (client_id.as_str(), pid.as_str()));
    assert_eq!((redeemed.scope.as_str(), redeemed.auth_time), ("openid", signed_in));

    backdate(&mut conn, &approved.device_code, "interval_secs = 600, last_polled_at = now()").await;
    assert_replay(&svc, &approved.device_code, &client_id, &redeemed.family_id, "a replay inside the interval").await;

    let late = issue(&svc, &client_id).await;
    assert!(svc.store.approve_device_code(&late.user_code, &digest).await.unwrap());
    let late_family = redeem(&svc, &late.device_code, &client_id).await;
    backdate(&mut conn, &late.device_code, "expires_at = now() - interval '1 second'").await;
    assert_replay(&svc, &late.device_code, &client_id, &late_family, "a replay after expiry").await;

    let stale = issue(&svc, &client_id).await;
    backdate(&mut conn, &stale.device_code, "expires_at = now() - interval '1 second'").await;
    assert_eq!(name(&poll(&svc, &stale.device_code, &client_id).await), "expired", "past its expiry");

    let signed_out = issue(&svc, &client_id).await;
    let ended = session(&svc, &pid).await;
    assert!(svc.store.approve_device_code(&signed_out.user_code, &ended).await.unwrap());
    sqlx::query("DELETE FROM accounts.browser_sessions WHERE token_digest = $1")
        .bind(&ended)
        .execute(&pool)
        .await
        .unwrap();
    on_time(&pool, &signed_out.device_code).await;
    assert_eq!(name(&poll(&svc, &signed_out.device_code, &client_id).await), "expired", "signed out since approval");

    drop(conn);
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
    delete_client(&pool, &other_client).await;
}

/// The digest a device code's row is keyed by.
fn code_digest(device_code: &str) -> Vec<u8> {
    Sha256::digest(device_code.as_bytes()).to_vec()
}

/// Deletes the row of `device_code`, as a reset or Revoke all does.
async fn delete_row(conn: &mut PgConnection, device_code: &str) {
    sqlx::query("DELETE FROM accounts.device_codes WHERE device_code_digest = $1")
        .bind(code_digest(device_code))
        .execute(&mut *conn)
        .await
        .unwrap();
}

/// Ends the browser session whose digest is `session_digest`, as a sign-out does.
async fn sign_out(pool: &PgPool, session_digest: &[u8]) {
    sqlx::query("DELETE FROM accounts.browser_sessions WHERE token_digest = $1")
        .bind(session_digest)
        .execute(pool)
        .await
        .unwrap();
}

/// A code of `client_id` approved under `session_digest` whose first poll is classified on
/// time, so its stamp runs next.
async fn classified_on_time(svc: &Service, client_id: &str, session_digest: &[u8]) -> IssuedDeviceCode {
    let code = issue(svc, client_id).await;
    assert!(svc.store.approve_device_code(&code.user_code, session_digest).await.unwrap());
    let check = svc.store.classify_device_poll(&code_digest(&code.device_code), client_id, false).await.unwrap();
    assert!(matches!(check, PollCheck::OnTime), "a first poll is on time");
    code
}

/// As [`classified_on_time`], then stamped, so its redemption runs next.
async fn stamped_approved(svc: &Service, client_id: &str, session_digest: &[u8]) -> IssuedDeviceCode {
    let code = classified_on_time(svc, client_id, session_digest).await;
    let check = svc.store.stamp_device_poll(&code_digest(&code.device_code)).await.unwrap();
    assert!(matches!(check, Some(PollCheck::Approved)), "an approved code stamps to its redemption");
    code
}

/// Asserts the stamp of `device_code` loses its guard, then answers the poll as
/// [`Store::poll_device_code`] does after that loss.
async fn after_lost_stamp(svc: &Service, device_code: &str, client_id: &str, case: &str) -> DevicePoll {
    let digest = code_digest(device_code);
    assert!(svc.store.stamp_device_poll(&digest).await.unwrap().is_none(), "{case}: the stamp lost");
    svc.store.settle_device_poll(&digest, client_id, PollAttempt::Lost { stamped: false }).await.unwrap()
}

/// Asserts the redemption of `device_code` loses its guard, then answers the poll as
/// [`Store::poll_device_code`] does after that loss.
async fn after_lost_redeem(svc: &Service, device_code: &str, client_id: &str, case: &str) -> DevicePoll {
    let digest = code_digest(device_code);
    assert!(svc.store.redeem_device_poll(&digest).await.unwrap().is_none(), "{case}: the redemption lost");
    svc.store.settle_device_poll(&digest, client_id, PollAttempt::Lost { stamped: true }).await.unwrap()
}

/// Asserts `answer` replays `family_id` and that family is revoked.
async fn assert_replayed(svc: &Service, answer: DevicePoll, family_id: &str, case: &str) {
    match answer {
        DevicePoll::Replayed { family_id: replayed } => assert_eq!(replayed, family_id, "{case}"),
        other => panic!("{case}: answered {}", name(&other)),
    }
    let mut conn = svc.store.pool.acquire().await.unwrap();
    assert!(svc.store.family_revoked_tx(&mut conn, family_id).await.unwrap(), "{case}: family not revoked");
}

/// A stamp that loses to a change made after the poll was classified reads the row again and
/// answers what the row settled to: an expiry is expired, a delete is unknown, another poll's
/// redemption is a revoked replay, and another poll's stamp is too soon (the interval grows).
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_stamp_answers_the_settled_row() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-lost-stamp").await;
    let live = session(&svc, &pid).await;
    let mut conn = pool.acquire().await.unwrap();

    let expired = classified_on_time(&svc, &client_id, &live).await;
    backdate(&mut conn, &expired.device_code, "expires_at = now() - interval '1 second'").await;
    let answer = after_lost_stamp(&svc, &expired.device_code, &client_id, "expired since").await;
    assert_eq!(name(&answer), "expired", "expired since");

    let deleted = classified_on_time(&svc, &client_id, &live).await;
    delete_row(&mut conn, &deleted.device_code).await;
    let answer = after_lost_stamp(&svc, &deleted.device_code, &client_id, "deleted since").await;
    assert_eq!(name(&answer), "unknown", "deleted since");

    let raced = classified_on_time(&svc, &client_id, &live).await;
    let other = svc.store.redeem_device_poll(&code_digest(&raced.device_code)).await.unwrap().expect("redeemed");
    let answer = after_lost_stamp(&svc, &raced.device_code, &client_id, "redeemed since").await;
    assert_replayed(&svc, answer, &other.family_id, "redeemed since").await;

    let stamped = classified_on_time(&svc, &client_id, &live).await;
    backdate(&mut conn, &stamped.device_code, "last_polled_at = now()").await;
    let answer = after_lost_stamp(&svc, &stamped.device_code, &client_id, "stamped since").await;
    assert_eq!(name(&answer), "slow_down", "stamped since");
    assert_eq!(interval_of(&pool, &stamped.device_code).await, POLL_INTERVAL_SECS + SLOW_DOWN_STEP_SECS);

    drop(conn);
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A redemption that loses to a change made after the poll was stamped reads the row again,
/// without its own stamp reading as too soon: a sign-out or an expiry is expired, a delete is
/// unknown, and another poll's redemption is a revoked replay.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_redemption_answers_the_settled_row() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-lost-redeem").await;
    let live = session(&svc, &pid).await;
    let mut conn = pool.acquire().await.unwrap();

    let ended = session(&svc, &pid).await;
    let signed_out = stamped_approved(&svc, &client_id, &ended).await;
    sign_out(&pool, &ended).await;
    let answer = after_lost_redeem(&svc, &signed_out.device_code, &client_id, "signed out since").await;
    assert_eq!(name(&answer), "expired", "signed out since");
    let interval = interval_of(&pool, &signed_out.device_code).await;
    assert_eq!(interval, POLL_INTERVAL_SECS, "its own stamp read as too soon");

    let expired = stamped_approved(&svc, &client_id, &live).await;
    backdate(&mut conn, &expired.device_code, "expires_at = now() - interval '1 second'").await;
    let answer = after_lost_redeem(&svc, &expired.device_code, &client_id, "expired since").await;
    assert_eq!(name(&answer), "expired", "expired since");

    let deleted = stamped_approved(&svc, &client_id, &live).await;
    delete_row(&mut conn, &deleted.device_code).await;
    let answer = after_lost_redeem(&svc, &deleted.device_code, &client_id, "deleted since").await;
    assert_eq!(name(&answer), "unknown", "deleted since");

    let raced = stamped_approved(&svc, &client_id, &live).await;
    let other = svc.store.redeem_device_poll(&code_digest(&raced.device_code)).await.unwrap().expect("redeemed");
    let answer = after_lost_redeem(&svc, &raced.device_code, &client_id, "redeemed since").await;
    assert_replayed(&svc, answer, &other.family_id, "redeemed since").await;

    drop(conn);
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A redeemed code polled again more than [`REPLAY_WINDOW_SECS`] past its expiry is refused
/// as unknown and revokes nothing, even while the prune has not deleted its row yet.
#[tokio::test(flavor = "multi_thread")]
async fn a_replay_past_the_window_revokes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-late-replay").await;
    let mut conn = pool.acquire().await.unwrap();

    let code = issue(&svc, &client_id).await;
    assert!(svc.store.approve_device_code(&code.user_code, &session(&svc, &pid).await).await.unwrap());
    let family_id = redeem(&svc, &code.device_code, &client_id).await;
    let past = format!("expires_at = now() - make_interval(secs => {})", REPLAY_WINDOW_SECS + 1);
    backdate(&mut conn, &code.device_code, &past).await;
    assert_eq!(rows_of(&mut conn, &code.device_code).await, 1, "the prune has not run");
    assert_eq!(name(&poll(&svc, &code.device_code, &client_id).await), "unknown", "a replay past the window");
    assert!(!svc.store.family_revoked_tx(&mut conn, &family_id).await.unwrap(), "a replay past the window revoked");

    drop(conn);
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// Approve and deny move only a pending, unexpired code, and approve needs a live session.
#[tokio::test(flavor = "multi_thread")]
async fn approve_and_deny_move_only_a_pending_unexpired_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-approve").await;
    let digest = session(&svc, &pid).await;
    let mut conn = pool.acquire().await.unwrap();

    let code = issue(&svc, &client_id).await;
    assert!(!svc.store.approve_device_code(&code.user_code, &store::new_token().into_bytes()).await.unwrap(), "no session");
    assert!(svc.store.approve_device_code(&code.user_code, &digest).await.unwrap());
    assert!(!svc.store.approve_device_code(&code.user_code, &digest).await.unwrap(), "a second approve");
    assert!(!svc.store.deny_device_code(&code.user_code).await.unwrap(), "a deny after approval");
    assert!(svc.store.pending_device_code(&code.user_code).await.unwrap().is_none(), "approved is not pending");

    let denied = issue(&svc, &client_id).await;
    assert!(svc.store.deny_device_code(&denied.user_code).await.unwrap());
    assert!(!svc.store.approve_device_code(&denied.user_code, &digest).await.unwrap(), "an approve after deny");

    let stale = issue(&svc, &client_id).await;
    backdate(&mut conn, &stale.device_code, "expires_at = now() - interval '1 second'").await;
    assert!(!svc.store.approve_device_code(&stale.user_code, &digest).await.unwrap(), "an approve after expiry");
    assert!(!svc.store.deny_device_code(&stale.user_code).await.unwrap(), "a deny after expiry");
    assert!(svc.store.pending_device_code(&stale.user_code).await.unwrap().is_none(), "expired is not pending");

    drop(conn);
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_reset_and_revoke_all_delete_the_players_device_codes() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-reset").await;

    let code = issue(&svc, &client_id).await;
    assert!(svc.store.approve_device_code(&code.user_code, &session(&svc, &pid).await).await.unwrap());
    redeem(&svc, &code.device_code, &client_id).await;
    svc.reset_password(&pid).await.unwrap();
    assert_eq!(device_codes_of(&pool, &pid).await, 0, "a password reset left the player's device codes");

    let code = issue(&svc, &client_id).await;
    assert!(svc.store.approve_device_code(&code.user_code, &session(&svc, &pid).await).await.unwrap());
    let mut tx = pool.begin().await.unwrap();
    assert!(svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap().is_some());
    tx.commit().await.unwrap();
    assert_eq!(device_codes_of(&pool, &pid).await, 0, "Revoke all left the player's device codes");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// The prune keeps a code until the replay window has passed its expiry and skips a row
/// another transaction holds instead of waiting on it.
#[tokio::test(flavor = "multi_thread")]
async fn the_prune_keeps_the_replay_window_and_skips_locked_rows() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let code = issue(&svc, &client_id).await;

    let mut tx = pool.begin().await.unwrap();
    let window = f64::from(REPLAY_WINDOW_SECS);
    backdate(&mut tx, &code.device_code, &format!("expires_at = now() - make_interval(secs => {window} - 0.001)")).await;
    svc.store.prune_device_codes(&mut *tx).await.unwrap();
    assert_eq!(rows_of(&mut tx, &code.device_code).await, 1, "pruned inside the replay window");
    backdate(&mut tx, &code.device_code, &format!("expires_at = now() - make_interval(secs => {window})")).await;
    svc.store.prune_device_codes(&mut *tx).await.unwrap();
    assert_eq!(rows_of(&mut tx, &code.device_code).await, 0, "kept past the replay window");
    tx.rollback().await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    backdate(&mut conn, &code.device_code, &format!("expires_at = now() - make_interval(secs => {window} + 1)")).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM accounts.device_codes WHERE device_code_digest = sha256(convert_to($1, 'UTF8')) FOR UPDATE")
        .bind(&code.device_code)
        .execute(&mut *holder)
        .await
        .unwrap();
    // The timeout only turns a wait on the held row into a failure instead of a hang.
    tokio::time::timeout(Duration::from_secs(30), svc.store.prune_device_codes(&pool))
        .await
        .expect("the prune waited on a locked row")
        .unwrap();
    assert_eq!(rows_of(&mut conn, &code.device_code).await, 1, "the prune took a locked row");
    holder.rollback().await.unwrap();
    svc.store.prune_device_codes(&pool).await.unwrap();
    assert_eq!(rows_of(&mut conn, &code.device_code).await, 0, "an unlocked expired row kept");

    drop(conn);
    delete_client(&pool, &client_id).await;
}

/// A code of `client_id` approved by `pid` under `scope` and redeemed by a poll.
async fn redeemed(svc: &Service, client_id: &str, pid: &str, scope: &str) -> crate::device_codes::RedeemedDevice {
    let code = issue(svc, client_id).await;
    let mut conn = svc.store.pool.acquire().await.unwrap();
    backdate(&mut conn, &code.device_code, &format!("scope = '{scope}'")).await;
    assert!(svc.store.approve_device_code(&code.user_code, &session(svc, pid).await).await.unwrap());
    on_time(&svc.store.pool, &code.device_code).await;
    match poll(svc, &code.device_code, client_id).await {
        DevicePoll::Redeemed(redeemed) => redeemed,
        other => panic!("an approved code answered {}", name(&other)),
    }
}

async fn families_of(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A redeemed device code mints what a code exchange does, under its redemption's family: a
/// refresh token only when the client holds that grant and an id_token only for `openid`.
#[tokio::test(flavor = "multi_thread")]
async fn a_redeemed_device_code_mints_under_its_family() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let refreshing = client_of(&pool, &[DEVICE_CODE, REFRESH_TOKEN]).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-mint").await;

    let grant = redeemed(&svc, &refreshing, &pid, "openid").await;
    let (session, id_token) = mint_code(&svc, &grant).await.expect("a device code with the grant mints");
    assert_eq!(session.player_id, pid);
    assert!(!session.refresh_token.is_empty(), "no refresh token for a client holding the grant");
    assert!(id_token.is_some(), "no id_token for openid");
    assert_eq!(families_of(&pool, &grant.family_id).await, 1, "not minted under the redemption's family");

    let grant = redeemed(&svc, &client_id, &pid, "").await;
    let (session, id_token) = mint_code(&svc, &grant).await.expect("a device code with the grant mints");
    assert!(session.refresh_token.is_empty(), "a refresh token for a client without the grant");
    assert!(id_token.is_none(), "an id_token without openid");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &refreshing).await;
    delete_client(&pool, &client_id).await;
}

/// A device code mints only for a client listing the device grant: holding
/// `authorization_code` instead is `invalid_grant`, with no family.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_mint_needs_the_device_grant() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = client_of(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN]).await;
    let (_, pid) = registered(&svc, "device-no-grant").await;

    let grant = redeemed(&svc, &client_id, &pid, "openid").await;
    let minted = mint_code(&svc, &grant).await;
    assert_eq!(minted.unwrap_err().status, opsapi::Status::Unauthorized, "a mint without the device grant");
    assert_eq!(families_of(&pool, &grant.family_id).await, 0);

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// The mint re-reads the redeemed device code's row after the player's key share: a code a
/// reset deleted between the redemption and the mint is `invalid_grant`, with no family.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_code_deleted_before_the_mint_prevents_it() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = device_client(&pool).await;
    let (_, pid) = registered(&svc, "device-kept").await;

    let grant = redeemed(&svc, &client_id, &pid, "openid").await;
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(svc.store.delete_player_device_codes_tx(&mut tx, &pid).await.unwrap(), 1);
    tx.commit().await.unwrap();
    let minted = mint_code(&svc, &grant).await;
    assert_eq!(minted.unwrap_err().status, opsapi::Status::Unauthorized, "the mint after the delete");
    assert_eq!(families_of(&pool, &grant.family_id).await, 0, "a family minted after the delete");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

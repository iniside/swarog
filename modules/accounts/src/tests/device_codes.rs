//! `accounts.device_codes` against the live database. A child of `tests` so it reuses that
//! module's live-DB harness. Expiry, interval and age are set by backdating the row, never by
//! waiting.

use std::net::{IpAddr, Ipv4Addr};

use accountsapi::grant_types::DEVICE_CODE;
use sqlx::PgConnection;

use super::hosted_login::registered;
use super::*;
use crate::codes::REPLAY_WINDOW_SECS;
use crate::device_codes::{
    format_user_code, normalize_user_code, DevicePoll, IssuedDeviceCode, NewDeviceCode, MAX_USER_AGENT_BYTES,
    SLOW_DOWN_STEP_SECS,
};
use crate::web;

/// A public client holding the device grant.
async fn device_client(pool: &PgPool) -> String {
    let client_id = format!("t-{}", suffix());
    sqlx::query(
        "INSERT INTO accounts.oidc_clients (client_id, display_name, grant_types, audience, scopes) \
         VALUES ($1, 'device test client', $2, $3, $4)",
    )
    .bind(&client_id)
    .bind(&[DEVICE_CODE][..])
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

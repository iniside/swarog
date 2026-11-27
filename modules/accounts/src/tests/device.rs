//! `POST /accounts/oidc/device_authorization` and the `/token` device arm over a real listener
//! against the live database. A child of `tests` so it reuses that module's live-DB harness.
//! Expiry and the poll interval are set by backdating the row, never by waiting.

use accountsapi::grant_types::{AUTHORIZATION_CODE, DEVICE_CODE, REFRESH_TOKEN};

use super::hosted_login::{client, registered, serve_default};
use super::token::{families, json, token_client};
use super::*;
use crate::device_codes::DevicePoll;
use crate::web;
use crate::web::device::DEVICE_AUTHORIZATION;
use crate::web::token::{mint_code, TOKEN};

async fn authorize(base: &str, form: &[(&str, &str)]) -> reqwest::Response {
    client().post(format!("{base}{DEVICE_AUTHORIZATION}")).form(form).send().await.unwrap()
}

/// A code pair for `client_id` under `scope`: `(device_code, user_code)`.
async fn pair(base: &str, client_id: &str, scope: &str) -> (String, String) {
    let resp = authorize(base, &[("client_id", client_id), ("scope", scope)]).await;
    assert_eq!(resp.status(), 200, "device authorization refused");
    let body = json(resp).await;
    (body["device_code"].as_str().unwrap().to_string(), body["user_code"].as_str().unwrap().to_string())
}

async fn poll(base: &str, client_id: &str, device_code: &str) -> reqwest::Response {
    let form = [("grant_type", DEVICE_CODE), ("device_code", device_code), ("client_id", client_id)];
    client().post(format!("{base}{TOKEN}")).form(&form).send().await.unwrap()
}

/// Asserts an RFC 6749 §5.2 answer of `status` and `error`.
async fn assert_error(resp: reqwest::Response, status: u16, error: &str, case: &str) {
    assert_eq!(resp.status(), status, "{case}");
    assert_eq!(resp.headers()["cache-control"], "no-store", "{case}");
    assert_eq!(json(resp).await["error"], error, "{case}");
}

/// Runs `set` (an assignment list) on the row of `device_code`.
async fn backdate(pool: &PgPool, device_code: &str, set: &str) {
    sqlx::query(&format!(
        "UPDATE accounts.device_codes SET {set} WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))"
    ))
    .bind(device_code)
    .execute(pool)
    .await
    .unwrap();
}

/// Moves `device_code`'s last poll out of its interval, so the next poll is on time.
async fn on_time(pool: &PgPool, device_code: &str) {
    backdate(pool, device_code, "last_polled_at = now() - make_interval(secs => interval_secs + 1)").await;
}

/// Approves `user_code` under a fresh browser session of `pid`, answering that session's digest.
async fn approve(svc: &Service, user_code: &str, pid: &str) -> Vec<u8> {
    let session = svc.store.create_browser_session(pid).await.unwrap().unwrap();
    let digest = web::session::digest(&session.token);
    assert!(svc.store.approve_device_code(user_code, &digest).await.unwrap(), "approval refused");
    digest
}

async fn codes_of(pool: &PgPool, client_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.device_codes WHERE client_id = $1")
        .bind(client_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_of(pool: &PgPool, device_code: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT family_id::text FROM accounts.device_codes WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(device_code)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The token families `pid` holds for `client_id`; the families its sign-in minted for the test
/// client are not counted.
async fn client_families(pool: &PgPool, pid: &str, client_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE player_id = $1::uuid AND client_id = $2")
        .bind(pid)
        .bind(client_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn revoked(svc: &Service, family_id: &str) -> bool {
    let mut conn = svc.store.pool.acquire().await.unwrap();
    svc.store.family_revoked_tx(&mut conn, family_id).await.unwrap()
}

/// The answer carries the code pair, URIs built from the issuer (not from `Host`), the
/// interval and `no-store`; the row binds the canonical scope, the peer IP (no trusted proxy,
/// so `X-Forwarded-For` is ignored) and the User-Agent.
#[tokio::test(flavor = "multi_thread")]
async fn device_authorization_answers_a_code_pair() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let base = serve_default(svc).await;

    let resp = client()
        .post(format!("{base}{DEVICE_AUTHORIZATION}"))
        .header("host", "evil.example")
        .header("x-forwarded-for", "198.51.100.9")
        .header("user-agent", "lore/0.14")
        .form(&[("client_id", client_id.as_str()), ("scope", "profile openid")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let body = json(resp).await;
    let user_code = body["user_code"].as_str().unwrap();
    assert_eq!(user_code.len(), 9, "user_code {user_code} is not XXXX-XXXX");
    assert!(!body["device_code"].as_str().unwrap().is_empty());
    let uri = format!("{}/device", accountsapi::DEFAULT_OIDC_ISSUER);
    assert_eq!(body["verification_uri"], uri.as_str());
    assert_eq!(body["verification_uri_complete"], format!("{uri}?user_code={user_code}").as_str());
    assert_eq!(body["interval"], 5);
    assert!(body["expires_in"].as_i64().unwrap() > 0);

    let (scope, ip, ua): (String, String, Option<String>) = sqlx::query_as(
        "SELECT scope, requested_ip, user_agent FROM accounts.device_codes \
          WHERE device_code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(body["device_code"].as_str().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scope, "openid profile");
    assert_eq!(ip, "127.0.0.1", "the requesting IP came from an untrusted X-Forwarded-For");
    assert_eq!(ua.as_deref(), Some("lore/0.14"));

    delete_client(&pool, &client_id).await;
}

/// A confidential client, a client without the device grant, an unknown client and a scope the
/// client does not list are each refused, and none issues a code.
#[tokio::test(flavor = "multi_thread")]
async fn device_authorization_refusals() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let confidential = token_client(&pool, &[DEVICE_CODE], true).await;
    let code_only = token_client(&pool, &[AUTHORIZATION_CODE], false).await;
    let device = token_client(&pool, &[DEVICE_CODE], false).await;
    let base = serve_default(svc).await;

    let cases = [
        ("confidential client", vec![("client_id", confidential.as_str())], 400, "unauthorized_client"),
        ("client without the grant", vec![("client_id", code_only.as_str())], 400, "unauthorized_client"),
        ("unknown client", vec![("client_id", "t-no-such-client")], 401, "invalid_client"),
        ("no client", vec![("scope", "openid")], 401, "invalid_client"),
        ("unlisted scope", vec![("client_id", device.as_str()), ("scope", "openid offline_access")], 400, "invalid_scope"),
    ];
    for (case, form, status, error) in cases {
        assert_error(authorize(&base, &form).await, status, error, case).await;
    }
    let not_a_form = client().post(format!("{base}{DEVICE_AUTHORIZATION}")).body("client_id=x").send().await.unwrap();
    assert_error(not_a_form, 400, "invalid_request", "not a form").await;
    for client_id in [&confidential, &code_only, &device] {
        assert_eq!(codes_of(&pool, client_id).await, 0, "a refused request issued a code");
        delete_client(&pool, client_id).await;
    }
}

/// pending → slow_down → approved → tokens with a refresh token and an id_token; a poll after
/// the redemption is a replay, `invalid_grant`, and revokes the family it minted.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_poll_runs_pending_slow_down_tokens_then_replay() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE, REFRESH_TOKEN], false).await;
    let other = token_client(&pool, &[DEVICE_CODE], false).await;
    let (_, pid) = registered(&svc, "device-flow").await;
    let base = serve_default(svc.clone()).await;
    let (device_code, user_code) = pair(&base, &client_id, "openid").await;

    assert_error(poll(&base, &client_id, &device_code).await, 400, "authorization_pending", "first poll").await;
    assert_error(poll(&base, &client_id, &device_code).await, 400, "slow_down", "a poll too soon").await;
    assert_error(poll(&base, &other, &device_code).await, 400, "invalid_grant", "another client's code").await;
    let form = [("grant_type", DEVICE_CODE), ("client_id", client_id.as_str())];
    let missing = client().post(format!("{base}{TOKEN}")).form(&form).send().await.unwrap();
    assert_error(missing, 400, "invalid_request", "no device_code").await;

    approve(&svc, &user_code, &pid).await;
    on_time(&pool, &device_code).await;
    let resp = poll(&base, &client_id, &device_code).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let body = json(resp).await;
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["access_token"].as_str().is_some_and(|t| !t.is_empty()));
    assert!(body["refresh_token"].as_str().is_some_and(|t| !t.is_empty()), "no refresh token");
    assert!(body["id_token"].as_str().is_some_and(|t| !t.is_empty()), "no id_token for openid");
    assert_eq!(body["scope"], "openid");
    let family_id = family_of(&pool, &device_code).await.expect("a redeemed code has a family");
    assert_eq!(families(&pool, &family_id).await, 1, "not minted under the redemption's family");

    on_time(&pool, &device_code).await;
    assert_error(poll(&base, &client_id, &device_code).await, 400, "invalid_grant", "a replay").await;
    assert!(revoked(&svc, &family_id).await, "a replay left its first redemption's family live");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
    delete_client(&pool, &other).await;
}

/// A client without the device grant polling is `unauthorized_client`, before any code lookup.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_poll_needs_the_device_grant() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let base = serve_default(svc).await;
    assert_error(poll(&base, &client_id, "any").await, 400, "unauthorized_client", "no device grant").await;
    delete_client(&pool, &client_id).await;
}

/// A denied code is `access_denied`; an expired one, or one approved under a browser session that
/// has since ended, `expired_token`.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_poll_answers_denied_and_expired() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let (_, pid) = registered(&svc, "device-settled").await;
    let base = serve_default(svc.clone()).await;

    let (denied, user_code) = pair(&base, &client_id, "").await;
    assert!(svc.store.deny_device_code(&user_code).await.unwrap());
    assert_error(poll(&base, &client_id, &denied).await, 400, "access_denied", "denied").await;

    let (expired, _) = pair(&base, &client_id, "").await;
    let past = "created_at = now() - interval '20 minutes', expires_at = now() - interval '1 second'";
    backdate(&pool, &expired, past).await;
    assert_error(poll(&base, &client_id, &expired).await, 400, "expired_token", "expired").await;

    let (signed_out, user_code) = pair(&base, &client_id, "").await;
    let session = approve(&svc, &user_code, &pid).await;
    sqlx::query("DELETE FROM accounts.browser_sessions WHERE token_digest = $1")
        .bind(&session)
        .execute(&pool)
        .await
        .unwrap();
    assert_error(poll(&base, &client_id, &signed_out).await, 400, "expired_token", "signed out").await;

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A password reset (or, with `revoke_all`, a Revoke all) holding the player row lock when a
/// redeemed device code's mint takes its key share deletes the code: the mint waits on the
/// lock, then is `invalid_grant` and the player has no family.
async fn a_reset_before_the_device_mint_prevents_it(revoke_all: bool) {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, if revoke_all { "device-revoke-all" } else { "device-reset" }).await;
    let base = serve_default(svc.clone()).await;
    let (device_code, user_code) = pair(&base, &client_id, "openid").await;
    approve(&svc, &user_code, &pid).await;
    let redeemed = match svc.store.poll_device_code(&device_code, &client_id).await.unwrap() {
        DevicePoll::Redeemed(redeemed) => redeemed,
        _ => panic!("an approved code was not redeemed"),
    };

    let mut reset = pool.begin().await.unwrap();
    let reset_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *reset).await.unwrap();
    let held = if revoke_all {
        svc.store.revoke_player_families_tx(&mut reset, &pid).await.unwrap().is_some()
    } else {
        svc.store.kill_player_families_tx(&mut reset, &pid).await.unwrap().is_some()
    };
    assert!(held, "no player to reset");
    let mint = tokio::spawn({
        let svc = svc.clone();
        async move { mint_code(&svc, &redeemed).await.map(|_| ()) }
    });
    let waiting = blocked_query(&pool, reset_pid).await.unwrap_or_default();
    assert!(waiting.contains("accounts.players"), "the mint never waited on the player lock: {waiting:?}");
    reset.commit().await.unwrap();
    let minted = mint.await.unwrap();
    assert_eq!(minted.unwrap_err().status, opsapi::Status::Unauthorized, "the mint after the reset");
    let families = client_families(&pool, &pid, &client_id).await;
    assert_eq!(families, 0, "a family minted after the reset");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_reset_before_the_device_mint_prevents_it() {
    a_reset_before_the_device_mint_prevents_it(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoke_all_before_the_device_mint_prevents_it() {
    a_reset_before_the_device_mint_prevents_it(true).await;
}

/// A poll of an approved code arriving while `hold` deletes the code's row (a reset, or a
/// client delete's cascade) waits on that row, then answers `invalid_grant` once the delete
/// commits: no deadlock, no 5xx, no family.
async fn a_poll_racing_a_delete_is_invalid_grant(hold_client: bool) {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let (_, pid) = registered(&svc, if hold_client { "device-client-gone" } else { "device-poll-reset" }).await;
    let base = serve_default(svc.clone()).await;
    let (device_code, user_code) = pair(&base, &client_id, "openid").await;
    approve(&svc, &user_code, &pid).await;

    let mut hold = pool.begin().await.unwrap();
    let hold_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *hold).await.unwrap();
    if hold_client {
        sqlx::query("SELECT 1 FROM accounts.oidc_clients WHERE client_id = $1 FOR UPDATE")
            .bind(&client_id)
            .execute(&mut *hold)
            .await
            .unwrap();
        sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
            .bind(&client_id)
            .execute(&mut *hold)
            .await
            .unwrap();
    } else {
        assert!(svc.store.kill_player_families_tx(&mut hold, &pid).await.unwrap().is_some(), "no player to reset");
    }
    let polled = tokio::spawn({
        let (base, client_id, device_code) = (base.clone(), client_id.clone(), device_code.clone());
        async move {
            let resp = poll(&base, &client_id, &device_code).await;
            (resp.status().as_u16(), json(resp).await)
        }
    });
    let waiting = blocked_query(&pool, hold_pid).await.unwrap_or_default();
    assert!(waiting.contains("accounts.device_codes"), "the poll never waited on the code's row: {waiting:?}");
    hold.commit().await.unwrap();
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(30), polled).await.unwrap().unwrap();
    assert_eq!((status, body["error"].as_str()), (400, Some("invalid_grant")), "the poll after the delete");
    let families = client_families(&pool, &pid, &client_id).await;
    assert_eq!(families, 0, "a family minted after the delete");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_poll_racing_a_reset_is_invalid_grant() {
    a_poll_racing_a_delete_is_invalid_grant(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_poll_racing_a_client_delete_is_invalid_grant() {
    a_poll_racing_a_delete_is_invalid_grant(true).await;
}

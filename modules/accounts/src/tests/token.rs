//! `POST /accounts/oidc/token` against the live database. A child of `tests` so it reuses that
//! module's live-DB harness.

use accountsapi::grant_types::{AUTHORIZATION_CODE, REFRESH_TOKEN};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::hosted_login::{client, registered, serve_default};
use super::*;
use crate::codes::NewCode;
use crate::web;
use crate::web::token::{mint_code, TOKEN};

const CALLBACK: &str = "https://app.example/cb";
const VERIFIER: &str = "dBjftJeZ4CVP-mJ92K9yLx3_kq0~OOXWqDHVvF.Tm7Q-verifier";
const SECRET: &str = "cs_token-test-secret";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A client of `grants` returning to [`CALLBACK`], confidential under [`SECRET`] when asked.
async fn token_client(pool: &PgPool, grants: &[&str], confidential: bool) -> String {
    let client_id = format!("t-{}", suffix());
    sqlx::query(
        "INSERT INTO accounts.oidc_clients \
           (client_id, display_name, grant_types, audience, redirect_uris, scopes, secret_digest) \
         VALUES ($1, 'token test client', $2, $3, $4, $5, $6)",
    )
    .bind(&client_id)
    .bind(grants)
    .bind(accountsapi::SWAROG_AUDIENCE)
    .bind(&[CALLBACK][..])
    .bind(&["openid", "profile"][..])
    .bind(confidential.then(|| crate::secret::digest(SECRET)))
    .execute(pool)
    .await
    .unwrap();
    client_id
}

/// A code for `client_id` and `pid` under a fresh browser session, challenged by [`VERIFIER`].
async fn issue(svc: &Service, client_id: &str, pid: &str, scope: &str, nonce: Option<&str>) -> String {
    let session = svc.store.create_browser_session(pid).await.unwrap().unwrap();
    let code = store::new_token();
    let challenge = challenge(VERIFIER);
    let new = NewCode { client_id, redirect_uri: CALLBACK, code_challenge: &challenge, scope, nonce };
    assert!(svc.store.insert_oidc_code(&code, &web::session::digest(&session.token), &new).await.unwrap());
    code
}

fn exchange<'a>(client_id: &'a str, code: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("grant_type", AUTHORIZATION_CODE),
        ("code", code),
        ("redirect_uri", CALLBACK),
        ("code_verifier", VERIFIER),
        ("client_id", client_id),
    ]
}

fn refresh_form<'a>(token: &'a str, client_id: &'a str) -> [(&'a str, &'a str); 3] {
    [("grant_type", REFRESH_TOKEN), ("refresh_token", token), ("client_id", client_id)]
}

fn set<'a>(mut form: Vec<(&'a str, &'a str)>, name: &'a str, value: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    form.retain(|(k, _)| *k != name);
    if let Some(value) = value {
        form.push((name, value));
    }
    form
}

async fn post(base: &str, form: &[(&str, &str)], basic: Option<(&str, &str)>) -> reqwest::Response {
    let mut req = client().post(format!("{base}{TOKEN}")).form(form);
    if let Some((id, secret)) = basic {
        req = req.basic_auth(id, Some(secret));
    }
    req.send().await.unwrap()
}

async fn json(resp: reqwest::Response) -> serde_json::Value {
    resp.json().await.unwrap()
}

/// Asserts an RFC 6749 §5.2 answer of `status` and `error`.
async fn assert_error(resp: reqwest::Response, status: u16, error: &str, case: &str) {
    assert_eq!(resp.status(), status, "{case}");
    assert_eq!(resp.headers()["cache-control"], "no-store", "{case}");
    assert_eq!(json(resp).await["error"], error, "{case}");
}

async fn redeemed(pool: &PgPool, code: &str) -> bool {
    sqlx::query_scalar(
        "SELECT redeemed_at IS NOT NULL FROM accounts.oidc_codes WHERE code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(code)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn code_family(pool: &PgPool, code: &str) -> String {
    sqlx::query_scalar(
        "SELECT family_id::text FROM accounts.oidc_codes WHERE code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(code)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn families(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_revocations(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_revocations WHERE family_id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn refresh_used(pool: &PgPool, token: &str) -> bool {
    sqlx::query_scalar("SELECT used_at IS NOT NULL FROM accounts.refresh_tokens WHERE token = $1")
        .bind(token)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn header_typ(jwt: &str) -> String {
    let header = jwt.split('.').next().unwrap();
    let header: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap();
    header["typ"].as_str().unwrap().to_string()
}

/// A code + PKCE exchange answers an access token, a refresh token when the client lists the
/// grant, and an id_token with `nonce` and `auth_time` only for `openid`.
#[tokio::test(flavor = "multi_thread")]
async fn a_code_exchange_mints_per_the_client_grant_and_the_openid_scope() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let with_refresh = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let code_only = token_client(&pool, &[AUTHORIZATION_CODE], false).await;
    let (_, pid) = registered(&svc, "token-full").await;
    let handle = handle_of(&svc, &pid).await;

    let code = issue(&svc, &with_refresh, &pid, "openid profile", Some("n-1")).await;
    let resp = post(&base, &exchange(&with_refresh, &code), None).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let body = json(resp).await;
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], store::access_expires_in_secs());
    assert_eq!(body["scope"], "openid profile");
    assert!(body["refresh_token"].as_str().is_some_and(|t| !t.is_empty()), "{body}");
    let access = body["access_token"].as_str().unwrap();
    let claims = svc.tokens.verify_access(access).await.unwrap().expect("the access token verifies");
    assert_eq!((claims.sub.as_str(), claims.client_id.as_str()), (pid.as_str(), with_refresh.as_str()));
    assert_eq!(claims.sid, code_family(&pool, &code).await, "the family the redemption chose");
    let id_token = body["id_token"].as_str().unwrap();
    assert_eq!(header_typ(id_token), crate::jwt::ID_TOKEN_TYP);
    assert!(svc.tokens.verify_access(id_token).await.unwrap().is_none(), "an id_token taken as an access token");
    let id = unverified_claims(id_token);
    assert_eq!(id["sub"], pid.as_str());
    assert_eq!(id["aud"], with_refresh.as_str());
    assert_eq!(id["nonce"], "n-1");
    assert_eq!(id["preferred_username"], handle.as_str());
    assert_eq!(id["iat"], claims.iat);
    let auth_time: i64 = sqlx::query_scalar(
        "SELECT floor(EXTRACT(EPOCH FROM auth_time))::int8 FROM accounts.oidc_codes \
          WHERE code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(&code)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(id["auth_time"], auth_time);

    let code = issue(&svc, &code_only, &pid, "profile", None).await;
    let body = json(post(&base, &exchange(&code_only, &code), None).await).await;
    assert!(body["access_token"].is_string(), "{body}");
    assert_eq!(body["scope"], "profile");
    assert!(body.get("refresh_token").is_none(), "a refresh token for a client without the grant: {body}");
    assert!(body.get("id_token").is_none(), "an id_token without openid: {body}");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &with_refresh).await;
    delete_client(&pool, &code_only).await;
}

/// Every check after the redemption refuses with the code consumed and nothing minted: a wrong
/// verifier, a malformed one, a wrong redirect URI, another client, and missing parameters.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_exchange_leaves_the_code_consumed() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let other = token_client(&pool, &[AUTHORIZATION_CODE], false).await;
    let (_, pid) = registered(&svc, "token-refused").await;
    let wrong_verifier = "a".repeat(43);
    let short = "a".repeat(42);
    let long = "a".repeat(129);

    let reserved = format!("a+{}", "b".repeat(43));
    let cases: [(&str, &str, Option<&str>, &str); 7] = [
        ("wrong verifier", "code_verifier", Some(&wrong_verifier), "invalid_grant"),
        ("short verifier", "code_verifier", Some(&short), "invalid_request"),
        ("long verifier", "code_verifier", Some(&long), "invalid_request"),
        ("reserved verifier char", "code_verifier", Some(&reserved), "invalid_request"),
        ("no verifier", "code_verifier", None, "invalid_request"),
        ("wrong redirect", "redirect_uri", Some("https://app.example/other"), "invalid_grant"),
        ("another client", "client_id", Some(&other), "invalid_grant"),
    ];
    for (case, name, value, error) in cases {
        let code = issue(&svc, &client_id, &pid, "openid", None).await;
        let form = set(exchange(&client_id, &code), name, value);
        assert_error(post(&base, &form, None).await, 400, error, case).await;
        assert!(redeemed(&pool, &code).await, "{case}: the code is not consumed");
        assert_eq!(families(&pool, &code_family(&pool, &code).await).await, 0, "{case}: a family was minted");
        let retry = post(&base, &exchange(&client_id, &code), None).await;
        assert_error(retry, 400, "invalid_grant", &format!("{case}: the retry")).await;
    }
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let no_code = set(exchange(&client_id, &code), "code", None);
    assert_error(post(&base, &no_code, None).await, 400, "invalid_request", "no code").await;
    assert!(!redeemed(&pool, &code).await, "a request without a code redeemed one");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
    delete_client(&pool, &other).await;
}

/// A confidential client must send its secret: without it, or with a wrong one, 401
/// `invalid_client` before the code is touched, challenging Basic only when Basic was used.
/// With it, by Basic or in the body, it exchanges and refreshes its own family.
#[tokio::test(flavor = "multi_thread")]
async fn a_confidential_client_authenticates_with_its_secret() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], true).await;
    let (_, pid) = registered(&svc, "token-conf").await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;

    let resp = post(&base, &exchange(&client_id, &code), None).await;
    assert!(resp.headers().get("www-authenticate").is_none(), "a Basic challenge without Basic");
    assert_error(resp, 401, "invalid_client", "no secret").await;
    let wrong = set(exchange(&client_id, &code), "client_secret", Some("cs_wrong"));
    assert_error(post(&base, &wrong, None).await, 401, "invalid_client", "wrong post secret").await;
    let resp = post(&base, &set(exchange(&client_id, &code), "client_id", None), Some((&client_id, "cs_wrong"))).await;
    let challenge = resp.headers()["www-authenticate"].to_str().unwrap().to_string();
    assert!(challenge.starts_with("Basic "), "{challenge}");
    assert_error(resp, 401, "invalid_client", "wrong basic secret").await;
    assert!(!redeemed(&pool, &code).await, "an unauthenticated request consumed the code");

    let resp = post(&base, &set(exchange(&client_id, &code), "client_id", None), Some((&client_id, SECRET))).await;
    assert_eq!(resp.status(), 200);
    let body = json(resp).await;
    let refresh_token = body["refresh_token"].as_str().unwrap().to_string();
    assert!(body["id_token"].is_string(), "{body}");

    let form = [
        ("grant_type", REFRESH_TOKEN),
        ("refresh_token", &refresh_token),
        ("client_id", &client_id),
        ("client_secret", SECRET),
    ];
    let resp = post(&base, &form, None).await;
    assert_eq!(resp.status(), 200, "a confidential client refreshing its own family");
    let rotated = json(resp).await;
    assert_ne!(rotated["refresh_token"], refresh_token.as_str());
    assert!(rotated.get("scope").is_none() && rotated.get("id_token").is_none(), "{rotated}");
    let op_path = svc.refresh(rotated["refresh_token"].as_str().unwrap().to_string()).await;
    assert_eq!(op_path.unwrap_err().status, opsapi::Status::Unauthorized, "the op path refreshed a confidential family");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// Basic credentials and a body secret, or a Basic id other than the body's, are two methods.
#[tokio::test(flavor = "multi_thread")]
async fn two_authentication_methods_are_an_invalid_request() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE], true).await;
    let (_, pid) = registered(&svc, "token-two").await;
    let code = issue(&svc, &client_id, &pid, "", None).await;

    let both = set(exchange(&client_id, &code), "client_secret", Some(SECRET));
    assert_error(post(&base, &both, Some((&client_id, SECRET))).await, 400, "invalid_request", "basic + post").await;
    let other_id = set(exchange(&client_id, &code), "client_id", Some("someone-else"));
    assert_error(post(&base, &other_id, Some((&client_id, SECRET))).await, 400, "invalid_request", "two ids").await;
    assert!(!redeemed(&pool, &code).await);
    let same_id = exchange(&client_id, &code);
    assert_eq!(post(&base, &same_id, Some((&client_id, SECRET))).await.status(), 200, "basic + its own client_id");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A public client refreshes by `client_id` alone, keeping `sub`; another client presenting its
/// refresh token, live or replayed outside the grace window, is `invalid_grant` and the family
/// survives.
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_rotates_for_its_own_client_only() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let other = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, "token-refresh").await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let first = json(post(&base, &exchange(&client_id, &code), None).await).await;
    let first_refresh = first["refresh_token"].as_str().unwrap().to_string();
    let family = code_family(&pool, &code).await;
    let stolen = post(&base, &refresh_form(&first_refresh, &other), None).await;
    assert_error(stolen, 400, "invalid_grant", "another client's live token").await;
    assert!(!refresh_used(&pool, &first_refresh).await, "a refused refresh consumed the token");

    let resp = post(&base, &refresh_form(&first_refresh, &client_id), None).await;
    assert_eq!(resp.status(), 200);
    let rotated = json(resp).await;
    assert_ne!(rotated["refresh_token"], first_refresh.as_str());
    let claims = svc.tokens.verify_access(rotated["access_token"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!((claims.sub.as_str(), claims.sid.as_str()), (pid.as_str(), family.as_str()));

    sqlx::query("UPDATE accounts.refresh_tokens SET used_at = now() - interval '60 seconds' WHERE token = $1")
        .bind(&first_refresh)
        .execute(&pool)
        .await
        .unwrap();
    let replay = post(&base, &refresh_form(&first_refresh, &other), None).await;
    assert_error(replay, 400, "invalid_grant", "another client's replay").await;
    assert_eq!(families(&pool, &family).await, 1, "another client's replay killed the family");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
    delete_client(&pool, &other).await;
}

/// An unknown grant is `unsupported_grant_type`; a known one the client does not list is
/// `unauthorized_client`; a body that is not a form, or repeats a parameter, `invalid_request`.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_and_unlisted_grants_and_malformed_bodies_are_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE], false).await;

    let form = [("grant_type", "password"), ("client_id", client_id.as_str())];
    assert_error(post(&base, &form, None).await, 400, "unsupported_grant_type", "password").await;
    let form = [("grant_type", REFRESH_TOKEN), ("refresh_token", "x"), ("client_id", client_id.as_str())];
    assert_error(post(&base, &form, None).await, 400, "unauthorized_client", "unlisted refresh").await;
    let form = [("grant_type", AUTHORIZATION_CODE), ("grant_type", AUTHORIZATION_CODE), ("client_id", client_id.as_str())];
    assert_error(post(&base, &form, None).await, 400, "invalid_request", "repeated parameter").await;
    let resp = client()
        .post(format!("{base}{TOKEN}"))
        .json(&serde_json::json!({ "grant_type": AUTHORIZATION_CODE, "client_id": client_id }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 400, "invalid_request", "a JSON body").await;
    let form = [("grant_type", AUTHORIZATION_CODE), ("client_id", "no-such-client")];
    assert_error(post(&base, &form, None).await, 401, "invalid_client", "unknown client").await;

    delete_client(&pool, &client_id).await;
}

/// A replayed code is `invalid_grant` and revokes its first redemption: a family revocation
/// is written, the family is deleted and its refresh token refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_code_revokes_its_first_redemption() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc.clone()).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, "token-replay").await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let first = json(post(&base, &exchange(&client_id, &code), None).await).await;
    let refresh_token = first["refresh_token"].as_str().unwrap().to_string();
    let family = code_family(&pool, &code).await;
    assert_eq!((families(&pool, &family).await, family_revocations(&pool, &family).await), (1, 0));

    assert_error(post(&base, &exchange(&client_id, &code), None).await, 400, "invalid_grant", "replay").await;
    assert_eq!(families(&pool, &family).await, 0, "the first redemption's family survived the replay");
    assert_eq!(family_revocations(&pool, &family).await, 1, "no family revocation written");
    let refused = post(&base, &refresh_form(&refresh_token, &client_id), None).await;
    assert_error(refused, 400, "invalid_grant", "the revoked family's refresh").await;

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A replay whose revocation takes the code family's lock before the first redemption's mint
/// makes that mint `invalid_grant`, and no family is created: the mint waits on the lock,
/// then finds the revocation.
#[tokio::test(flavor = "multi_thread")]
async fn a_replay_that_locks_first_prevents_the_mint() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, "token-race").await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let redeemed = svc.store.redeem_oidc_code(&pool, &code).await.unwrap().unwrap();
    let family = redeemed.family_id.clone();

    let mut replay = pool.begin().await.unwrap();
    let replay_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *replay).await.unwrap();
    svc.store.revoke_replayed_family_tx(&mut replay, &family).await.unwrap();
    let mint = tokio::spawn({
        let svc = svc.clone();
        async move { mint_code(&svc, &redeemed).await.map(|_| ()) }
    });
    assert!(blocked_query(&pool, replay_pid).await.is_some(), "the mint never waited on the replay's lock");
    replay.commit().await.unwrap();
    let minted = mint.await.unwrap();
    assert_eq!(minted.unwrap_err().status, opsapi::Status::Unauthorized, "the mint after the replay");
    assert_eq!(families(&pool, &family).await, 0, "a family minted after the replay's revocation");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// A password reset (or, with `revoke_all`, a Revoke all) holding the player row lock when a
/// redeemed code's mint takes its key share deletes the code: the mint waits on the lock, then
/// is `invalid_grant` and the player has no family.
async fn a_reset_before_the_mint_prevents_it(revoke_all: bool) {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE, REFRESH_TOKEN], false).await;
    let (_, pid) = registered(&svc, if revoke_all { "token-revoke-all" } else { "token-reset" }).await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let redeemed = svc.store.redeem_oidc_code(&pool, &code).await.unwrap().unwrap();

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
    let families: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE player_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(families, 0, "a family minted after the reset");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_reset_before_the_mint_prevents_it() {
    a_reset_before_the_mint_prevents_it(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoke_all_before_the_mint_prevents_it() {
    a_reset_before_the_mint_prevents_it(true).await;
}

/// A player deleted between the redemption and the mint is `invalid_grant`, never a 500.
#[tokio::test(flavor = "multi_thread")]
async fn a_player_deleted_after_the_redemption_is_an_invalid_grant() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[AUTHORIZATION_CODE], false).await;
    let (_, pid) = registered(&svc, "token-gone").await;
    let code = issue(&svc, &client_id, &pid, "openid", None).await;
    let redeemed = svc.store.redeem_oidc_code(&pool, &code).await.unwrap().unwrap();
    cleanup_player(&pool, &pid).await;

    let minted = mint_code(&svc, &redeemed).await;
    assert_eq!(minted.unwrap_err().status, opsapi::Status::Unauthorized);
    assert_eq!(families(&pool, &redeemed.family_id).await, 0);

    delete_client(&pool, &client_id).await;
}

/// A database outage is 503, never a client or grant error.
#[tokio::test]
async fn a_database_outage_is_503() {
    let svc = dead_service_at("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend", Arc::new(ArgonVerifier));
    let base = serve_default(svc).await;
    let form = [("grant_type", AUTHORIZATION_CODE), ("code", "c"), ("client_id", "any")];
    assert_error(post(&base, &form, None).await, 503, "temporarily_unavailable", "outage").await;
}

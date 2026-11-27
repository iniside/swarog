//! `/accounts/oidc/authorize` and `accounts.oidc_codes` against the live database. A child of
//! `tests` so it reuses that module's live-DB harness.

use std::collections::HashMap;

use accountsapi::grant_types::{AUTHORIZATION_CODE, PASSWORD};
use sqlx::PgConnection;

use super::hosted_login::{client, cookie_value, location, login_csrf, post_login, registered, serve_default, set_cookie};
use super::*;
use crate::codes::{NewCode, CODE_TTL_SECS, REPLAY_WINDOW_SECS};
use crate::web;
use crate::web::authorize::{AUTHORIZE, MAX_NONCE_BYTES, MAX_STATE_BYTES};

const CALLBACK: &str = "https://app.example/cb";
const QUERIED_CALLBACK: &str = "http://127.0.0.1:9/cb?x=1";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const STATE: &str = "s t/1&=?";

type Params = Vec<(String, String)>;

/// A public client of `grants` registering `redirect_uris` and the openid and profile scopes.
async fn code_client(pool: &PgPool, grants: &[&str], redirect_uris: &[&str]) -> String {
    let client_id = format!("t-{}", suffix());
    sqlx::query(
        "INSERT INTO accounts.oidc_clients \
           (client_id, display_name, grant_types, audience, redirect_uris, scopes) \
         VALUES ($1, 'authorize test client', $2, $3, $4, $5)",
    )
    .bind(&client_id)
    .bind(grants)
    .bind(accountsapi::SWAROG_AUDIENCE)
    .bind(redirect_uris)
    .bind(&["openid", "profile"][..])
    .execute(pool)
    .await
    .unwrap();
    client_id
}

/// A valid request of `client_id` returning to `redirect_uri`.
fn valid(client_id: &str, redirect_uri: &str) -> Params {
    [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("state", STATE),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// `params` with `name` replaced by `value`, or removed for `None`.
fn set(mut params: Params, name: &str, value: Option<&str>) -> Params {
    params.retain(|(k, _)| k != name);
    if let Some(value) = value {
        params.push((name.to_string(), value.to_string()));
    }
    params
}

async fn get(base: &str, params: &Params, session: Option<&str>) -> reqwest::Response {
    let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(params).finish();
    let mut req = client().get(format!("{base}{AUTHORIZE}?{query}"));
    if let Some(session) = session {
        req = req.header("Cookie", format!("{}={session}", web::SESSION_COOKIE));
    }
    req.send().await.unwrap()
}

fn query_of(location: &str) -> HashMap<String, String> {
    let url = url::Url::parse(location).or_else(|_| url::Url::parse(&format!("http://x{location}"))).unwrap();
    url.query_pairs().into_owned().collect()
}

/// Asserts a 303 back to `redirect_uri` with `error` and `state`.
async fn assert_error(resp: reqwest::Response, redirect_uri: &str, error: &str, state: Option<&str>, case: &str) {
    assert_eq!(resp.status(), 303, "{case}");
    let loc = location(&resp);
    assert!(loc.starts_with(&format!("{redirect_uri}?")), "{case}: {loc}");
    let q = query_of(&loc);
    assert_eq!(q.get("error").map(String::as_str), Some(error), "{case}: {loc}");
    assert_eq!(q.get("state").map(String::as_str), state, "{case}: {loc}");
    assert!(!q.contains_key("code"), "{case}: {loc}");
}

fn new_code(client_id: &str) -> NewCode<'_> {
    NewCode { client_id, redirect_uri: CALLBACK, code_challenge: CHALLENGE, scope: "", nonce: None }
}

/// A code issued to `pid` for `client_id` under a fresh browser session.
async fn issue(svc: &Service, client_id: &str, pid: &str) -> String {
    let session = svc.store.create_browser_session(pid).await.unwrap().unwrap();
    let code = store::new_token();
    let digest = web::session::digest(&session.token);
    assert!(svc.store.insert_oidc_code(&code, &digest, &new_code(client_id)).await.unwrap());
    code
}

/// Moves `code`'s issue time to `secs` before the caller's transaction's `now()`.
async fn age(conn: &mut PgConnection, code: &str, secs: f64) {
    sqlx::query(
        "UPDATE accounts.oidc_codes SET created_at = now() - make_interval(secs => $2) \
          WHERE code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(code)
    .bind(secs)
    .execute(&mut *conn)
    .await
    .unwrap();
}

async fn code_rows(conn: &mut PgConnection, code: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.oidc_codes WHERE code_digest = sha256(convert_to($1, 'UTF8'))")
        .bind(code)
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

async fn codes_of(pool: &PgPool, pid: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.oidc_codes WHERE player_id = $1::uuid")
        .bind(pid)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_client_or_redirect_uri_renders_a_page_and_never_redirects() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let base = serve_default(svc).await;

    let mut repeated = valid(&client_id, CALLBACK);
    repeated.push(("redirect_uri".to_string(), CALLBACK.to_string()));
    let cases = [
        valid("no-such-client", CALLBACK),
        valid(&client_id, "https://app.example/cb/"),
        valid(&client_id, "https://APP.example/cb"),
        set(valid(&client_id, CALLBACK), "client_id", None),
        set(valid(&client_id, CALLBACK), "redirect_uri", None),
        repeated,
    ];
    for params in &cases {
        let resp = get(&base, params, None).await;
        assert_eq!(resp.status(), 400, "{params:?}");
        assert!(resp.headers().get("location").is_none(), "{params:?}");
        assert!(resp.text().await.unwrap().contains("Sign-in error"), "{params:?}");
    }

    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_redirect_error_carries_state() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let no_code = code_client(&pool, &[PASSWORD], &[CALLBACK]).await;
    let base = serve_default(svc).await;

    let ok = valid(&client_id, CALLBACK);
    let cases = [
        ("no authorization_code grant", valid(&no_code, CALLBACK), "unauthorized_client"),
        ("response_type token", set(ok.clone(), "response_type", Some("token")), "unsupported_response_type"),
        ("no response_type", set(ok.clone(), "response_type", None), "invalid_request"),
        ("no code_challenge", set(ok.clone(), "code_challenge", None), "invalid_request"),
        ("no method", set(ok.clone(), "code_challenge_method", None), "invalid_request"),
        ("plain method", set(ok.clone(), "code_challenge_method", Some("plain")), "invalid_request"),
        ("non-base64url challenge", set(ok.clone(), "code_challenge", Some(&"=".repeat(43))), "invalid_request"),
        ("unregistered scope", set(ok.clone(), "scope", Some("openid offline_access")), "invalid_scope"),
        ("prompt none, no session", set(ok.clone(), "prompt", Some("none")), "login_required"),
    ];
    for (case, params, error) in cases {
        assert_error(get(&base, &params, None).await, CALLBACK, error, Some(STATE), case).await;
    }

    delete_client(&pool, &client_id).await;
    delete_client(&pool, &no_code).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_or_repeated_parameters_are_invalid_request() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let long_callback = format!("https://app.example/{}", "a/".repeat(240));
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK, &long_callback]).await;
    let base = serve_default(svc).await;

    let ok = valid(&client_id, CALLBACK);
    let mut repeated = ok.clone();
    repeated.push(("nonce".to_string(), "a".to_string()));
    repeated.push(("nonce".to_string(), "b".to_string()));
    let mut two_states = ok.clone();
    two_states.push(("state".to_string(), "other".to_string()));
    let long_state = "s".repeat(MAX_STATE_BYTES + 1);
    let long_nonce = "n".repeat(MAX_NONCE_BYTES + 1);
    let cases = [
        ("state over the cap", set(ok.clone(), "state", Some(&long_state)), None),
        ("repeated state", two_states, None),
        ("repeated nonce", repeated, Some(STATE)),
        ("nonce over the cap", set(ok.clone(), "nonce", Some(&long_nonce)), Some(STATE)),
        ("challenge of 42", set(ok.clone(), "code_challenge", Some(&CHALLENGE[..42])), Some(STATE)),
        ("challenge of 44", set(ok.clone(), "code_challenge", Some(&format!("{CHALLENGE}A"))), Some(STATE)),
    ];
    for (case, params, state) in cases {
        assert_error(get(&base, &params, None).await, CALLBACK, "invalid_request", state, case).await;
    }

    // At the caps the request is valid: no session sends it to the login.
    let at_caps = set(
        set(valid(&client_id, CALLBACK), "state", Some(&"s".repeat(MAX_STATE_BYTES))),
        "nonce",
        Some(&"n".repeat(MAX_NONCE_BYTES)),
    );
    let resp = get(&base, &at_caps, None).await;
    assert_eq!(resp.status(), 303);
    assert!(location(&resp).starts_with("/accounts/oidc/login?return_to="));

    // A login return URL over MAX_RETURN_TO_BYTES: every slash of these values is encoded.
    let slashes = "/".repeat(MAX_STATE_BYTES);
    let long = set(
        set(valid(&client_id, &long_callback), "state", Some(&slashes)),
        "nonce",
        Some(&slashes[..MAX_NONCE_BYTES]),
    );
    let resp = get(&base, &long, None).await;
    assert_error(resp, &long_callback, "invalid_request", Some(&slashes), "return URL over the cap").await;
    let resp = get(&base, &set(long, "nonce", None), None).await;
    assert!(location(&resp).starts_with("/accounts/oidc/login?return_to="), "the same request under the cap");

    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_session_login_returns_to_the_same_request() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let (email, pid) = registered(&svc, "authorize").await;
    let base = serve_default(svc.clone()).await;

    let raw = format!(
        "{base}{AUTHORIZE}?response_type=code&client_id={client_id}&redirect_uri=https://app.example/cb\
         &scope=openid&state=xyz&code_challenge={CHALLENGE}&code_challenge_method=S256"
    );
    let resp = client().get(&raw).send().await.unwrap();
    assert_eq!(resp.status(), 303);
    let to_login = location(&resp);
    assert!(to_login.starts_with("/accounts/oidc/login?return_to="), "{to_login}");
    let return_to = query_of(&to_login)["return_to"].clone();
    assert_eq!(web::safe_return_to(Some(&return_to)), return_to, "the login would drop the request");

    let csrf = login_csrf(&base).await;
    let resp = post_login(&base, &csrf, &email, "pw", &return_to).await;
    assert_eq!(resp.status(), 303);
    assert_eq!(location(&resp), return_to);
    let session = cookie_value(&set_cookie(&resp, web::SESSION_COOKIE));

    let resp = client()
        .get(format!("{base}{return_to}"))
        .header("Cookie", format!("{}={session}", web::SESSION_COOKIE))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    let back = location(&resp);
    assert!(back.starts_with(&format!("{CALLBACK}?code=")), "{back}");
    let q = query_of(&back);
    assert_eq!(q["state"], "xyz");
    let redeemed = svc.store.redeem_oidc_code(&pool, &q["code"]).await.unwrap().unwrap();
    assert_eq!(
        (redeemed.client_id.as_str(), redeemed.player_id.as_str(), redeemed.redirect_uri.as_str(), redeemed.scope.as_str()),
        (client_id.as_str(), pid.as_str(), CALLBACK, "openid"),
    );

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_issues_a_code_bound_to_the_client_redirect_challenge_nonce_and_auth_time() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK, QUERIED_CALLBACK]).await;
    let (_, pid) = registered(&svc, "bound").await;
    let session = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let signed_in: i64 = sqlx::query_scalar(
        "SELECT floor(EXTRACT(EPOCH FROM created_at))::int8 FROM accounts.browser_sessions WHERE token_digest = $1",
    )
    .bind(web::session::digest(&session.token))
    .fetch_one(&pool)
    .await
    .unwrap();
    let base = serve_default(svc.clone()).await;

    let params = set(set(valid(&client_id, QUERIED_CALLBACK), "scope", Some("profile openid openid")), "nonce", Some("n-1"));
    let resp = get(&base, &params, Some(&session.token)).await;
    assert_eq!(resp.status(), 303);
    let back = location(&resp);
    assert!(back.starts_with(&format!("{QUERIED_CALLBACK}&code=")), "{back}");
    let q = query_of(&back);
    assert_eq!((q["x"].as_str(), q["state"].as_str()), ("1", STATE));
    let code = &q["code"];
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(code_rows(&mut conn, code).await, 1, "the row is keyed by the code's digest");

    let r = svc.store.redeem_oidc_code(&pool, code).await.unwrap().unwrap();
    assert_eq!(
        (r.client_id.as_str(), r.player_id.as_str(), r.redirect_uri.as_str(), r.code_challenge.as_str()),
        (client_id.as_str(), pid.as_str(), QUERIED_CALLBACK, CHALLENGE),
    );
    assert_eq!((r.scope.as_str(), r.nonce.as_deref(), r.auth_time), ("openid profile", Some("n-1"), signed_in));
    assert!(store::is_canonical_uuid(&r.family_id));

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_code_redeems_once_and_not_once_its_ttl_has_passed() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let (_, pid) = registered(&svc, "ttl").await;
    let (once, fresh, stale) =
        (issue(&svc, &client_id, &pid).await, issue(&svc, &client_id, &pid).await, issue(&svc, &client_id, &pid).await);

    assert!(svc.store.redeem_oidc_code(&pool, &once).await.unwrap().is_some());
    assert!(svc.store.redeem_oidc_code(&pool, &once).await.unwrap().is_none(), "a code redeemed twice");

    // One transaction, so every `now()` below is the same instant.
    let mut tx = pool.begin().await.unwrap();
    age(&mut tx, &fresh, f64::from(CODE_TTL_SECS) - 0.001).await;
    age(&mut tx, &stale, f64::from(CODE_TTL_SECS)).await;
    assert!(svc.store.redeem_oidc_code(&mut *tx, &fresh).await.unwrap().is_some(), "inside the TTL");
    assert!(svc.store.redeem_oidc_code(&mut *tx, &stale).await.unwrap().is_none(), "at the TTL");
    tx.rollback().await.unwrap();

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redeemed_code_is_kept_for_the_replay_window_and_pruned_after() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let (_, pid) = registered(&svc, "replay").await;
    let code = issue(&svc, &client_id, &pid).await;
    assert!(svc.store.redeem_oidc_code(&pool, &code).await.unwrap().is_some());

    let mut tx = pool.begin().await.unwrap();
    age(&mut tx, &code, f64::from(REPLAY_WINDOW_SECS) - 0.001).await;
    svc.store.prune_oidc_codes(&mut *tx).await.unwrap();
    assert_eq!(code_rows(&mut tx, &code).await, 1, "pruned inside the replay window");
    age(&mut tx, &code, f64::from(REPLAY_WINDOW_SECS)).await;
    svc.store.prune_oidc_codes(&mut *tx).await.unwrap();
    assert_eq!(code_rows(&mut tx, &code).await, 0, "kept past the replay window");
    tx.rollback().await.unwrap();

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_reset_and_revoke_all_delete_the_players_codes() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let (_, pid) = registered(&svc, "reset").await;

    issue(&svc, &client_id, &pid).await;
    svc.reset_password(&pid).await.unwrap();
    assert_eq!(codes_of(&pool, &pid).await, 0, "a password reset left the player's codes");

    issue(&svc, &client_id, &pid).await;
    let mut tx = pool.begin().await.unwrap();
    assert!(svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap().is_some());
    tx.commit().await.unwrap();
    assert_eq!(codes_of(&pool, &pid).await, 0, "Revoke all left the player's codes");

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// Ending a browser session (a logout) ends the unredeemed code issued under it.
#[tokio::test(flavor = "multi_thread")]
async fn a_logout_removes_the_sessions_unredeemed_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;
    let (_, pid) = registered(&svc, "logout").await;

    let session = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let digest = web::session::digest(&session.token);
    let code = store::new_token();
    assert!(svc.store.insert_oidc_code(&code, &digest, &new_code(&client_id)).await.unwrap());
    let mut conn = pool.acquire().await.unwrap();
    assert!(svc.store.delete_browser_session_digest_tx(&mut conn, &digest).await.unwrap());
    assert_eq!(code_rows(&mut conn, &code).await, 0, "a logout left the session's code");
    drop(conn);

    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client_id).await;
}

/// The insert's foreign-key checks fire in trigger-name order, which is the order the
/// constraints were created in: the player before the session, the order a reset locks
/// them, so an insert and a reset cannot deadlock over the two rows.
#[tokio::test]
async fn a_code_insert_checks_the_player_before_the_session() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let order: Vec<String> = sqlx::query_scalar(
        "SELECT p.relname::text FROM pg_trigger t \
           JOIN pg_constraint c ON c.oid = t.tgconstraint \
           JOIN pg_class p ON p.oid = c.confrelid \
          WHERE t.tgrelid = 'accounts.oidc_codes'::regclass AND c.contype = 'f' \
            AND t.tgname LIKE 'RI\\_ConstraintTrigger\\_c\\_%' AND (t.tgtype & 4) <> 0 \
          ORDER BY t.tgname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(order, ["oidc_clients", "players", "browser_sessions"]);
}

/// Both orders of a reset's session delete and a code insert under that session. Insert
/// behind the reset: it waits on the reset's player lock, then finds the session gone and
/// answers signed out, leaving no code. Insert first: the session delete cascades to the
/// code before the reset's own code delete runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_reset_racing_a_code_insert_leaves_no_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;

    let (_, pid) = registered(&svc, "reset-first").await;
    let session = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let digest = web::session::digest(&session.token);
    let mut reset = pool.begin().await.unwrap();
    let reset_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *reset).await.unwrap();
    assert!(svc.store.lock_player_row_tx(&mut reset, &pid).await.unwrap());
    assert!(svc.store.delete_player_browser_sessions_tx(&mut reset, &pid).await.unwrap() > 0);
    let insert = tokio::spawn({
        let (svc, client_id) = (svc.clone(), client_id.clone());
        async move { svc.store.insert_oidc_code(&store::new_token(), &digest, &new_code(&client_id)).await }
    });
    assert!(blocked_query(&pool, reset_pid).await.is_some(), "the insert never waited on the reset");
    reset.commit().await.unwrap();
    assert!(!insert.await.unwrap().unwrap(), "a code was issued under a session the reset ended");
    assert_eq!(codes_of(&pool, &pid).await, 0, "a code outlived the reset");
    cleanup_player(&pool, &pid).await;

    let (_, pid) = registered(&svc, "insert-first").await;
    let code = issue(&svc, &client_id, &pid).await;
    let mut reset = pool.begin().await.unwrap();
    assert!(svc.store.lock_player_row_tx(&mut reset, &pid).await.unwrap());
    assert!(svc.store.delete_player_browser_sessions_tx(&mut reset, &pid).await.unwrap() > 0);
    assert_eq!(code_rows(&mut reset, &code).await, 0, "the session delete left its code");
    reset.commit().await.unwrap();
    cleanup_player(&pool, &pid).await;

    delete_client(&pool, &client_id).await;
}

/// Migrate refuses an `oidc_codes` without `session_digest`, naming the drop remedy; the
/// booted table passes.
#[tokio::test]
async fn boot_refusal_fires_on_a_codes_table_without_a_session_digest() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let schema = format!("accounts_stalecodes_{}_{nanos}", std::process::id());
    for sql in [
        format!("CREATE SCHEMA {schema}"),
        format!("CREATE TABLE {schema}.oidc_codes (code_digest bytea PRIMARY KEY)"),
    ] {
        sqlx::query(&sql).execute(&pool).await.unwrap();
    }
    let refused = crate::refuse_stale_codes(&pool, &schema).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&pool).await.unwrap();

    let err = refused.expect_err("a codes table without session_digest must refuse boot").to_string();
    assert!(err.contains(&format!("{schema}.oidc_codes has no session_digest column")), "{err}");
    assert!(err.contains(&format!("DROP SCHEMA {schema} CASCADE;")), "{err}");
    crate::refuse_stale_codes(&pool, "accounts").await.unwrap();
}

/// Both orders of a player delete and a redemption, each waiting on the other's row lock:
/// neither deadlocks, and a redemption behind the delete finds no code.
#[tokio::test(flavor = "multi_thread")]
async fn a_player_delete_during_redemption_ends_the_code_without_a_deadlock() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = code_client(&pool, &[AUTHORIZATION_CODE], &[CALLBACK]).await;

    let (_, pid) = registered(&svc, "del-first").await;
    let code = issue(&svc, &client_id, &pid).await;
    let mut deleter = pool.begin().await.unwrap();
    let deleter_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *deleter).await.unwrap();
    assert!(svc.store.delete_player_tx(&mut deleter, &pid).await.unwrap());
    let redeem = tokio::spawn({
        let svc = svc.clone();
        async move { svc.store.redeem_oidc_code(&svc.store.pool, &code).await }
    });
    assert!(blocked_query(&pool, deleter_pid).await.is_some(), "the redemption never waited on the delete");
    deleter.commit().await.unwrap();
    assert!(redeem.await.unwrap().unwrap().is_none(), "a deleted player's code redeemed");

    let (_, pid) = registered(&svc, "redeem-first").await;
    let code = issue(&svc, &client_id, &pid).await;
    let mut redeemer = pool.begin().await.unwrap();
    let redeemer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *redeemer).await.unwrap();
    assert!(svc.store.redeem_oidc_code(&mut *redeemer, &code).await.unwrap().is_some());
    let delete = tokio::spawn({
        let (svc, pid) = (svc.clone(), pid.clone());
        async move {
            let mut tx = svc.store.pool.begin().await.unwrap();
            let deleted = svc.store.delete_player_tx(&mut tx, &pid).await.unwrap();
            tx.commit().await.unwrap();
            deleted
        }
    });
    assert!(blocked_query(&pool, redeemer_pid).await.is_some(), "the delete never waited on the redemption");
    redeemer.commit().await.unwrap();
    assert!(delete.await.unwrap());
    assert_eq!(codes_of(&pool, &pid).await, 0);

    delete_client(&pool, &client_id).await;
}

//! Epic on the hosted sign-in page: the form's `browser` state and the callback that
//! signs the browser in, against a mock Epic and the live database. A child of `tests`
//! so it reuses that module's live-DB and mock-IdP helpers.

use super::hosted_login::{client, cookie_value, has_attr, location, serve_default, set_cookie};
use super::*;
use crate::epic_oauth::EpicOAuth;
use crate::oauth_flow::Intent;
use crate::web;

const KID: &str = "k1";
const CLIENT_ID: &str = "client-xyz";
const ISSUER: &str = "https://eas.example";
const BINDING_COOKIE: &str = "epic_oauth_binding";

/// The Epic flow against a local JWKS and a token endpoint at `token_url`.
fn flow(pool: &PgPool, jwks_url: &str, token_url: &str) -> Arc<EpicOAuth> {
    let verifier = Arc::new(
        OidcVerifier::new(jwks_url, IssuerMatch::prefix("issuer", ISSUER).unwrap(), vec![CLIENT_ID.to_string()])
            .unwrap(),
    );
    Arc::new(
        EpicOAuth::new(
            CLIENT_ID.into(),
            oauth_config("http://localhost/accounts/epic/callback", token_url),
            verifier,
            pool.clone(),
            false,
        )
        .unwrap(),
    )
}

/// A mock Epic whose token endpoint answers every code with an id_token for `subject`.
async fn mock_epic(pool: &PgPool, subject: &str) -> Arc<EpicOAuth> {
    let (enc, jwks) = test_key(KID);
    let jwks_url = serve_jwks(jwks).await;
    let id_token = sign(
        &enc,
        KID,
        serde_json::json!({"iss": format!("{ISSUER}/x"), "aud": CLIENT_ID, "sub": subject, "exp": future_exp()}),
    );
    let body = serde_json::json!({ "id_token": id_token }).to_string();
    let token_app = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let body = body.clone();
            async move { ([(axum::http::header::CONTENT_TYPE, "application/json")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, token_app).await.unwrap();
    });
    flow(pool, &jwks_url, &token_url)
}

/// The hosted pages with Epic and the Epic routes on one listener, as `init` mounts them.
async fn serve(svc: Arc<Service>, oauth: Arc<EpicOAuth>) -> String {
    serve_limited(svc, oauth, httpmw::IpLimiter::new("accounts_epic_browser_test", 5.0, 20)).await
}

/// `serve` with the given sign-in bucket, shared by the hosted form and the Epic start as in `init`.
async fn serve_limited(svc: Arc<Service>, oauth: Arc<EpicOAuth>, limiter: Arc<httpmw::IpLimiter>) -> String {
    let limiter = web::LoginLimiter::new(limiter, Vec::new());
    let flows = web::Flows { epic: Some(oauth), google: None };
    let app = web::hosted_browser_router(svc, false, limiter, flows).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// Submits the "Continue with Epic" form, optionally from a browser holding `session`.
async fn post_epic(base: &str, csrf: &str, session: Option<&str>, return_to: &str) -> reqwest::Response {
    let mut cookies = format!("{}={csrf}", web::LOGIN_CSRF_COOKIE);
    if let Some(session) = session {
        cookies.push_str(&format!("; {}={session}", web::SESSION_COOKIE));
    }
    client()
        .post(format!("{base}/accounts/oidc/epic"))
        .header("Cookie", cookies)
        .form(&[("csrf", csrf), ("return_to", return_to)])
        .send()
        .await
        .unwrap()
}

/// The `state` of a form start's authorize redirect.
fn form_state(start: &reqwest::Response) -> String {
    state_of(&location(start))
}

fn state_of(authorize: &str) -> String {
    url::Url::parse(authorize)
        .unwrap()
        .query_pairs()
        .find_map(|(k, v)| (k == "state").then(|| v.into_owned()))
        .expect("state in the authorize url")
}

/// Follows a start response to the callback the way Epic sends the browser back.
async fn callback(base: &str, start: &reqwest::Response) -> reqwest::Response {
    let state = form_state(start);
    let binding = cookie_value(&set_cookie(start, BINDING_COOKIE));
    client()
        .get(format!("{base}/accounts/epic/callback?code=abc&state={state}"))
        .header("Cookie", format!("{BINDING_COOKIE}={binding}"))
        .send()
        .await
        .unwrap()
}

async fn epic_player(pool: &PgPool, subject: &str) -> Option<String> {
    sqlx::query_scalar("SELECT player_id::text FROM accounts.identities WHERE provider = 'epic' AND subject = $1")
        .bind(subject)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// Player bearer sessions and refresh tokens the player holds.
async fn player_tokens(pool: &PgPool, player_id: &str) -> (i64, i64) {
    let sessions = sqlx::query_scalar("SELECT count(*) FROM accounts.sessions WHERE player_id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let refresh = sqlx::query_scalar("SELECT count(*) FROM accounts.refresh_tokens WHERE player_id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (sessions, refresh)
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_intent_sets_swarog_session_and_issues_no_player_session() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let subject = format!("epic-browser-{}", suffix());
    let base = serve(svc.clone(), mock_epic(&pool, &subject).await).await;
    let other = svc
        .register(format!("epic-browser-{}@test.local", suffix().to_ascii_lowercase()), "pw".into(), "Other".into())
        .await
        .unwrap()
        .player_id;
    let held = svc.store.create_browser_session(&other).await.unwrap().unwrap().token;

    let form = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap();
    let csrf = cookie_value(&set_cookie(&form, web::LOGIN_CSRF_COOKIE));
    let form_body = form.text().await.unwrap();
    let return_to = "/accounts/oidc/authorize?client_id=lore&state=abc";
    let start = post_epic(&base, &csrf, Some(&held), return_to).await;
    let start_status = start.status();
    let authorize = location(&start);
    let binding_cookie = set_cookie(&start, BINDING_COOKIE);
    let first = callback(&base, &start).await;
    let first_status = first.status();
    let first_to = location(&first);
    let session_cookie = set_cookie(&first, web::SESSION_COOKIE);
    let csrf_cleared = set_cookie(&first, web::LOGIN_CSRF_COOKIE);
    let token = cookie_value(&session_cookie);
    let player = epic_player(&pool, &subject).await.expect("the callback created the epic player");
    let live = svc.store.browser_session(&token).await.unwrap().map(|s| s.player_id);
    let held_after = svc.store.browser_session(&held).await.unwrap().is_some();
    let as_bearer = svc.verify_session(token.clone()).await.unwrap();
    let tokens_after_first = player_tokens(&pool, &player).await;

    // A second Epic sign-in from the now signed-in browser resolves the same player and
    // replaces the browser's session.
    let again = callback(&base, &post_epic(&base, &csrf, Some(&token), web::LANDING).await).await;
    let again_to = location(&again);
    let second = cookie_value(&set_cookie(&again, web::SESSION_COOKIE));
    let second_player = svc.store.browser_session(&second).await.unwrap().map(|s| s.player_id);
    let first_after = svc.store.browser_session(&token).await.unwrap().is_some();
    let tokens_after_second = player_tokens(&pool, &player).await;
    let registered = registered_events(&pool, &player).await;
    cleanup_player(&pool, &player).await;
    cleanup_player(&pool, &other).await;

    assert!(form_body.contains("Continue with Epic") && form_body.contains(r#"action="/accounts/oidc/epic""#), "{form_body}");
    assert_eq!(start_status, 303);
    assert!(authorize.starts_with("http://localhost/authorize?"), "{authorize}");
    assert!(has_attr(&binding_cookie, "Path=/accounts"), "{binding_cookie}");
    assert_eq!(first_status, 303);
    assert_eq!(first_to, return_to, "the callback follows the stored return_to");
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/accounts/oidc", "Max-Age=43200"] {
        assert!(has_attr(&session_cookie, attr), "{attr}: {session_cookie}");
    }
    assert!(has_attr(&csrf_cleared, "Max-Age=0"), "{csrf_cleared}");
    assert_eq!(live.as_deref(), Some(player.as_str()));
    assert!(!held_after, "the session the browser held at start ends at sign-in");
    assert!(as_bearer.is_none(), "a browser session token is not a player bearer");
    assert_eq!(tokens_after_first, (0, 0), "no player session or refresh token for a browser sign-in");
    assert_eq!(again_to, web::LANDING);
    assert_eq!(second_player.as_deref(), Some(player.as_str()), "the second sign-in resolves the same player");
    assert!(!first_after, "the second sign-in ends the first browser session");
    assert_eq!(tokens_after_second, (0, 0));
    assert_eq!(registered, 1, "one player.registered for the created player");
}

/// A form start reuses the binding a JSON start (or an earlier form start) set, so every
/// state this browser started stays redeemable with the one cookie it now holds.
#[tokio::test(flavor = "multi_thread")]
async fn form_starts_reuse_the_browsers_binding_so_parallel_states_redeem() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let oauth = mock_epic(&pool, &format!("epic-parallel-{}", suffix())).await;
    let base = serve(svc, oauth.clone()).await;
    let csrf = crate::store::new_token();

    let json = client().post(format!("{base}/accounts/epic/start")).send().await.unwrap();
    let json_cookie = set_cookie(&json, BINDING_COOKIE);
    let binding = cookie_value(&json_cookie);
    let body: serde_json::Value = json.json().await.unwrap();
    let json_state = state_of(body["authorize_url"].as_str().unwrap());
    let form_start = |extra: String| {
        let (base, csrf) = (base.clone(), csrf.clone());
        async move {
            client()
                .post(format!("{base}/accounts/oidc/epic"))
                .header("Cookie", format!("{}={csrf}; {extra}", web::LOGIN_CSRF_COOKIE))
                .form(&[("csrf", csrf.as_str()), ("return_to", "")])
                .send()
                .await
                .unwrap()
        }
    };
    let first = form_start(format!("{BINDING_COOKIE}={binding}")).await;
    let first_binding = cookie_value(&set_cookie(&first, BINDING_COOKIE));
    let first_state = form_state(&first);
    let second = form_start(format!("{BINDING_COOKIE}={first_binding}")).await;
    let held = cookie_value(&set_cookie(&second, BINDING_COOKIE));
    let second_state = form_state(&second);

    assert_eq!((first.status().as_u16(), second.status().as_u16()), (303, 303));
    assert!(has_attr(&json_cookie, "Path=/accounts"), "{json_cookie}");
    assert_eq!(first_binding, binding, "the form start reuses the JSON start's binding");
    assert_eq!(held, binding, "a second form start reuses it too");
    assert_eq!(oauth.take_state(&json_state, Some(&held)).await.unwrap(), Some(Intent::Login));
    for state in [first_state, second_state] {
        let intent = oauth.take_state(&state, Some(&held)).await.unwrap();
        assert!(matches!(intent, Some(Intent::Browser { .. })), "{intent:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn login_intent_still_hands_a_player_session_to_the_demo_page() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let subject = format!("epic-login-{}", suffix());
    let oauth = mock_epic(&pool, &subject).await;
    let binding = store::new_token();
    let state = oauth.new_state(Intent::Login, binding.clone()).await.unwrap();
    let base = serve(svc.clone(), oauth).await;
    let resp = client()
        .get(format!("{base}/accounts/epic/callback?code=abc&state={state}"))
        .header("Cookie", format!("{BINDING_COOKIE}={binding}"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let to = location(&resp);
    let sets_session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap().starts_with(web::SESSION_COOKIE));
    let player = epic_player(&pool, &subject).await.expect("the callback created the epic player");
    let tokens = player_tokens(&pool, &player).await;
    cleanup_player(&pool, &player).await;

    assert_eq!(status, 303);
    assert!(to.starts_with("/#token=") && to.contains("&refresh="), "{to}");
    assert!(!sets_session, "a login intent mints no browser session");
    assert_eq!(tokens, (1, 1), "a login intent issues one player session and refresh token");
}

#[tokio::test(flavor = "multi_thread")]
async fn epic_form_requires_the_login_csrf_and_writes_no_state_without_it() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve(svc.clone(), mock_epic(&pool, "unused").await).await;
    let return_to = format!("/accounts/oidc/?t={}", suffix());
    let forged = client()
        .post(format!("{base}/accounts/oidc/epic"))
        .form(&[("csrf", store::new_token().as_str()), ("return_to", return_to.as_str())])
        .send()
        .await
        .unwrap()
        .status();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts.oauth_states WHERE return_to = $1")
        .bind(&return_to)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(forged, 403);
    assert_eq!(rows, 0, "a refused form mints no state");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_epic_the_page_has_no_epic_button_and_the_form_route_is_404() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc).await;
    let body = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap().text().await.unwrap();
    let post = client()
        .post(format!("{base}/accounts/oidc/epic"))
        .form(&[("csrf", ""), ("return_to", "")])
        .send()
        .await
        .unwrap()
        .status();

    assert!(!body.contains("Continue with Epic"), "{body}");
    assert_eq!(post, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_browser_callback_returns_to_the_sign_in_page_with_an_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    // Nothing listens on the discard port, so the code exchange fails.
    let oauth = flow(&pool, "http://127.0.0.1:9/jwks", "http://127.0.0.1:9/token");
    let binding = store::new_token();
    let intent = Intent::Browser { return_to: "/accounts/oidc/?a=b".into(), replaces: None };
    let state = oauth.new_state(intent, binding.clone()).await.unwrap();
    let base = serve(svc, oauth).await;
    let resp = client()
        .get(format!("{base}/accounts/epic/callback?code=abc&state={state}"))
        .header("Cookie", format!("{BINDING_COOKIE}={binding}"))
        .send()
        .await
        .unwrap();
    let to = location(&resp);
    let page = client().get(format!("{base}{to}")).send().await.unwrap().text().await.unwrap();

    assert_eq!(resp.status(), 303);
    assert_eq!(to, "/accounts/oidc/login?epic=error&return_to=%2Faccounts%2Foidc%2F%3Fa%3Db");
    assert!(page.contains("Sign-in with Epic failed."), "{page}");
}

/// One token: the first request is admitted, the next is refused.
fn one_token_bucket() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new("accounts_epic_browser_test", 0.001, 1)
}

/// The JSON Epic start draws from the sign-in bucket: an admitted start empties it, so the
/// next start and the hosted form both get 429.
#[tokio::test(flavor = "multi_thread")]
async fn the_json_epic_start_is_rate_limited_with_the_login_bucket() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let oauth = mock_epic(&pool, &format!("epic-limited-{}", suffix())).await;
    let base = serve_limited(svc, oauth, one_token_bucket()).await;
    let start = || client().post(format!("{base}/accounts/epic/start")).send();

    let first = start().await.unwrap();
    let second = start().await.unwrap();
    let retry_after = second.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string());
    let form = client()
        .post(format!("{base}/accounts/oidc/login"))
        .form(&[("email", "a@x.io"), ("password", "pw"), ("csrf", "")])
        .send()
        .await
        .unwrap();

    assert_eq!(first.status(), 200, "the first start is admitted");
    assert_eq!(second.status(), 429, "the bucket is empty");
    assert_eq!(retry_after.as_deref(), Some("1"));
    assert_eq!(form.status(), 429, "the hosted form shares the bucket the start emptied");
}

/// A hosted form POST that empties the bucket limits the JSON Epic start to a 429.
#[tokio::test(flavor = "multi_thread")]
async fn a_hosted_form_post_drains_the_json_epic_start_bucket() {
    let svc = lazy_service();
    let oauth = flow(&svc.store.pool, "http://127.0.0.1:9/jwks", "http://127.0.0.1:9/token");
    let base = serve_limited(svc, oauth, one_token_bucket()).await;

    let form = client()
        .post(format!("{base}/accounts/oidc/login"))
        .form(&[("email", "a@x.io"), ("password", "pw"), ("csrf", "")])
        .send()
        .await
        .unwrap();
    let start = client()
        .post(format!("{base}/accounts/epic/start"))
        .header("Authorization", "Bearer some-session")
        .send()
        .await
        .unwrap();
    let retry_after = start.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string());

    assert_eq!(form.status(), 403, "admitted, then refused for the missing login-CSRF");
    assert_eq!(start.status(), 429, "the start shares the bucket the form emptied");
    assert_eq!(retry_after.as_deref(), Some("1"));
}

/// A federated sign-in whose player is deleted while the mint waits on the player row
/// mints nothing and keeps the session the browser held.
#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_refused_at_the_mint_keeps_the_browsers_session() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let subject = format!("epic-refused-{}", suffix());
    let base = serve(svc.clone(), mock_epic(&pool, &subject).await).await;
    let (player, _) = svc.external_browser_login(crate::providers::EPIC, &subject, "Refused").await.unwrap();
    let other = svc
        .register(format!("epic-refused-{}@test.local", suffix().to_ascii_lowercase()), "pw".into(), "Other".into())
        .await
        .unwrap()
        .player_id;
    let held = svc.store.create_browser_session(&other).await.unwrap().unwrap().token;
    let form = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap();
    let csrf = cookie_value(&set_cookie(&form, web::LOGIN_CSRF_COOKIE));
    let start = post_epic(&base, &csrf, Some(&held), web::LANDING).await;
    let mut gate = pool.begin().await.unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *gate).await.unwrap();
    sqlx::query("SELECT 1 FROM accounts.players WHERE id = $1::uuid FOR UPDATE")
        .bind(&player.id)
        .execute(&mut *gate)
        .await
        .unwrap();
    let signed_in = {
        let base = base.clone();
        tokio::spawn(async move { callback(&base, &start).await })
    };
    let parked = blocked_query(&pool, gate_pid).await;
    sqlx::query("DELETE FROM accounts.players WHERE id = $1::uuid")
        .bind(&player.id)
        .execute(&mut *gate)
        .await
        .unwrap();
    gate.commit().await.unwrap();
    let resp = signed_in.await.unwrap();
    let status = resp.status();
    let to = location(&resp);
    let minted = resp.headers().get_all("set-cookie").iter().any(|v| {
        v.to_str().unwrap().starts_with(&format!("{}=", web::SESSION_COOKIE))
    });
    let held_after = svc.store.browser_session(&held).await.unwrap().map(|s| s.player_id);
    cleanup_player(&pool, &player.id).await;
    cleanup_player(&pool, &other).await;

    assert!(
        parked.as_deref().is_some_and(|q| q.contains("accounts.browser_sessions")),
        "the callback must park on the mint's player row lock: {parked:?}"
    );
    assert_eq!(status, 303);
    assert!(to.contains("=error"), "{to}");
    assert!(!minted, "a refused sign-in sets no session cookie");
    assert_eq!(held_after, Some(other), "the session the browser held survives the refusal");
}

//! Google on the hosted sign-in page: the form's start, the browser-only callback and the
//! shared limiter, against a fake Google (JWKS and `/token`, Google's issuer) and the live
//! database. The flow is built from parsed `GOOGLE_*` variables through the same
//! constructor `init` uses. A child of `tests` so it reuses that module's helpers.

use std::collections::{BTreeMap, HashMap};

use super::hosted_login::{client, cookie_value, has_attr, location, serve_default, set_cookie};
use super::*;
use crate::epic_oauth::EpicOAuth;
use crate::oauth_flow::{Intent, OAuthFlow};
use crate::providers::ProviderConfig;
use crate::web;

const KID: &str = "g1";
const CLIENT_ID: &str = "google-client";
const SECRET: &str = "google-secret";
const GOOD_CODE: &str = "good-code";
const BINDING_COOKIE: &str = "epic_oauth_binding";
const CALLBACK: &str = "/accounts/oidc/google/callback";

/// Google's flow as `init` builds it from these variables, with the default redirect URI.
fn google_flow(pool: &PgPool, jwks_url: &str, token_url: &str) -> Arc<OAuthFlow> {
    let vars: BTreeMap<String, String> = [
        ("GOOGLE_CLIENT_IDS", CLIENT_ID),
        ("GOOGLE_JWKS_URL", jwks_url),
        ("GOOGLE_CLIENT_SECRET", SECRET),
        ("GOOGLE_OAUTH_CLIENT_ID", CLIENT_ID),
        ("GOOGLE_TOKEN_URL", token_url),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let google = ProviderConfig::from_vars(&vars).unwrap().google.expect("google is configured");
    Arc::new(google.browser_flow(pool, false).unwrap().expect("the secret enables the flow"))
}

/// A fake Google: its JWKS, and a `/token` that requires the client's basic auth and
/// answers `GOOD_CODE` with an id_token for `subject` from `https://accounts.google.com`,
/// any other code with 400.
async fn fake_google(pool: &PgPool, subject: &str) -> Arc<OAuthFlow> {
    let (enc, jwks) = test_key(KID);
    let jwks_url = serve_jwks(jwks).await;
    let id_token = sign(
        &enc,
        KID,
        serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": CLIENT_ID,
            "sub": subject,
            "exp": future_exp(),
        }),
    );
    let body = serde_json::json!({ "id_token": id_token }).to_string();
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{SECRET}"))
    );
    let token_app = axum::Router::new().route(
        "/token",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Form(form): axum::Form<HashMap<String, String>>| {
                let (body, basic) = (body.clone(), basic.clone());
                async move {
                    use axum::response::IntoResponse as _;
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
                    if auth != Some(basic.as_str()) {
                        return axum::http::StatusCode::UNAUTHORIZED.into_response();
                    }
                    if form.get("code").map(String::as_str) != Some(GOOD_CODE) {
                        return (axum::http::StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#).into_response();
                    }
                    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, token_app).await.unwrap();
    });
    google_flow(pool, &jwks_url, &token_url)
}

/// The hosted pages with Google (and Epic when given) and their callbacks on one listener,
/// as `init` mounts them, sharing `limiter`.
async fn serve(
    svc: Arc<Service>,
    google: Arc<OAuthFlow>,
    epic: Option<Arc<EpicOAuth>>,
    limiter: Arc<httpmw::IpLimiter>,
) -> String {
    let limiter = web::LoginLimiter::new(limiter, Vec::new());
    let flows = web::Flows { epic, google: Some(google) };
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

/// The production sign-in rate, roomy enough for one test's requests.
fn roomy_bucket() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new(
        "accounts_google_browser_test",
        accountsapi::LOGIN_LIMIT_PER_SEC,
        accountsapi::LOGIN_LIMIT_BURST,
    )
}

/// One token: the first request is admitted, the next is refused.
fn one_token_bucket() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new("accounts_google_browser_test", 0.001, 1)
}

/// Submits the "Continue with Google" form, optionally from a browser holding `session`.
async fn post_google(base: &str, csrf: &str, session: Option<&str>, return_to: &str) -> reqwest::Response {
    let mut cookies = format!("{}={csrf}", web::LOGIN_CSRF_COOKIE);
    if let Some(session) = session {
        cookies.push_str(&format!("; {}={session}", web::SESSION_COOKIE));
    }
    client()
        .post(format!("{base}/accounts/oidc/google"))
        .header("Cookie", cookies)
        .form(&[("csrf", csrf), ("return_to", return_to)])
        .send()
        .await
        .unwrap()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url::Url::parse(url).unwrap().query_pairs().find_map(|(k, v)| (k == key).then(|| v.into_owned()))
}

/// Calls `path` back with `code` and `state` from a browser holding `binding`, if any.
async fn call_back(base: &str, path: &str, code: &str, state: &str, binding: Option<&str>) -> reqwest::Response {
    let mut req = client().get(format!("{base}{path}?code={code}&state={state}"));
    if let Some(binding) = binding {
        req = req.header("Cookie", format!("{BINDING_COOKIE}={binding}"));
    }
    req.send().await.unwrap()
}

fn browser(return_to: &str) -> Intent {
    Intent::Browser { return_to: return_to.into(), replaces: None }
}

async fn google_player(pool: &PgPool, subject: &str) -> Option<String> {
    sqlx::query_scalar("SELECT player_id::text FROM accounts.identities WHERE provider = 'google' AND subject = $1")
        .bind(subject)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn google_sign_in_sets_the_session_and_ends_the_replaced_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let subject = format!("google-browser-{}", suffix());
    let base = serve(svc.clone(), fake_google(&pool, &subject).await, None, roomy_bucket()).await;
    let other = svc
        .register(format!("google-browser-{}@test.local", suffix().to_ascii_lowercase()), "pw".into(), "Other".into(), test_client())
        .await
        .unwrap()
        .player_id;
    let held = svc.store.create_browser_session(&other).await.unwrap().unwrap().token;

    let form = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap();
    let csrf = cookie_value(&set_cookie(&form, web::LOGIN_CSRF_COOKIE));
    let form_body = form.text().await.unwrap();
    let return_to = "/accounts/oidc/authorize?client_id=lore&state=abc";
    let start = post_google(&base, &csrf, Some(&held), return_to).await;
    let start_status = start.status();
    let authorize = location(&start);
    let binding_cookie = set_cookie(&start, BINDING_COOKIE);
    let state = query_param(&authorize, "state").expect("state in the authorize url");
    let done = call_back(&base, CALLBACK, GOOD_CODE, &state, Some(&cookie_value(&binding_cookie))).await;
    let done_status = done.status();
    let done_to = location(&done);
    let done_cache = done.headers().get("cache-control").map(|v| v.to_str().unwrap().to_string());
    let token = cookie_value(&set_cookie(&done, web::SESSION_COOKIE));
    let player = google_player(&pool, &subject).await.expect("the callback created the google identity");
    let live = svc.store.browser_session(&token).await.unwrap().map(|s| s.player_id);
    let held_after = svc.store.browser_session(&held).await.unwrap().is_some();
    let display: String = sqlx::query_scalar("SELECT display_name FROM accounts.players WHERE id = $1::uuid")
        .bind(&player)
        .fetch_one(&pool)
        .await
        .unwrap();
    cleanup_player(&pool, &player).await;
    cleanup_player(&pool, &other).await;

    assert!(
        form_body.contains("Continue with Google") && form_body.contains(r#"action="/accounts/oidc/google""#),
        "{form_body}"
    );
    assert_eq!(start_status, 303);
    assert!(authorize.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"), "{authorize}");
    assert_eq!(query_param(&authorize, "scope").as_deref(), Some("openid"));
    assert_eq!(query_param(&authorize, "client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(
        query_param(&authorize, "redirect_uri").as_deref(),
        Some("http://localhost:8080/accounts/oidc/google/callback")
    );
    assert!(has_attr(&binding_cookie, "Path=/accounts"), "{binding_cookie}");
    assert_eq!(done_status, 303);
    assert_eq!(done_to, return_to, "the callback follows the stored return_to");
    assert_eq!(done_cache.as_deref(), Some("no-store"), "the sign-in response is not cached");
    assert_eq!(live.as_deref(), Some(player.as_str()), "the session cookie signs in the google player");
    assert!(!held_after, "the session the browser held at start ends at sign-in");
    assert!(display.starts_with("google:"), "{display}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_code_returns_to_the_sign_in_page_with_the_google_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let google = fake_google(&pool, "unused").await;
    let binding = store::new_token();
    let state = google.new_state(browser("/accounts/oidc/?a=b"), binding.clone()).await.unwrap();
    let base = serve(svc, google, None, roomy_bucket()).await;
    let resp = call_back(&base, CALLBACK, "bad-code", &state, Some(&binding)).await;
    let to = location(&resp);
    let page = client().get(format!("{base}{to}")).send().await.unwrap().text().await.unwrap();

    assert_eq!(resp.status(), 303);
    assert_eq!(to, "/accounts/oidc/login?google=error&return_to=%2Faccounts%2Foidc%2F%3Fa%3Db");
    assert!(page.contains("Sign-in with Google failed."), "{page}");
    assert!(!page.contains("Sign-in with Epic failed."), "{page}");
}

/// A state is redeemable only on its own provider's callback: the other one answers 400
/// and leaves the row for its owner, in both directions.
#[tokio::test(flavor = "multi_thread")]
async fn a_state_on_the_other_providers_callback_is_400_and_kept() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let google = fake_google(&pool, "unused").await;
    let epic = oauth_fixture(pool.clone(), "http://localhost/accounts/epic/callback", "http://127.0.0.1:9/token");
    let binding = store::new_token();
    let google_state = google.new_state(browser(web::LANDING), binding.clone()).await.unwrap();
    let epic_state = epic.new_state(browser(web::LANDING), binding.clone()).await.unwrap();
    let base = serve(svc, google.clone(), Some(epic.clone()), roomy_bucket()).await;

    let on_google = call_back(&base, CALLBACK, GOOD_CODE, &epic_state, Some(&binding)).await.status();
    let on_epic = call_back(&base, "/accounts/epic/callback", "abc", &google_state, Some(&binding)).await.status();

    assert_eq!(on_google, 400, "an epic state on the google callback");
    assert_eq!(on_epic, 400, "a google state on the epic callback");
    assert_eq!(google.take_state(&google_state, Some(&binding)).await.unwrap(), Some(browser(web::LANDING)));
    assert_eq!(epic.take_state(&epic_state, Some(&binding)).await.unwrap(), Some(browser(web::LANDING)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_or_mismatched_binding_is_400_and_keeps_the_state() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let google = fake_google(&pool, "unused").await;
    let binding = store::new_token();
    let state = google.new_state(browser(web::LANDING), binding.clone()).await.unwrap();
    let base = serve(svc, google.clone(), None, roomy_bucket()).await;

    let missing = call_back(&base, CALLBACK, GOOD_CODE, &state, None).await.status();
    let mismatched = call_back(&base, CALLBACK, GOOD_CODE, &state, Some(&store::new_token())).await.status();

    assert_eq!(missing, 400);
    assert_eq!(mismatched, 400);
    assert_eq!(google.take_state(&state, Some(&binding)).await.unwrap(), Some(browser(web::LANDING)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_csrf_mismatch_is_403_and_writes_no_state() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve(svc, fake_google(&pool, "unused").await, None, roomy_bucket()).await;
    let return_to = format!("/accounts/oidc/?t={}", suffix());
    let forged = client()
        .post(format!("{base}/accounts/oidc/google"))
        .header("Cookie", format!("{}={}", web::LOGIN_CSRF_COOKIE, store::new_token()))
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
async fn without_google_the_page_has_no_google_button_and_the_start_is_404() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc).await;
    let body = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap().text().await.unwrap();
    let post = client()
        .post(format!("{base}/accounts/oidc/google"))
        .form(&[("csrf", ""), ("return_to", "")])
        .send()
        .await
        .unwrap()
        .status();

    assert!(!body.contains("Continue with Google"), "{body}");
    assert_eq!(post, 404);
}

/// The Google start draws from the hosted form's bucket: a drained client is refused there,
/// and an admitted start drains it for the form.
#[tokio::test(flavor = "multi_thread")]
async fn the_google_start_shares_the_sign_in_bucket() {
    let svc = lazy_service();
    let google = || google_flow(&svc.store.pool, "http://127.0.0.1:9/jwks", "http://127.0.0.1:9/token");
    let login = |base: String| async move {
        client()
            .post(format!("{base}/accounts/oidc/login"))
            .form(&[("email", "a@x.io"), ("password", "pw"), ("csrf", "")])
            .send()
            .await
            .unwrap()
    };
    let start = |base: String| async move {
        client()
            .post(format!("{base}/accounts/oidc/google"))
            .form(&[("csrf", ""), ("return_to", "")])
            .send()
            .await
            .unwrap()
    };

    let drained_by_form = serve(svc.clone(), google(), None, one_token_bucket()).await;
    let form = login(drained_by_form.clone()).await;
    let refused = start(drained_by_form).await;
    let retry_after = refused.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string());
    let drained_by_start = serve(svc.clone(), google(), None, one_token_bucket()).await;
    let admitted = start(drained_by_start.clone()).await;
    let form_after = login(drained_by_start).await;

    assert_eq!(form.status(), 403, "admitted, then refused for the missing login-CSRF");
    assert_eq!(refused.status(), 429, "the google start shares the bucket the form emptied");
    assert_eq!(retry_after.as_deref(), Some("1"));
    assert_eq!(admitted.status(), 403, "admitted, then refused for the missing login-CSRF");
    assert_eq!(form_after.status(), 429, "the form shares the bucket the google start emptied");
}

/// With the sign-in bucket emptied by the hosted form, the Google and the Epic callbacks
/// still reach their handlers, without the hosted pages' headers.
#[tokio::test(flavor = "multi_thread")]
async fn the_callbacks_answer_with_the_sign_in_bucket_empty() {
    let svc = lazy_service();
    let google = google_flow(&svc.store.pool, "http://127.0.0.1:9/jwks", "http://127.0.0.1:9/token");
    let epic = oauth_fixture(
        svc.store.pool.clone(),
        "http://localhost/accounts/epic/callback",
        "http://127.0.0.1:9/token",
    );
    let base = serve(svc, google, Some(epic), one_token_bucket()).await;
    let login = || {
        client()
            .post(format!("{base}/accounts/oidc/login"))
            .form(&[("email", "a@x.io"), ("password", "pw"), ("csrf", "")])
            .send()
    };

    let admitted = login().await.unwrap();
    let refused = login().await.unwrap();
    let on_google = client().get(format!("{base}{CALLBACK}")).send().await.unwrap();
    let on_epic = client().get(format!("{base}/accounts/epic/callback")).send().await.unwrap();

    assert_eq!(admitted.status(), 403, "admitted, then refused for the missing login-CSRF");
    assert_eq!(refused.status(), 429, "the bucket is empty");
    assert!(refused.headers().contains_key("content-security-policy"), "the hosted form carries the headers");
    for (name, resp) in [("google", &on_google), ("epic", &on_epic)] {
        assert_eq!(resp.status(), 400, "the {name} callback reaches its handler: missing code or state");
        assert!(!resp.headers().contains_key("content-security-policy"), "the {name} callback has no page headers");
    }
}

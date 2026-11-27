//! The device approval pages over a real listener against the live database. A child of `tests`
//! so it reuses that module's live-DB harness. Expiry and age are set by backdating the row, never
//! by waiting.

use std::net::{IpAddr, Ipv4Addr};

use accountsapi::grant_types::{DEVICE_CODE, REFRESH_TOKEN};

use super::hosted_login::{client, location, registered, serve_web};
use super::token::token_client;
use super::*;
use crate::device_codes::{normalize_user_code, NewDeviceCode};
use crate::web;
use crate::web::device::{DEVICE, DEVICE_APPROVE, DEVICE_DENY, EXPIRED_PAGE_ERROR, INVALID_CODE_ERROR};

const DEVICE_NAME: &str = "lore-cli 0.14";
const LOGIN_WITH_CODE: &str = "/accounts/oidc/login?return_to=%2Faccounts%2Foidc%2Fdevice%3Fuser_code%3DBCDF-GHJK";

fn roomy() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new("accounts_device_test", 1000.0, 1000)
}

fn tight() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new("accounts_device_test", 0.001, 1)
}

/// A pending code for `client_id`, as the device shows it (`XXXX-XXXX`).
async fn issue(svc: &Service, client_id: &str) -> String {
    let new = NewDeviceCode {
        client_id,
        scope: "openid",
        requested_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        user_agent: Some(DEVICE_NAME),
    };
    svc.store.insert_device_code(&new).await.unwrap().expect("a known client").user_code
}

/// A signed-in browser: the player, the session cookie value and its CSRF token.
struct Browser {
    pid: String,
    token: String,
    csrf: String,
}

async fn browser(svc: &Service) -> Browser {
    let (_, pid) = registered(svc, "device").await;
    let session = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    Browser { pid, token: session.token, csrf: session.csrf_token }
}

async fn page(base: &str, query: &[(&str, &str)], session: Option<&str>) -> reqwest::Response {
    let mut req = client().get(format!("{base}{DEVICE}")).query(query);
    if let Some(session) = session {
        req = req.header("Cookie", format!("{}={session}", web::SESSION_COOKIE));
    }
    req.send().await.unwrap()
}

async fn decide(
    base: &str,
    path: &str,
    user_code: &str,
    csrf: Option<&str>,
    session: Option<&str>,
) -> reqwest::Response {
    let mut form = vec![("user_code", user_code)];
    if let Some(csrf) = csrf {
        form.push(("csrf", csrf));
    }
    let mut req = client().post(format!("{base}{path}")).form(&form);
    if let Some(session) = session {
        req = req.header("Cookie", format!("{}={session}", web::SESSION_COOKIE));
    }
    req.send().await.unwrap()
}

/// Runs `set` (an assignment list) on the row of `user_code`.
async fn backdate(pool: &PgPool, user_code: &str, set: &str) {
    sqlx::query(&format!(
        "UPDATE accounts.device_codes SET {set} WHERE user_code_digest = sha256(convert_to($1, 'UTF8'))"
    ))
    .bind(normalize_user_code(user_code).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

type Row = (String, Option<String>, Option<Vec<u8>>, Option<i64>);

/// The row of `user_code`: status, player, session digest and `auth_time` in Unix seconds.
async fn row(pool: &PgPool, user_code: &str) -> Row {
    sqlx::query_as(
        "SELECT status, player_id::text, session_digest, floor(EXTRACT(EPOCH FROM auth_time))::int8 \
           FROM accounts.device_codes WHERE user_code_digest = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(normalize_user_code(user_code).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn pending() -> Row {
    ("pending".to_string(), None, None, None)
}

async fn body(resp: reqwest::Response, status: u16, case: &str) -> String {
    assert_eq!(resp.status(), status, "{case}");
    resp.text().await.unwrap()
}

/// A signed-out browser goes to the login with a `return_to` that comes back to the same code,
/// whether it was typed in the boxes, linked as `user_code` or posted to approve; without a
/// well-formed code it comes back to the empty form.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_out_browser_goes_to_the_login_keeping_the_code() {
    let base = serve_web(lazy_service(), "", roomy()).await;

    let linked = page(&base, &[("user_code", "bcdf-ghjk")], None).await;
    assert_eq!(linked.status(), 303);
    assert_eq!(location(&linked), LOGIN_WITH_CODE);

    let boxes: Vec<(String, String)> =
        "bcdfghjk".chars().enumerate().map(|(i, c)| (format!("c{}", i + 1), c.to_string())).collect();
    let boxes: Vec<(&str, &str)> = boxes.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    let typed = page(&base, &boxes, None).await;
    assert_eq!(typed.status(), 303);
    assert_eq!(location(&typed), LOGIN_WITH_CODE);

    let empty = page(&base, &[], None).await;
    assert_eq!(location(&empty), "/accounts/oidc/login?return_to=%2Faccounts%2Foidc%2Fdevice");
    let malformed = page(&base, &[("user_code", "not a code")], None).await;
    assert_eq!(location(&malformed), "/accounts/oidc/login?return_to=%2Faccounts%2Foidc%2Fdevice");

    let posted = decide(&base, DEVICE_APPROVE, "BCDF-GHJK", Some("x"), None).await;
    assert_eq!(posted.status(), 303);
    assert_eq!(location(&posted), LOGIN_WITH_CODE);
}

/// No code is the enter-code form: eight labelled boxes and the lifetime bound of every code.
#[tokio::test(flavor = "multi_thread")]
async fn the_enter_form_draws_eight_boxes_and_the_lifetime_bound() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let b = browser(&svc).await;
    let base = serve_web(svc, "", roomy()).await;

    let html = body(page(&base, &[], Some(&b.token)).await, 200, "the empty form").await;
    for i in 1..=8 {
        assert!(html.contains(&format!("name=\"c{i}\" aria-label=\"Character {i}\"")), "box {i}");
    }
    assert!(html.contains("aria-label=\"Device code\""));
    let minutes = crate::clients::MAX_DEVICE_CODE_TTL_SECS / 60;
    let ttl = format!("A code expires at most {minutes} min after it is issued.");
    assert!(html.contains(&ttl), "the lifetime bound");
    assert!(!html.contains("banner err"), "no error on the empty form");
    assert!(html.contains("<title>Connect a device · Swarog</title>"));
}

/// Unknown, expired, approved, denied and malformed codes all get the same page.
#[tokio::test(flavor = "multi_thread")]
async fn every_code_that_is_not_pending_gets_one_generic_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let b = browser(&svc).await;
    let expired = issue(&svc, &client_id).await;
    backdate(&pool, &expired, "expires_at = now() - interval '1 second'").await;
    let approved = issue(&svc, &client_id).await;
    let digest = web::session::digest(&b.token);
    assert!(svc.store.approve_device_code(&approved, &digest).await.unwrap());
    let denied = issue(&svc, &client_id).await;
    assert!(svc.store.deny_device_code(&denied).await.unwrap());
    let base = serve_web(svc, "", roomy()).await;

    let mut bodies = Vec::new();
    for (case, code) in [
        ("unknown", "BCDF-GHJK"),
        ("expired", expired.as_str()),
        ("approved", approved.as_str()),
        ("denied", denied.as_str()),
        ("malformed", "AEIO-U123"),
    ] {
        let html = body(page(&base, &[("user_code", code)], Some(&b.token)).await, 400, case).await;
        assert!(html.contains(INVALID_CODE_ERROR), "{case}");
        bodies.push(html);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "the pages differ");
}

/// The confirm screen names the client, the device, the player, the code, the requesting IP and
/// age, and how long the session lasts: a refresh family for a client that can refresh, else
/// one access token.
#[tokio::test(flavor = "multi_thread")]
async fn the_confirm_screen_shows_the_request_and_the_session_length() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let access_only = token_client(&pool, &[DEVICE_CODE], false).await;
    let refreshing = token_client(&pool, &[DEVICE_CODE, REFRESH_TOKEN], false).await;
    let b = browser(&svc).await;
    let short = issue(&svc, &access_only).await;
    backdate(&pool, &short, "created_at = now() - interval '125.5 seconds'").await;
    let long = issue(&svc, &refreshing).await;
    let base = serve_web(svc, "", roomy()).await;

    let typed = short.to_ascii_lowercase();
    let html = body(page(&base, &[("user_code", &typed)], Some(&b.token)).await, 200, "access only").await;
    assert!(html.contains(&format!("<h1>Allow token test client on {DEVICE_NAME} to act as Hosted?</h1>")));
    assert!(html.contains("token test client will be able to act as you in every service that accepts your account."));
    assert!(html.contains(&format!("<b>Code</b><span class=\"mono\">{short}</span>")));
    assert!(html.contains(&format!("<b>Device</b><span class=\"mono\">{DEVICE_NAME}</span> (reported by the device)")));
    let from = format!("<b>Requested from</b><span class=\"mono\">203.0.113.7</span> · {}", controlplane::ago(125));
    assert!(html.contains(&from), "{from}");
    assert!(html.contains(&format!("<b>Session lasts</b>{} min<", accountsapi::ACCESS_TTL_MINUTES)));
    assert!(html.contains(&format!("name=\"csrf\" value=\"{}\"", b.csrf)));
    assert!(html.contains("action=\"/accounts/oidc/device/approve\""));

    let html = body(page(&base, &[("user_code", &long)], Some(&b.token)).await, 200, "refreshing").await;
    assert!(html.contains(&format!("<b>Session lasts</b>{} d<", crate::store::REFRESH_TTL_DAYS)));
    assert_eq!(row(&pool, &short).await, pending(), "a lookup moves nothing");
}

/// Approve refuses a missing or wrong CSRF token and leaves the row pending; with the session's
/// token it binds the player, the session and its sign-in time once, and a second approve or a
/// deny then gets the generic error and changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn approve_needs_the_sessions_csrf_and_moves_only_a_pending_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let b = browser(&svc).await;
    let code = issue(&svc, &client_id).await;
    let base = serve_web(svc, "", roomy()).await;

    for (case, csrf) in [("no csrf", None), ("wrong csrf", Some("wrong"))] {
        let html = body(decide(&base, DEVICE_APPROVE, &code, csrf, Some(&b.token)).await, 403, case).await;
        assert!(html.contains(EXPIRED_PAGE_ERROR), "{case}");
        assert_eq!(row(&pool, &code).await, pending(), "{case}");
    }

    let html = body(decide(&base, DEVICE_APPROVE, &code, Some(&b.csrf), Some(&b.token)).await, 200, "approve").await;
    assert!(html.contains(&format!("{DEVICE_NAME} is connected")));
    assert!(html.contains("Return to your device. token test client is signed in as Hosted."));
    let view_devices = "<button class=\"btn\" type=\"button\" disabled title=\"Not implemented yet\">View devices</button>";
    assert!(html.contains(view_devices), "View devices is drawn disabled");
    let digest = web::session::digest(&b.token);
    let signed_in_at: i64 = sqlx::query_scalar(
        "SELECT floor(EXTRACT(EPOCH FROM created_at))::int8 FROM accounts.browser_sessions WHERE token_digest = $1",
    )
    .bind(&digest)
    .fetch_one(&pool)
    .await
    .unwrap();
    let approved: Row = ("approved".to_string(), Some(b.pid.clone()), Some(digest), Some(signed_in_at));
    assert_eq!(row(&pool, &code).await, approved);

    for (case, path) in [("second approve", DEVICE_APPROVE), ("deny after approve", DEVICE_DENY)] {
        let html = body(decide(&base, path, &code, Some(&b.csrf), Some(&b.token)).await, 400, case).await;
        assert!(html.contains(INVALID_CODE_ERROR), "{case}");
        assert_eq!(row(&pool, &code).await, approved, "{case}");
    }
}

/// Deny refuses a wrong CSRF token, then denies once; an approve after it, and either decision
/// on an expired code, get the generic error and leave the row as it was.
#[tokio::test(flavor = "multi_thread")]
async fn deny_moves_only_a_pending_unexpired_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let b = browser(&svc).await;
    let code = issue(&svc, &client_id).await;
    let expired = issue(&svc, &client_id).await;
    backdate(&pool, &expired, "expires_at = now() - interval '1 second'").await;
    let base = serve_web(svc, "", roomy()).await;

    let html = body(decide(&base, DEVICE_DENY, &code, Some("wrong"), Some(&b.token)).await, 403, "wrong csrf").await;
    assert!(html.contains(EXPIRED_PAGE_ERROR));
    assert_eq!(row(&pool, &code).await, pending());

    let html = body(decide(&base, DEVICE_DENY, &code, Some(&b.csrf), Some(&b.token)).await, 200, "deny").await;
    assert!(html.contains("Device not connected"));
    assert!(html.contains(&format!("You denied code <span class=\"mono\">{code}</span>.")));
    let denied: Row = ("denied".to_string(), None, None, None);
    assert_eq!(row(&pool, &code).await, denied);

    let resp = decide(&base, DEVICE_APPROVE, &code, Some(&b.csrf), Some(&b.token)).await;
    let html = body(resp, 400, "approve after deny").await;
    assert!(html.contains(INVALID_CODE_ERROR));
    assert_eq!(row(&pool, &code).await, denied);

    for path in [DEVICE_APPROVE, DEVICE_DENY] {
        let html = body(decide(&base, path, &expired, Some(&b.csrf), Some(&b.token)).await, 400, path).await;
        assert!(html.contains(INVALID_CODE_ERROR), "{path}");
        assert_eq!(row(&pool, &expired).await, pending(), "{path} on an expired code");
    }
}

/// A lookup, an approve and a deny each take a token from the sign-in bucket, so the pages are
/// no side door for guessing codes; an empty form takes none.
#[tokio::test(flavor = "multi_thread")]
async fn lookup_approve_and_deny_draw_from_the_sign_in_limiter() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client_id = token_client(&pool, &[DEVICE_CODE], false).await;
    let b = browser(&svc).await;
    let code = issue(&svc, &client_id).await;

    let base = serve_web(svc.clone(), "", tight()).await;
    assert_eq!(page(&base, &[], Some(&b.token)).await.status(), 200, "the empty form");
    assert_eq!(page(&base, &[("user_code", "BCDF-GHJK")], Some(&b.token)).await.status(), 400, "admitted");
    let limited = page(&base, &[("user_code", &code)], Some(&b.token)).await;
    assert_eq!(limited.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string()).as_deref(), Some("1"));
    assert_eq!(limited.status(), 429, "the lookup bucket is empty");

    for path in [DEVICE_APPROVE, DEVICE_DENY] {
        let base = serve_web(svc.clone(), "", tight()).await;
        let first = decide(&base, path, &code, Some("wrong"), Some(&b.token)).await;
        assert_eq!(first.status(), 403, "{path}: admitted, then refused for the CSRF token");
        let second = decide(&base, path, &code, Some(&b.csrf), Some(&b.token)).await;
        assert_eq!(second.status(), 429, "{path}: the bucket is empty");
        assert_eq!(row(&pool, &code).await, pending(), "{path}");
    }
}

//! The hosted sign-in pages over a real listener against the live database. A child of
//! `tests` so it reuses that module's live-DB harness.

use super::*;
use crate::web;

const SECURE: bool = true;

async fn serve_web(svc: Arc<Service>, trusted: &str, limiter: Arc<httpmw::IpLimiter>) -> String {
    let trusted = httpmw::parse_cidrs(trusted).unwrap();
    let hosted = web::Web::new(svc, SECURE, web::LoginLimiter::new(limiter, trusted), web::Flows::default()).unwrap();
    let app = web::router(Arc::new(hosted));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

pub(super) async fn serve_default(svc: Arc<Service>) -> String {
    let bucket = httpmw::IpLimiter::new(
        "accounts_login_test",
        accountsapi::LOGIN_LIMIT_PER_SEC,
        accountsapi::LOGIN_LIMIT_BURST,
    );
    serve_web(svc, "", bucket).await
}

pub(super) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// The `Set-Cookie` header that sets `name`.
pub(super) fn set_cookie(resp: &reqwest::Response, name: &str) -> String {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|v| v.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} cookie set"))
}

pub(super) fn cookie_value(set_cookie: &str) -> String {
    let pair = set_cookie.split(';').next().unwrap();
    pair.split_once('=').unwrap().1.to_string()
}

pub(super) fn has_attr(set_cookie: &str, attr: &str) -> bool {
    set_cookie.split("; ").any(|part| part == attr)
}

pub(super) fn location(resp: &reqwest::Response) -> String {
    resp.headers().get("location").unwrap().to_str().unwrap().to_string()
}

/// GETs the form and returns its login-CSRF token.
async fn login_csrf(base: &str) -> String {
    let resp = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    cookie_value(&set_cookie(&resp, web::LOGIN_CSRF_COOKIE))
}

async fn post_login(base: &str, csrf: &str, email: &str, password: &str, return_to: &str) -> reqwest::Response {
    client()
        .post(format!("{base}/accounts/oidc/login"))
        .header("Cookie", format!("{}={csrf}", web::LOGIN_CSRF_COOKIE))
        .form(&[("email", email), ("password", password), ("csrf", csrf), ("return_to", return_to)])
        .send()
        .await
        .unwrap()
}

async fn registered(svc: &Service, tag: &str) -> (String, String) {
    let email = format!("hosted-{tag}-{}@test.local", suffix().to_ascii_lowercase());
    let pid = svc.register(email.clone(), "pw".into(), "Hosted".into(), test_client()).await.unwrap().player_id;
    (email, pid)
}

#[test]
fn return_to_keeps_a_path_under_the_prefix_with_its_query() {
    let authorize = "/accounts/oidc/authorize?client_id=lore&redirect_uri=https%3A%2F%2Fx&state=s";
    assert_eq!(web::safe_return_to(Some(authorize)), authorize);
    assert_eq!(web::safe_return_to(Some("/accounts/oidc/")), "/accounts/oidc/");
}

#[test]
fn return_to_outside_the_rule_is_the_landing_page() {
    let long = format!("/accounts/oidc/{}", "a".repeat(web::MAX_RETURN_TO_BYTES));
    for bad in [
        "//evil",
        "https://x",
        "https://x/accounts/oidc/",
        "/accounts/oidc//evil.example",
        "/accounts/oidc/\\evil.example",
        "/accounts/oidc",
        "/accounts/oidcx/",
        "/admin",
        "/accounts/oidc/../../admin",
        "/accounts/oidc/%2E%2e/admin",
        "/accounts/oidc/..#",
        "/accounts/oidc/%2e%2e#",
        "/accounts/oidc/x\r\nSet-Cookie:a=b",
        "",
        long.as_str(),
    ] {
        assert_eq!(web::safe_return_to(Some(bad)), web::LANDING, "{bad:?}");
    }
    assert_eq!(web::safe_return_to(None), web::LANDING);
}

/// Sign-in through the form: both cookies carry their attributes, `return_to` keeps its
/// query, the browser token is no bearer, the landing page shows the handle, and logout
/// needs the session's CSRF token before it deletes the session.
#[tokio::test(flavor = "multi_thread")]
async fn sign_in_landing_and_logout() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (email, pid) = registered(&svc, "flow").await;
    let base = serve_default(svc.clone()).await;

    let form = client().get(format!("{base}/accounts/oidc/login")).send().await.unwrap();
    let csrf_cookie = set_cookie(&form, web::LOGIN_CSRF_COOKIE);
    let headers = form.headers().clone();
    let csrf = cookie_value(&csrf_cookie);
    let return_to = "/accounts/oidc/authorize?client_id=lore&state=abc";
    let resp = post_login(&base, &csrf, &email.to_ascii_uppercase(), "pw", return_to).await;
    let status = resp.status();
    let to = location(&resp);
    let session_cookie = set_cookie(&resp, web::SESSION_COOKIE);
    let token = cookie_value(&session_cookie);
    let as_bearer = svc.tokens.verify_access(&token).await.unwrap();
    let session = format!("{}={token}", web::SESSION_COOKIE);

    let landing = client()
        .get(format!("{base}/accounts/oidc/"))
        .header("Cookie", &session)
        .send()
        .await
        .unwrap();
    let landing_status = landing.status();
    let landing_body = landing.text().await.unwrap();
    let summary = svc.store.player_summary(&pid).await.unwrap().unwrap();
    let live = svc.store.browser_session(&token).await.unwrap().unwrap();

    let no_csrf = client()
        .post(format!("{base}/accounts/oidc/logout"))
        .header("Cookie", &session)
        .form(&[("csrf", "")])
        .send()
        .await
        .unwrap()
        .status();
    let after_no_csrf = svc.store.browser_session(&token).await.unwrap();
    let logout = client()
        .post(format!("{base}/accounts/oidc/logout"))
        .header("Cookie", &session)
        .form(&[("csrf", live.csrf_token.as_str())])
        .send()
        .await
        .unwrap();
    let logout_status = logout.status();
    let cleared = set_cookie(&logout, web::SESSION_COOKIE);
    let after_logout = svc.store.browser_session(&token).await.unwrap();
    cleanup_player(&pool, &pid).await;

    for attr in ["HttpOnly", "SameSite=Strict", "Path=/accounts/oidc", "Max-Age=600", "Secure"] {
        assert!(has_attr(&csrf_cookie, attr), "{attr}: {csrf_cookie}");
    }
    let csp = headers.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'") && !csp.contains("script-src"), "{csp}");
    // form-action would also bind the redirects after the POST, so the 303 to authorize
    // and the client's cross-origin redirect_uri would be blocked.
    assert!(!csp.contains("form-action"), "{csp}");
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");

    assert_eq!(status, 303);
    assert_eq!(to, return_to, "a valid return_to keeps its query");
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/accounts/oidc", "Max-Age=43200", "Secure"] {
        assert!(has_attr(&session_cookie, attr), "{attr}: {session_cookie}");
    }
    assert_eq!(as_bearer, None, "a browser session token is not an access token");

    assert_eq!(landing_status, 200);
    assert!(landing_body.contains(&summary.handle), "{landing_body}");

    assert_eq!(no_csrf, 403, "logout without the CSRF token is refused");
    assert!(after_no_csrf.is_some(), "a refused logout keeps the session");
    assert_eq!(logout_status, 303);
    assert!(has_attr(&cleared, "Max-Age=0"), "{cleared}");
    assert!(after_logout.is_none(), "logout deletes the session");
}

/// A form rendered for a browser that already holds a login-CSRF token sets that token
/// again, so the cookie's ten minutes run from the form the browser shows.
#[tokio::test(flavor = "multi_thread")]
async fn a_reused_login_csrf_token_is_set_again() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let base = serve_default(svc).await;
    let csrf = login_csrf(&base).await;

    let again = client()
        .get(format!("{base}/accounts/oidc/login"))
        .header("Cookie", format!("{}={csrf}", web::LOGIN_CSRF_COOKIE))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 200);
    let reset = set_cookie(&again, web::LOGIN_CSRF_COOKIE);
    assert_eq!(cookie_value(&reset), csrf, "the browser's token is reused");
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/accounts/oidc", "Max-Age=600", "Secure"] {
        assert!(has_attr(&reset, attr), "{attr}: {reset}");
    }
}

/// Signing in again from a browser that holds a session ends that session.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_sign_in_ends_the_browsers_old_session() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (email, pid) = registered(&svc, "again").await;
    let base = serve_default(svc.clone()).await;

    let csrf = login_csrf(&base).await;
    let first = post_login(&base, &csrf, &email, "pw", "").await;
    let old = cookie_value(&set_cookie(&first, web::SESSION_COOKIE));
    let csrf = login_csrf(&base).await;
    let second = client()
        .post(format!("{base}/accounts/oidc/login"))
        .header("Cookie", format!("{}={csrf}; {}={old}", web::LOGIN_CSRF_COOKIE, web::SESSION_COOKIE))
        .form(&[("email", email.as_str()), ("password", "pw"), ("csrf", &csrf), ("return_to", "")])
        .send()
        .await
        .unwrap();
    let second_status = second.status();
    let new = cookie_value(&set_cookie(&second, web::SESSION_COOKIE));
    let old_after = svc.store.browser_session(&old).await.unwrap();
    let new_after = svc.store.browser_session(&new).await.unwrap();
    cleanup_player(&pool, &pid).await;

    assert_eq!(second_status, 303);
    assert_ne!(new, old);
    assert!(old_after.is_none(), "the replaced session no longer resolves");
    assert!(new_after.is_some(), "the new session resolves");
}

/// A sign-in refused at its identity stamp keeps the session the browser held. A third
/// transaction parks the sign-in at the stamp, then changes the password hash under it.
#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_refused_at_the_stamp_keeps_the_browsers_session() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (email, pid) = registered(&svc, "stamp").await;
    let (_, other) = registered(&svc, "stamp-other").await;
    let held = svc.store.create_browser_session(&other).await.unwrap().unwrap().token;
    let base = serve_default(svc.clone()).await;
    let csrf = login_csrf(&base).await;
    let mut gate = pool.begin().await.unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *gate).await.unwrap();
    sqlx::query("SELECT 1 FROM accounts.identities WHERE player_id = $1::uuid FOR UPDATE")
        .bind(&pid)
        .execute(&mut *gate)
        .await
        .unwrap();
    let login = {
        let (base, csrf, email, held) = (base.clone(), csrf.clone(), email.clone(), held.clone());
        tokio::spawn(async move {
            client()
                .post(format!("{base}/accounts/oidc/login"))
                .header("Cookie", format!("{}={csrf}; {}={held}", web::LOGIN_CSRF_COOKIE, web::SESSION_COOKIE))
                .form(&[("email", email.as_str()), ("password", "pw"), ("csrf", csrf.as_str()), ("return_to", "")])
                .send()
                .await
                .unwrap()
        })
    };
    let parked = blocked_query(&pool, gate_pid).await;
    sqlx::query("UPDATE accounts.identities SET secret_hash = 'changed' WHERE player_id = $1::uuid")
        .bind(&pid)
        .execute(&mut *gate)
        .await
        .unwrap();
    gate.commit().await.unwrap();
    let resp = login.await.unwrap();
    let status = resp.status();
    let minted = resp.headers().get_all("set-cookie").iter().any(|v| {
        v.to_str().unwrap().starts_with(&format!("{}=", web::SESSION_COOKIE))
    });
    let held_after = svc.store.browser_session(&held).await.unwrap().map(|s| s.player_id);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts.browser_sessions WHERE player_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    cleanup_player(&pool, &pid).await;
    cleanup_player(&pool, &other).await;

    assert!(
        parked.as_deref().is_some_and(|q| q.contains("accounts.identities")),
        "the sign-in must park at its identity stamp: {parked:?}"
    );
    assert_eq!(status, 401);
    assert!(!minted, "a refused sign-in sets no session cookie");
    assert_eq!(held_after, Some(other), "the session the browser held survives the refusal");
    assert_eq!(rows, 0, "the refused mint leaves no session");
}

/// A wrong password and an unknown email get the same 401 page, and the wrong password ends
/// no session the browser held; a POST without the login-CSRF cookie is 403 and mints nothing.
#[tokio::test(flavor = "multi_thread")]
async fn bad_credentials_are_one_generic_error_and_csrf_is_required() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (email, pid) = registered(&svc, "bad").await;
    let base = serve_default(svc.clone()).await;
    let csrf = login_csrf(&base).await;

    let held = svc.store.create_browser_session(&pid).await.unwrap().unwrap().token;
    let wrong = client()
        .post(format!("{base}/accounts/oidc/login"))
        .header("Cookie", format!("{}={csrf}; {}={held}", web::LOGIN_CSRF_COOKIE, web::SESSION_COOKIE))
        .form(&[("email", email.as_str()), ("password", "not-it"), ("csrf", csrf.as_str()), ("return_to", "")])
        .send()
        .await
        .unwrap();
    let wrong_status = wrong.status();
    let held_after = svc.store.browser_session(&held).await.unwrap().is_some();
    let wrong_body = wrong.text().await.unwrap();
    let unknown = post_login(&base, &csrf, "nobody-hosted@test.local", "pw", "").await;
    let unknown_status = unknown.status();
    let unknown_body = unknown.text().await.unwrap();
    let forged = client()
        .post(format!("{base}/accounts/oidc/login"))
        .form(&[("email", email.as_str()), ("password", "pw"), ("csrf", csrf.as_str())])
        .send()
        .await
        .unwrap();
    let forged_status = forged.status();
    let forged_minted = forged.headers().get_all("set-cookie").iter().any(|v| {
        v.to_str().unwrap().starts_with(&format!("{}=", web::SESSION_COOKIE))
    });
    let other = crate::store::new_token();
    let mismatched = client()
        .post(format!("{base}/accounts/oidc/login"))
        .header("Cookie", format!("{}={csrf}", web::LOGIN_CSRF_COOKIE))
        .form(&[("email", email.as_str()), ("password", "pw"), ("csrf", other.as_str())])
        .send()
        .await
        .unwrap()
        .status();
    cleanup_player(&pool, &pid).await;

    assert_eq!(wrong_status, 401);
    assert_eq!(unknown_status, 401);
    assert_eq!(wrong_body, unknown_body, "no email oracle in the page");
    assert!(held_after, "a wrong password leaves the browser's session signed in");
    assert!(wrong_body.contains("Invalid email or password."), "{wrong_body}");
    assert_eq!(forged_status, 403, "no login-CSRF cookie");
    assert!(!forged_minted);
    assert_eq!(mismatched, 403, "the form token differs from the cookie");
}

/// `return_to=//evil` and `return_to=https://x` land on the landing page after sign-in.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsafe_return_to_lands_on_the_landing_page() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (email, pid) = registered(&svc, "ret").await;
    let base = serve_default(svc.clone()).await;
    let csrf = login_csrf(&base).await;
    let protocol_relative = post_login(&base, &csrf, &email, "pw", "//evil").await;
    let absolute = post_login(&base, &csrf, &email, "pw", "https://x").await;
    let (a_status, a_to) = (protocol_relative.status(), location(&protocol_relative));
    let (b_status, b_to) = (absolute.status(), location(&absolute));
    cleanup_player(&pool, &pid).await;

    assert_eq!((a_status.as_u16(), a_to.as_str()), (303, web::LANDING));
    assert_eq!((b_status.as_u16(), b_to.as_str()), (303, web::LANDING));
}

/// The bucket accounts builds for sign-in admits one client exactly `accountsapi::LOGIN_LIMIT_BURST`
/// requests at once, then refills exactly `accountsapi::LOGIN_LIMIT_PER_SEC` a second: on a clock
/// the test drives, a drained client gets that many whole tokens one second later and one fewer a
/// millisecond earlier. So the burst and rate a harness reads off the contract are the ones the
/// module runs.
#[test]
fn the_sign_in_bucket_runs_the_published_burst_and_refill_rate() {
    let ip: std::net::IpAddr = "203.0.113.9".parse().unwrap();
    let admitted = |limiter: &httpmw::IpLimiter, at: std::time::Instant| {
        (0..accountsapi::LOGIN_LIMIT_BURST * 2).take_while(|_| limiter.allow_at(ip, at)).count()
    };
    let t0 = std::time::Instant::now();
    let rate = accountsapi::LOGIN_LIMIT_PER_SEC as usize;

    let limiter = crate::login_ip_limiter();
    assert_eq!(admitted(&limiter, t0), accountsapi::LOGIN_LIMIT_BURST as usize, "the burst at t0");
    assert_eq!(admitted(&limiter, t0 + std::time::Duration::from_secs(1)), rate, "one second's refill");

    let early = crate::login_ip_limiter();
    assert_eq!(admitted(&early, t0), accountsapi::LOGIN_LIMIT_BURST as usize, "the burst at t0");
    let just_short = t0 + std::time::Duration::from_secs(1) - std::time::Duration::from_millis(1);
    assert_eq!(admitted(&early, just_short), rate - 1, "a millisecond short of one second's refill");
}

/// Behind a trusted proxy each forwarded client gets its own limiter bucket; the proxy's
/// own address is not the key.
#[tokio::test(flavor = "multi_thread")]
async fn clients_behind_a_trusted_proxy_have_separate_limiter_buckets() {
    let svc = lazy_service();
    let limiter = httpmw::IpLimiter::new("accounts_login_test", 0.001, 1);
    let base = serve_web(svc, "127.0.0.1/32", limiter).await;
    let post = |client_ip: &'static str| {
        let base = base.clone();
        async move {
            client()
                .post(format!("{base}/accounts/oidc/login"))
                .header("X-Forwarded-For", client_ip)
                .form(&[("email", "a@x.io"), ("password", "pw"), ("csrf", "")])
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    let first_a = post("203.0.113.1").await;
    let second_a = post("203.0.113.1").await;
    let first_b = post("203.0.113.2").await;

    assert_eq!(first_a, 403, "admitted, then refused for the missing login-CSRF");
    assert_eq!(second_a, 429, "client A's bucket is empty");
    assert_eq!(first_b, 403, "client B has its own bucket");
}

/// The Epic form POST checks the same per-IP limiter before anything else.
#[tokio::test(flavor = "multi_thread")]
async fn the_epic_form_post_is_rate_limited() {
    let svc = lazy_service();
    let base = serve_web(svc, "", httpmw::IpLimiter::new("accounts_login_test", 0.001, 1)).await;
    let post = || {
        client()
            .post(format!("{base}/accounts/oidc/epic"))
            .form(&[("csrf", ""), ("return_to", "")])
            .send()
    };
    let first = post().await.unwrap();
    let second = post().await.unwrap();
    let retry_after = second.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string());

    assert_eq!(first.status(), 404, "admitted, then refused: Epic is not configured here");
    assert_eq!(second.status(), 429, "the bucket is empty");
    assert_eq!(retry_after.as_deref(), Some("1"));
}

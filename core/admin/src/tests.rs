//! Admin tests. The pure helpers run with no DB. The session-auth matrix —
//! login/lockout/CSRF/logout/cookie flags plus the durable `admin.action` emits — targets
//! the local Postgres (the test DB) and gets its pool from `testdb`: an unreachable
//! cluster FAILS the run.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use lifecycle::Context;
use tower::ServiceExt as _; // for `oneshot`

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// ---- shared helpers -----------------------------------------------------------

fn login_reaper_is_owned(admin: &Admin) -> bool {
    admin
        .login_reaper
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

#[tokio::test]
async fn login_reaper_stops_and_restarts_without_clock_coordination() {
    let admin = Admin::new();
    assert!(
        admin
            .login_limiter
            .set(httpmw::IpLimiter::new("admin_login", 5.0, 20))
            .is_ok()
    );
    let ctx = Context::new();

    admin.start_login_reaper().unwrap();
    assert!(login_reaper_is_owned(&admin));
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin));

    admin.start_login_reaper().unwrap();
    assert!(login_reaper_is_owned(&admin));
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin));
}

// ---- env-knob parsing (dev knobs, explicit-only conventions) ------------------

/// Serializes the env-mutating tests below — the knobs are process-global.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with the given env var set (or cleared), restoring the prior value.
fn with_env(key: &str, val: Option<&str>, f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(key).ok();
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

/// `admin_open_explicitly_on` matches the apikeys/gateway truthy set, case-insensitively.
#[test]
fn admin_open_truthy_parsing() {
    for (val, want) in [
        (Some("1"), true),
        (Some("true"), true),
        (Some("ON"), true),
        (Some("TrUe"), true),
        (Some("0"), false),
        (Some("no"), false),
        (Some(""), false),
        (None, false),
    ] {
        with_env("ADMIN_OPEN", val, || {
            assert_eq!(admin_open_explicitly_on(), want, "ADMIN_OPEN={val:?}");
        });
    }
}

/// `ADMIN_COOKIE_SECURE` is fail-closed: Secure stays ON unless EXPLICITLY falsy.
#[test]
fn cookie_secure_knob_parsing() {
    for (val, want) in [
        (None, true),
        (Some("1"), true),
        (Some("yes"), true),
        (Some(""), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some("OFF"), false),
    ] {
        with_env("ADMIN_COOKIE_SECURE", val, || {
            assert_eq!(cookie_secure_on(), want, "ADMIN_COOKIE_SECURE={val:?}");
        });
    }
}

#[test]
fn backoff_is_exponential_and_capped() {
    assert_eq!(backoff_secs(5), 32);
    assert_eq!(backoff_secs(6), 64);
    assert_eq!(backoff_secs(9), 512);
    assert_eq!(backoff_secs(10), 900);
    assert_eq!(backoff_secs(100), 900);
    assert_eq!(backoff_secs(-1), 900, "nonsense input stays at the cap, never panics");
}

/// The consistency table (C18): `normalize_username` and the login path's own
/// `!username.is_empty() && username_within_cap(...)` check on the TRIMMED value
/// must agree on every case, or a CLI-created account and the login handler could
/// disagree about which usernames are usable (the zombie-account defect).
#[test]
fn normalize_username_agrees_with_login_validity_check() {
    let padded_128 = format!("  {}  ", "a".repeat(128));
    let cases: &[(&str, &str)] = &[
        ("empty", ""),
        ("whitespace-only", "   "),
        ("exactly-128-bytes", &"a".repeat(128)),
        ("129-bytes", &"a".repeat(129)),
        ("padded-under-cap", "  alice  "),
        ("padded-exactly-128-after-trim", &padded_128),
    ];
    for (label, input) in cases {
        let normalize_ok = normalize_username(input).is_ok();
        // Mirrors the OLD inline check `login_submit` used to run directly (now
        // routed through `normalize_username` too) — trim, then the same cap fn.
        let trimmed = input.trim();
        let login_ok = !trimmed.is_empty() && username_within_cap(trimmed);
        assert_eq!(
            normalize_ok, login_ok,
            "{label}: normalize_username()={normalize_ok} login-style check={login_ok} for {input:?}"
        );
    }
}

#[test]
fn normalize_username_trims_and_rejects_empty_or_over_cap() {
    assert_eq!(normalize_username("  alice  ").unwrap(), "alice");
    assert_eq!(normalize_username("bob").unwrap(), "bob");
    assert!(normalize_username("").is_err());
    assert!(normalize_username("   ").is_err());
    assert!(normalize_username(&"a".repeat(129)).is_err());
    assert!(normalize_username(&"a".repeat(128)).is_ok(), "exactly at the cap is accepted");
}

#[test]
fn password_roundtrip() {
    let h = hash_password("hunter2").unwrap();
    assert!(verify_password(&h, "hunter2"), "correct password rejected");
    assert!(!verify_password(&h, "wrong"), "wrong password accepted");
    assert!(!verify_password("not-a-hash", "hunter2"), "garbage hash accepted");
}

// ============================================================================
// Live-Postgres integration: the session-auth matrix. One schema migration per
// test binary; each test uses unique usernames + a unique peer IP so parallel
// tests never share a lockout subject.
// ============================================================================

use testdb::test_pool;

/// Migrates BOTH the asyncevents plane and the admin schema; the DDL runners
/// serialize concurrent callers.
async fn ensure_schema(pool: &PgPool) {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    asyncevents::Plane::new(pool.clone(), dsn)
        .unwrap()
        .migrate()
        .await
        .unwrap();
    let ctx = Context::with_db(pool.clone());
    let admin = Admin::new();
    admin.register(&ctx).unwrap();
    admin.migrate(&ctx).await.unwrap();
}

/// A wired live state: real pool, real durable transport (`emit_tx` appends to
/// `asyncevents.events`).
async fn wired(pool: &PgPool, open: bool, cookie_secure: bool) -> (Context, Arc<AdminState>) {
    wired_with_verifier(pool, open, cookie_secure, Arc::new(ArgonVerifier)).await
}

async fn wired_with_verifier(
    pool: &PgPool,
    open: bool,
    cookie_secure: bool,
    verifier: Arc<dyn PasswordVerifier>,
) -> (Context, Arc<AdminState>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let st = Arc::new(AdminState {
        env: render::env().unwrap(),
        pool: pool.clone(),
        bus: ctx.bus().clone(),
        open,
        cookie_secure,
        trusted: Vec::new(),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_limiter: httpmw::IpLimiter::new("admin_login", 1_000.0, 1_000),
        login_attempt_gc_requests: AtomicU64::new(0),
        verifier,
    });
    (ctx, st)
}

#[derive(Default)]
struct RecordingVerifier {
    calls: Mutex<Vec<(String, String)>>,
}

impl PasswordVerifier for RecordingVerifier {
    fn verify(&self, encoded: &str, password: &str) -> bool {
        self.calls.lock().unwrap().push((encoded.to_string(), password.to_string()));
        false
    }
}

/// Per-test unique suffix (parallel-safe, plus wall-clock so reruns never collide).
static UNIQ: AtomicU32 = AtomicU32::new(0);
fn uniq(prefix: &str) -> String {
    let n = UNIQ.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{prefix}-{t}-{n}")
}

async fn create_user(pool: &PgPool, username: &str, pass: &str) {
    let hash = hash_password(pass).unwrap();
    sqlx::query("INSERT INTO admin.users (username, pass_hash, role) VALUES ($1, $2, 'admin')")
        .bind(username)
        .bind(&hash)
        .execute(pool)
        .await
        .unwrap();
}

/// Test teardown: the user row (sessions CASCADE), its attempt rows, and its
/// durable events.
async fn cleanup_user(pool: &PgPool, username: &str) {
    let _ = sqlx::query("DELETE FROM admin.users WHERE username = $1")
        .bind(username)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM admin.login_attempts WHERE subject = $1")
        .bind(format!("user:{username}"))
        .execute(pool)
        .await;
    let _ = asyncevents::testing::cleanup_events(pool, "actor", username).await;
}

async fn cleanup_ip(pool: &PgPool, ip: &str) {
    let _ = sqlx::query("DELETE FROM admin.login_attempts WHERE subject = $1")
        .bind(format!("ip:{ip}"))
        .execute(pool)
        .await;
}

/// Counts `admin.action` rows by one payload key + the action value. Direct plane
/// SQL is sanctioned in test files (archcheck rule 14b exempts tests).
async fn action_rows(pool: &PgPool, key: &str, value: &str, action: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM asyncevents.events
         WHERE topic = 'admin.action' AND payload->>$1 = $2 AND payload->>'action' = $3",
    )
    .bind(key)
    .bind(value)
    .bind(action)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

/// Builds a form request with the ConnectInfo extension `app::run` injects in
/// production (`into_make_service_with_connect_info`).
fn form_req(method: &str, uri: &str, peer: &str, cookie: Option<&str>, body: Option<String>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let mut req = match body {
        Some(body) => b
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let addr: SocketAddr = peer.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn send(st: &Arc<AdminState>, req: Request<Body>) -> Response {
    router(st.clone()).oneshot(req).await.unwrap()
}

async fn body_string(resp: Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn set_cookie(resp: &Response) -> String {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Extracts the session token from a minting Set-Cookie value.
fn token_of(set_cookie: &str) -> String {
    set_cookie
        .strip_prefix("admin_session=")
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_default()
        .to_string()
}

fn location_of(resp: &Response) -> String {
    resp.headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn post_login(st: &Arc<AdminState>, peer: &str, user: &str, pass: &str) -> Response {
    send(
        st,
        form_req(
            "POST",
            "/admin/login",
            peer,
            None,
            Some(format!("username={user}&password={pass}")),
        ),
    )
    .await
}

async fn csrf_of(pool: &PgPool, token: &str) -> String {
    let (csrf,): (String,) =
        sqlx::query_as("SELECT csrf_token FROM admin.sessions WHERE token = $1")
            .bind(token)
            .fetch_one(pool)
            .await
            .unwrap();
    csrf
}

async fn attempts_row(pool: &PgPool, subject: &str) -> Option<(i32, bool)> {
    sqlx::query_as(
        "SELECT fails, locked_until IS NOT NULL FROM admin.login_attempts WHERE subject = $1",
    )
    .bind(subject)
    .fetch_optional(pool)
    .await
    .unwrap()
}

// ---- the matrix -----------------------------------------------------------------

/// Login success: 303 → /admin, the cookie carries every flag (Secure included by
/// default), the session row exists, and the durable `login-succeeded` row landed.
#[tokio::test(flavor = "multi_thread")]
async fn login_success_mints_session_cookie_and_emits() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-ok");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.10:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&resp), "/admin");
    let sc = set_cookie(&resp);
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/admin", "Max-Age=43200", "Secure"] {
        assert!(sc.contains(flag), "Set-Cookie missing {flag}: {sc}");
    }
    let token = token_of(&sc);
    assert_eq!(token.len(), 43, "32B base64url token");

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM admin.sessions WHERE token = $1 AND username = $2 AND expires_at > now()",
    )
    .bind(&token)
    .bind(&user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "session row minted");
    assert_eq!(action_rows(&pool, "actor", &user, "login-succeeded").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.10").await;
}

/// `ADMIN_COOKIE_SECURE=0` variant: everything identical except no `Secure` flag.
#[tokio::test(flavor = "multi_thread")]
async fn login_cookie_without_secure_when_opted_out() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, false).await;
    let user = uniq("t-insec");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.11:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let sc = set_cookie(&resp);
    assert!(!sc.contains("Secure"), "Secure must be absent when opted out: {sc}");
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/admin", "Max-Age=43200"] {
        assert!(sc.contains(flag), "Set-Cookie missing {flag}: {sc}");
    }

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.11").await;
}

/// No username oracle: wrong password, unknown user, and a LOCKED user answer 401
/// with BYTE-IDENTICAL bodies.
#[tokio::test(flavor = "multi_thread")]
async fn failed_logins_have_identical_generic_401_bodies() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-oracle");
    create_user(&pool, &user, "right").await;
    // A pre-locked user (row planted directly).
    let locked_user = uniq("t-locked");
    create_user(&pool, &locked_user, "right").await;
    sqlx::query(
        "INSERT INTO admin.login_attempts (subject, fails, locked_until)
         VALUES ($1, 5, now() + interval '10 minutes')",
    )
    .bind(format!("user:{locked_user}"))
    .execute(&pool)
    .await
    .unwrap();

    let wrong = post_login(&st, "203.0.113.12:9999", &user, "nope").await;
    let unknown = post_login(&st, "203.0.113.12:9999", &uniq("t-ghost"), "nope").await;
    let locked = post_login(&st, "203.0.113.12:9999", &locked_user, "right").await;

    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(locked.status(), StatusCode::UNAUTHORIZED);
    let (a, b, c) = (
        body_string(wrong).await,
        body_string(unknown).await,
        body_string(locked).await,
    );
    assert_eq!(a, b, "wrong-pass and unknown-user bodies must be identical");
    assert_eq!(a, c, "locked body must be identical too (no lock oracle)");
    assert!(a.contains(GENERIC_LOGIN_ERROR));

    cleanup_user(&pool, &user).await;
    cleanup_user(&pool, &locked_user).await;
    cleanup_ip(&pool, "203.0.113.12").await;
}

/// The user row locks at 5 consecutive fails (emitting `login-locked`), the correct
/// password STILL answers the generic 401 while locked (and does not increment),
/// and after the lock window expires the correct password logs in and resets the
/// attempt rows.
#[tokio::test(flavor = "multi_thread")]
async fn user_locks_at_five_and_unlocks_after_window() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-lock5");
    let subject = format!("user:{user}");
    create_user(&pool, &user, "right").await;

    for i in 0..5 {
        let resp = post_login(&st, "203.0.113.13:9999", &user, "wrong").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "fail #{i}");
    }
    assert_eq!(attempts_row(&pool, &subject).await, Some((5, true)), "locked at 5");
    assert_eq!(action_rows(&pool, "actor", &user, "login-locked").await, 1);

    // Correct password while locked: same generic 401, no further increment.
    let resp = post_login(&st, "203.0.113.13:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(attempts_row(&pool, &subject).await, Some((5, true)), "no increment while locked");

    // Expire the lock window → the correct password succeeds and resets the rows.
    sqlx::query(
        "UPDATE admin.login_attempts SET locked_until = now() - interval '1 second' WHERE subject = $1",
    )
    .bind(&subject)
    .execute(&pool)
    .await
    .unwrap();
    let resp = post_login(&st, "203.0.113.13:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "unlocked after the window");
    assert_eq!(attempts_row(&pool, &subject).await, None, "attempt rows reset on success");

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.13").await;
}

/// The IP row accumulates across usernames but does NOT lock below its 20
/// threshold — a shared office IP isn't bricked by one folk's typos [R2].
#[tokio::test(flavor = "multi_thread")]
async fn ip_row_accumulates_but_does_not_lock_below_twenty() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let ip = "203.0.113.14";
    let peer = format!("{ip}:9999");
    let ghosts: Vec<String> = (0..3).map(|_| uniq("t-ipghost")).collect();

    for g in &ghosts {
        let resp = post_login(&st, &peer, g, "wrong").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        attempts_row(&pool, &format!("ip:{ip}")).await,
        Some((3, false)),
        "ip row counts but is not locked below 20"
    );

    // The same IP can still log a real user in.
    let user = uniq("t-ipok");
    create_user(&pool, &user, "right").await;
    let resp = post_login(&st, &peer, &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    cleanup_user(&pool, &user).await;
    for g in &ghosts {
        cleanup_user(&pool, g).await;
    }
    cleanup_ip(&pool, ip).await;
}

/// An expired session is a miss: page GETs bounce to the login form, POSTs 401.
/// The login page serves 200 with the form (the split-proof `[AD1]` shape).
#[tokio::test(flavor = "multi_thread")]
async fn login_page_serves_the_form_with_security_headers() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;

    let login = send(&st, form_req("GET", "/admin/login", "203.0.113.16:9999", None, None)).await;
    assert_eq!(login.status(), StatusCode::OK);
    let html = body_string(login).await;
    assert!(html.contains(r#"action="/admin/login""#));
    // Security headers ride the admin router.
    // (Checked on a fresh response since `html` consumed the first.)
    let login2 = send(&st, form_req("GET", "/admin/login", "203.0.113.16:9999", None, None)).await;
    assert!(login2.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert_eq!(login2.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(login2.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(login2.headers()[header::REFERRER_POLICY], "no-referrer");
}

/// Logout: CSRF-gated, deletes the session row, clears the cookie, emits the
/// durable `logout`.
#[tokio::test(flavor = "multi_thread")]
async fn logout_deletes_session_clears_cookie_and_emits() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-logout");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.19:9999", &user, "right").await;
    let token = token_of(&set_cookie(&resp));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    // Without _csrf → 403, session intact.
    let bad = send(
        &st,
        form_req("POST", "/admin/logout", "203.0.113.19:9999", Some(&cookie), Some(String::new())),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);

    let good = send(
        &st,
        form_req(
            "POST",
            "/admin/logout",
            "203.0.113.19:9999",
            Some(&cookie),
            Some(format!("_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&good), "/admin/login");
    let sc = set_cookie(&good);
    assert!(sc.contains("Max-Age=0"), "clearing cookie: {sc}");

    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM admin.sessions WHERE token = $1")
        .bind(&token)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "session row deleted");
    assert_eq!(action_rows(&pool, "actor", &user, "logout").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.19").await;
}

/// Zero admin users is a WARNED boot, never a failure: the full lifecycle prefix
/// (`register` → `migrate` → `start`) succeeds against a live DB regardless of the
/// user count — the old `ADMIN_USER` fail-closed env gate is gone.
#[tokio::test(flavor = "multi_thread")]
async fn zero_user_boot_is_allowed() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let ctx = Context::with_db(pool.clone());
    let admin = Admin::new();
    admin.register(&ctx).unwrap();
    {
        // `init` reads process env synchronously; participate in the env-test lock so
        // this lifecycle test cannot observe another test's temporary dev knob.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        admin.init(&ctx).unwrap();
    }
    assert!(!login_reaper_is_owned(&admin), "init must wire without spawning background work");
    admin.migrate(&ctx).await.unwrap();
    admin.start(&ctx).await.expect("start must succeed with zero users (warn only)");
    assert!(login_reaper_is_owned(&admin), "a successful start must own the limiter reaper");
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin), "stop must await and release the limiter reaper");
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_is_idempotent_including_attempt_retention_index() {
    let Some(pool) = test_pool().await else { return };
    let ctx = Context::with_db(pool);
    let admin = Admin::new();
    admin.register(&ctx).unwrap();
    admin.migrate(&ctx).await.unwrap();
    admin.migrate(&ctx).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_and_invalid_usernames_never_create_user_attempt_rows() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let ghost = uniq("t-no-ghost-row");
    let ip = "203.0.113.81";

    let unknown = post_login(&st, &format!("{ip}:9999"), &ghost, "wrong").await;
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    let invalid_name = "x".repeat(129);
    let invalid = post_login(&st, &format!("{ip}:9999"), &invalid_name, "wrong").await;
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(attempts_row(&pool, &format!("user:{ghost}")).await, None);
    assert_eq!(attempts_row(&pool, &format!("user:{invalid_name}")).await, None);
    assert_eq!(attempts_row(&pool, &format!("ip:{ip}")).await, Some((2, false)));
    cleanup_ip(&pool, ip).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_ip_limiter_returns_exact_retry_after() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let st = Arc::new(AdminState {
        env: render::env().unwrap(),
        pool,
        bus: ctx.bus().clone(),
        open: false,
        cookie_secure: true,
        trusted: Vec::new(),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_limiter: httpmw::IpLimiter::new("admin_login", 0.0, 1),
        login_attempt_gc_requests: AtomicU64::new(0),
        verifier: Arc::new(ArgonVerifier),
    });
    let peer = "203.0.113.82:9999";
    assert_eq!(post_login(&st, peer, "ghost", "wrong").await.status(), StatusCode::UNAUTHORIZED);
    let denied = post_login(&st, peer, "ghost", "wrong").await;
    assert_eq!(denied.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(denied.headers()[header::RETRY_AFTER], "1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_failures_stop_exactly_at_user_threshold_and_emit_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-parallel-lock");
    let ip = "203.0.113.83";
    create_user(&pool, &user, "right").await;

    let calls = (0..12).map(|i| {
        let st = st.clone();
        let user = user.clone();
        async move {
            post_login(&st, &format!("{ip}:{}", 9000 + i), &user, "wrong").await.status()
        }
    });
    let statuses = futures::future::join_all(calls).await;
    assert!(statuses.iter().all(|status| *status == StatusCode::UNAUTHORIZED));
    assert_eq!(
        attempts_row(&pool, &format!("user:{user}")).await,
        Some((USER_LOCK_THRESHOLD, true))
    );
    assert_eq!(action_rows(&pool, "actor", &user, "login-locked").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, ip).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verifier_runs_exactly_once_for_every_denial_shape() {
    let Some(pool) = test_pool().await else { return };
    let verifier = Arc::new(RecordingVerifier::default());
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier.clone()).await;
    let known = uniq("t-verify-known");
    let user_locked = uniq("t-verify-userlocked");
    let ip_locked_user = uniq("t-verify-iplocked");
    create_user(&pool, &known, "right").await;
    create_user(&pool, &user_locked, "right").await;
    create_user(&pool, &ip_locked_user, "right").await;
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,locked_until) VALUES ($1,5,now()+interval '10 minutes')")
        .bind(format!("user:{user_locked}"))
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,locked_until) VALUES ($1,20,now()+interval '10 minutes')")
        .bind("ip:203.0.113.94")
        .execute(&pool).await.unwrap();

    let invalid_username = "x".repeat(129);
    let cases = [
        ("203.0.113.90:1", known.as_str(), "known-secret"),
        ("203.0.113.91:1", "unknown-structural", "unknown-secret"),
        ("203.0.113.92:1", user_locked.as_str(), "locked-secret"),
        ("203.0.113.94:1", ip_locked_user.as_str(), "ip-locked-secret"),
        ("203.0.113.95:1", invalid_username.as_str(), "invalid-secret"),
    ];
    for (peer, user, pass) in cases {
        assert_eq!(post_login(&st, peer, user, pass).await.status(), StatusCode::UNAUTHORIZED);
    }
    {
        let calls = verifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 5, "one verifier call per admitted request");
        assert_eq!(calls[0].1, "known-secret");
        for (_, candidate) in &calls[1..] {
            assert_eq!(candidate, "admin-invalid-credentials");
        }
    }

    for user in [&known, &user_locked, &ip_locked_user] { cleanup_user(&pool, user).await; }
    for ip in ["203.0.113.90", "203.0.113.91", "203.0.113.92", "203.0.113.94", "203.0.113.95"] {
        cleanup_ip(&pool, ip).await;
    }
}

struct SlowVerifier;
impl PasswordVerifier for SlowVerifier {
    fn verify(&self, _encoded: &str, _password: &str) -> bool {
        std::thread::sleep(Duration::from_millis(150));
        false
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_password_verify_does_not_stall_runtime_heartbeat() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired_with_verifier(&pool, false, true, Arc::new(SlowVerifier)).await;
    let login = post_login(&st, "203.0.113.96:1", "unknown-heartbeat", "wrong");
    let heartbeat = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        1
    };
    let (response, beat) = tokio::join!(login, heartbeat);
    assert_eq!(beat, 1);
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    cleanup_ip(&pool, "203.0.113.96").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossed_user_and_ip_lock_orders_do_not_deadlock() {
    let Some(pool) = test_pool().await else { return };
    let verifier = Arc::new(RecordingVerifier::default());
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier).await;
    let shared_user = uniq("t-two-ip");
    let user_a = uniq("t-one-ip-a");
    let user_b = uniq("t-one-ip-b");
    for user in [&shared_user, &user_a, &user_b] { create_user(&pool, user, "right").await; }

    let calls = vec![
        post_login(&st, "203.0.113.101:1", &shared_user, "wrong"),
        post_login(&st, "203.0.113.102:1", &shared_user, "wrong"),
        post_login(&st, "203.0.113.103:1", &user_a, "wrong"),
        post_login(&st, "203.0.113.103:2", &user_b, "wrong"),
    ];
    let responses = tokio::time::timeout(Duration::from_secs(3), futures::future::join_all(calls))
        .await.expect("sorted advisory locks must not deadlock");
    assert!(responses.iter().all(|r| r.status() == StatusCode::UNAUTHORIZED));

    for user in [&shared_user, &user_a, &user_b] { cleanup_user(&pool, user).await; }
    for ip in ["203.0.113.101", "203.0.113.102", "203.0.113.103"] { cleanup_ip(&pool, ip).await; }
}

#[tokio::test(flavor = "multi_thread")]
async fn login_attempt_gc_removes_only_stale_unlocked_rows() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let stale = format!("ip:198.51.100.{}", UNIQ.fetch_add(1, Ordering::Relaxed) % 200 + 1);
    let locked = format!("user:{}", uniq("t-gc-locked"));
    let fresh = format!("user:{}", uniq("t-gc-fresh"));
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,updated_at) VALUES ($1,1,now()-interval '25 hours'),($2,5,now()-interval '25 hours'),($3,1,now())")
        .bind(&stale).bind(&locked).bind(&fresh).execute(&pool).await.unwrap();
    sqlx::query("UPDATE admin.login_attempts SET locked_until=now()+interval '1 hour' WHERE subject=$1")
        .bind(&locked).execute(&pool).await.unwrap();
    st.cleanup_login_attempts().await;
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM admin.login_attempts WHERE subject=ANY($1)")
        .bind([&locked, &fresh]).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2);
    assert_eq!(attempts_row(&pool, &stale).await, None);
    sqlx::query("DELETE FROM admin.login_attempts WHERE subject=ANY($1)")
        .bind([stale, locked, fresh]).execute(&pool).await.unwrap();
}

/// A verifier that reports when `verify` has started and then blocks until the test
/// releases it — lets the test freeze a login mid-Argon2 deterministically.
struct GatedVerifier {
    started: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PasswordVerifier for GatedVerifier {
    fn verify(&self, _encoded: &str, _password: &str) -> bool {
        self.started.send(()).expect("test alive");
        let _ = self.release.lock().unwrap().recv();
        false
    }
}

/// The RAM-cap regression: `spawn_blocking` is NOT cancelled when its JoinHandle
/// drops, so if the argon permit lived in the handler's async frame a client
/// disconnect would release it while the detached 64 MiB hash keeps running. The
/// permit must be owned by the blocking closure — released only AFTER the hash
/// completes, even when the caller future is dropped mid-verify.
#[tokio::test(flavor = "multi_thread")]
async fn argon_permit_survives_login_cancellation_until_hash_completes() {
    let Some(pool) = test_pool().await else { return };
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let verifier = Arc::new(GatedVerifier {
        started: started_tx,
        release: Mutex::new(release_rx),
    });
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier).await;
    assert_eq!(st.argon_permits.available_permits(), 2);

    let task_st = st.clone();
    let login = tokio::spawn(async move {
        post_login(&task_st, "203.0.113.97:1", "unknown-cancelled", "wrong").await
    });
    // Wait until the login is provably inside the blocking verify.
    tokio::task::spawn_blocking(move || started_rx.recv().expect("verify started"))
        .await
        .unwrap();
    assert_eq!(st.argon_permits.available_permits(), 1);

    // Simulate the client disconnect: abort drops the handler future at its
    // `.await` on the spawn_blocking JoinHandle; the blocking hash keeps running.
    login.abort();
    let err = login.await.expect_err("login task was aborted mid-verify");
    assert!(err.is_cancelled());
    assert_eq!(
        st.argon_permits.available_permits(),
        1,
        "cancelling the request must NOT release the argon permit while the hash still runs"
    );

    // Let the hash finish; only then may the permit return.
    release_tx.send(()).expect("verifier still blocked");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while st.argon_permits.available_permits() != 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "permit was not released after the blocking verify completed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cleanup_ip(&pool, "203.0.113.97").await;
}

// ============================================================================
// Control-plane panels: the list-column fan-out through `GET /admin/_panel/..`,
// the write affordance and a ColumnCells answer in `panels::resolve`. Every
// module is its own source, routed by the production manifest refresh.
// ============================================================================

use std::collections::BTreeMap;

use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, ContributionDecl, DataTable, Emphasis, EntityRef, InvokeCall,
    InvokeOutcome, Manifest, PageInfo, PanelBody, PanelQuery, Point, Primitive, PropertyList, Role, Row, SearchHit,
    SearchQuery, Source, Then,
};

use crate::engine::manifests::{refresh_once, Snapshot};
use crate::engine::panels::{self, PanelState};
use crate::engine::roles::Operator;
use crate::engine::Engine;
use crate::golden_tests::table_grid;

#[derive(Clone)]
enum Answer {
    Body(Box<Primitive>),
    Fail(opsapi::Error),
}

fn answer(primitive: Primitive) -> Answer {
    Answer::Body(Box::new(primitive))
}

/// A provider that records every `PanelQuery` it is asked and answers by contribution id.
struct Recording {
    manifest: Manifest,
    answers: Mutex<BTreeMap<String, Answer>>,
    calls: Mutex<Vec<PanelQuery>>,
}

impl Recording {
    fn new(manifest: Manifest, answers: &[(&str, Answer)]) -> Arc<Recording> {
        let answers = answers.iter().map(|(id, a)| (id.to_string(), a.clone())).collect();
        Arc::new(Recording { manifest, answers: Mutex::new(answers), calls: Mutex::new(Vec::new()) })
    }

    fn answer(&self, contribution: &str, answer: Answer) {
        self.answers.lock().unwrap().insert(contribution.to_string(), answer);
    }

    fn calls(&self) -> Vec<PanelQuery> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl controlplane::Provider for Recording {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        self.calls.lock().unwrap().push(req.clone());
        match self.answers.lock().unwrap().get(&req.contribution).cloned() {
            Some(Answer::Body(primitive)) => Ok(PanelBody::new(*primitive)),
            Some(Answer::Fail(error)) => Err(error),
            None => Err(opsapi::Error::internal(format!("no answer for {}", req.contribution))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, _req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        Err(opsapi::Error::forbidden("a recording provider writes nothing"))
    }
}

fn contribution(id: &str, point: Point, min_role: Role) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order: 0,
        title: id.to_string(),
        icon: String::new(),
        min_role,
        cache_ttl_s: None,
        deadline_ms: 0,
    }
}

fn list_column(id: &str, kind: &str, label: &str, min_role: Role, order: i32) -> ContributionDecl {
    let point = Point::EntityListColumn { kind: kind.to_string(), label: label.to_string(), align: Align::Left };
    ContributionDecl { order, ..contribution(id, point, min_role) }
}

fn module(name: &str, contributions: Vec<ContributionDecl>, actions: Vec<ActionDecl>, writes: bool) -> Manifest {
    Manifest { module: name.to_string(), contributions, actions, writes, ..Manifest::default() }
}

fn action_decl(id: &str, point: ActionPoint, implemented: bool) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Viewer,
        then: Then::Invoke,
        implemented,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        when_empty: false,
        prepare: false,
    }
}

fn text(value: &str) -> Cell {
    Cell { kind: CellKind::Text, value: value.to_string(), tone: None, link: None, bar: None, more: Vec::new() }
}

fn table_row(entity: Option<(&str, &str)>, name: &str) -> Row {
    Row {
        entity: entity.map(|(kind, id)| EntityRef { kind: kind.to_string(), id: id.to_string() }),
        cells: vec![text(name), text("active")],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// A two-column table whose `player` list columns go after its first column.
fn players_table(rows: Vec<Row>) -> Primitive {
    let column = |key: &str, label: &str| controlplane::Column {
        key: key.to_string(),
        label: label.to_string(),
        kind: CellKind::Text,
        sortable: false,
        align: Align::Left,
        sorted: None,
    };
    Primitive::DataTable(DataTable {
        columns: vec![column("name", "Name"), column("status", "Status")],
        rows,
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: Some("player".to_string()),
        list_columns_at: 1,
        item_noun: String::new(),
    })
}

/// Two players, a guild and a row with no entity, in that order around the guild.
fn mixed_rows() -> Vec<Row> {
    vec![
        table_row(Some(("player", "p-1")), "Anna"),
        table_row(Some(("guild", "g-7")), "Night Owls"),
        table_row(None, "system"),
        table_row(Some(("player", "p-2")), "Bjorn"),
    ]
}

/// Each module behind its own source: `players` serves the table page, `levels` (Level),
/// `guilds` (Guild) and `secrets` (Secret, admin only) are `player` list columns, and
/// `members` (Members) is a `guild` list column. Level answers `p-1`, and `g-7` too, which
/// no `player` row may draw.
struct Fleet {
    engine: Arc<Engine>,
    players: Arc<Recording>,
    levels: Arc<Recording>,
    guilds: Arc<Recording>,
    secrets: Arc<Recording>,
    members: Arc<Recording>,
}

impl Fleet {
    async fn new(table: Primitive, guild: Answer, players_write: bool, players_actions: Vec<ActionDecl>) -> Fleet {
        let page = contribution("list", Point::Page("players".to_string()), Role::Viewer);
        let players = Recording::new(
            module("players", vec![page], players_actions, players_write),
            &[("list", answer(table))],
        );
        let levels_cells = BTreeMap::from([("p-1".to_string(), text("12")), ("g-7".to_string(), text("99"))]);
        let levels = Recording::new(
            module("levels", vec![list_column("level", "player", "Level", Role::Viewer, 0)], Vec::new(), false),
            &[("level", answer(Primitive::ColumnCells(levels_cells)))],
        );
        let guilds = Recording::new(
            module("guilds", vec![list_column("guild", "player", "Guild", Role::Viewer, 1)], Vec::new(), false),
            &[("guild", guild)],
        );
        let secrets = Recording::new(
            module("secrets", vec![list_column("secret", "player", "Secret", Role::Admin, 2)], Vec::new(), false),
            &[("secret", answer(Primitive::ColumnCells(BTreeMap::new())))],
        );
        let members = Recording::new(
            module("members", vec![list_column("members", "guild", "Members", Role::Viewer, 3)], Vec::new(), false),
            &[("members", answer(Primitive::ColumnCells(BTreeMap::new())))],
        );
        let sources = [&players, &levels, &guilds, &secrets, &members]
            .iter()
            .map(|r| {
                let provider: Arc<dyn controlplane::Provider> = Arc::clone(r) as Arc<dyn controlplane::Provider>;
                Source {
                    peer: format!("{}-svc", r.manifest.module),
                    face: Arc::new(controlplane::LocalFace::new(vec![provider])),
                }
            })
            .collect();
        let engine = Arc::new(Engine::new());
        engine.install(Snapshot::seeded(sources, None));
        refresh_once(&engine).await;
        assert_eq!(engine.snapshot().modules.len(), 5, "every source's manifest is routed");
        Fleet { engine, players, levels, guilds, secrets, members }
    }

    async fn get(&self, st: &Arc<AdminState>, cookie: &str, uri: &str) -> (StatusCode, String) {
        let portal = crate::routes::Portal { st: st.clone(), engine: self.engine.clone() };
        let req = Request::builder().uri(uri).header("cookie", cookie).body(Body::empty()).unwrap();
        let resp = crate::routes::router(portal).oneshot(req).await.unwrap();
        let status = resp.status();
        (status, body_string(resp).await)
    }
}

/// An `operator` session cookie for a fresh user, who is below `secrets`' admin column.
async fn operator_cookie(pool: &PgPool, user: &str) -> String {
    sqlx::query("INSERT INTO admin.users (username, pass_hash, role) VALUES ($1, 'unused', 'operator')")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    let token = uniq("tok");
    sqlx::query(
        "INSERT INTO admin.sessions (token, username, csrf_token, expires_at)
         VALUES ($1, $2, 'csrf', now() + interval '1 hour')",
    )
    .bind(&token)
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
    format!("{SESSION_COOKIE}={token}")
}

const PLAYERS_PANEL: &str = "/admin/_panel/players/list";
const GUILD_MARKER: &str = r#"<span class="muted" title="guilds module unavailable">"#;

/// The page as an operator sees it with Guild failing as `guild`: one call per allowed
/// `player` column with the page's player ids in row order, none for the admin-only or the
/// `guild` column, Level's cell at `list_columns_at` for the row it answered and empty
/// elsewhere, and Guild's marker in every row with exactly one warn banner.
async fn assert_one_fan_out_with_guild_failing(guild: Answer, why: &str) {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-cols");
    let cookie = operator_cookie(&pool, &user).await;
    let fleet = Fleet::new(players_table(mixed_rows()), guild, true, Vec::new()).await;

    let (status, raw) = fleet.get(&st, &cookie, PLAYERS_PANEL).await;
    assert_eq!(status, StatusCode::OK, "{why}: {raw}");
    assert!(raw.contains(r#"data-state="ok""#), "{why}: {raw}");
    let refs = vec!["p-1".to_string(), "p-2".to_string()];
    for (name, column) in [("levels", &fleet.levels), ("guilds", &fleet.guilds)] {
        let calls = column.calls();
        assert_eq!(calls.len(), 1, "{why}: {name} is called once per page: {calls:?}");
        assert_eq!(calls[0].refs, refs, "{why}: {name} gets the player ids in row order");
        assert_eq!(calls[0].entity, None, "{why}: {name}");
    }
    assert!(fleet.secrets.calls().is_empty(), "{why}: a column above the operator's role is never called");
    assert!(fleet.members.calls().is_empty(), "{why}: a column of another kind is never called");
    assert_eq!(fleet.players.calls().len(), 1, "{why}");
    assert!(fleet.players.calls()[0].refs.is_empty(), "{why}: a page panel's own query carries no refs");

    let (heads, rows) = table_grid(&raw);
    assert_eq!(heads, ["Name", "<span>Level</span>", "<span>Guild</span>", "Status"], "{why}");
    assert_eq!(rows.len(), 4, "{why}");
    assert_eq!(rows[0][1], "12", "{why}: the answered row's cell");
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            assert_eq!(row[1], "", "{why}: row {i} has no Level cell: {row:?}");
        }
        assert!(row[2].starts_with(GUILD_MARKER), "{why}: row {i} draws Guild's marker: {row:?}");
    }
    assert_eq!(raw.matches(r#"<div class="banner warn">"#).count(), 1, "{why}: {raw}");
    assert!(raw.contains("Guild column unavailable — guilds module did not answer."), "{why}: {raw}");
    assert!(!raw.contains("Secret") && !raw.contains("Members"), "{why}: {raw}");

    cleanup_user(&pool, &user).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_columns_fan_out_once_per_page_and_a_failing_column_marks_every_row() {
    assert_one_fan_out_with_guild_failing(
        Answer::Fail(opsapi::Error::unavailable("guild store down")),
        "a column answering an error",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_list_column_answering_another_primitive_fails_like_an_error() {
    let cell = answer(Primitive::Cell(text("Night Owls")));
    assert_one_fan_out_with_guild_failing(cell, "a column answering a Cell").await;
}

/// A readonly table page fans out too; an empty table and a table with no row of the
/// columns' kind make no column call.
#[tokio::test(flavor = "multi_thread")]
async fn list_columns_fan_out_on_a_readonly_page_and_not_without_refs() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-cols-ro");
    let cookie = operator_cookie(&pool, &user).await;
    let guild_ok = || answer(Primitive::ColumnCells(BTreeMap::new()));

    let ban = action_decl("ban", ActionPoint::TableRow("list".to_string()), true);
    let readonly = Fleet::new(players_table(mixed_rows()), guild_ok(), false, vec![ban]).await;
    let (_, raw) = readonly.get(&st, &cookie, PLAYERS_PANEL).await;
    assert!(raw.contains(r#"data-state="readonly""#), "{raw}");
    assert_eq!(readonly.levels.calls().len(), 1, "a readonly table still draws its list columns");
    assert_eq!(readonly.guilds.calls().len(), 1);
    assert_eq!(table_grid(&raw).0, ["Name", "<span>Level</span>", "<span>Guild</span>", "Status"]);

    let empty = Fleet::new(players_table(Vec::new()), guild_ok(), true, Vec::new()).await;
    let (_, raw) = empty.get(&st, &cookie, PLAYERS_PANEL).await;
    assert!(raw.contains(r#"data-state="empty""#), "{raw}");
    assert!(empty.levels.calls().is_empty() && empty.guilds.calls().is_empty(), "an empty table calls no column");

    let no_players = vec![table_row(Some(("guild", "g-7")), "Night Owls"), table_row(None, "system")];
    let unrelated = Fleet::new(players_table(no_players), guild_ok(), true, Vec::new()).await;
    let (_, raw) = unrelated.get(&st, &cookie, PLAYERS_PANEL).await;
    assert!(raw.contains(r#"data-state="ok""#), "{raw}");
    assert!(unrelated.levels.calls().is_empty() && unrelated.guilds.calls().is_empty(), "no player row, no call");
    assert!(unrelated.members.calls().is_empty(), "a guild row does not call the guild column of a player table");

    cleanup_user(&pool, &user).await;
}

/// A declared list column's own panel URL is refused for an operator its role admits,
/// without calling it; an undeclared id is refused too.
#[tokio::test(flavor = "multi_thread")]
async fn a_list_columns_own_panel_url_is_not_found_and_never_called() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-cols-404");
    let cookie = operator_cookie(&pool, &user).await;
    let guild_ok = answer(Primitive::ColumnCells(BTreeMap::new()));
    let fleet = Fleet::new(players_table(mixed_rows()), guild_ok, true, Vec::new()).await;

    let (status, raw) = fleet.get(&st, &cookie, "/admin/_panel/levels/level").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
    assert!(raw.contains("is a list column; its table draws it"), "{raw}");
    assert!(fleet.levels.calls().is_empty(), "the refused column is never called");

    let (status, raw) = fleet.get(&st, &cookie, "/admin/_panel/levels/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
    assert!(fleet.levels.calls().is_empty());

    cleanup_user(&pool, &user).await;
}

/// `refs` crosses the wire: a split column provider receives the ids it is asked for.
#[test]
fn panel_query_refs_survive_the_wire() {
    let query = PanelQuery {
        module: "levels".to_string(),
        contribution: "level".to_string(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: vec!["p-2".to_string(), "p-1".to_string()],
    };
    let back: PanelQuery = serde_json::from_str(&serde_json::to_string(&query).unwrap()).unwrap();
    assert_eq!(back, query);
    assert_eq!(back.refs, ["p-2", "p-1"]);
}

// ---- panels::resolve --------------------------------------------------------------

/// One module `m` behind one source serving `decl` with `actions`, answering `answer`.
async fn single(
    decl: ContributionDecl,
    actions: Vec<ActionDecl>,
    writes: bool,
    answer: Answer,
) -> (Engine, Arc<Recording>) {
    let id = decl.id.clone();
    let rec = Recording::new(module("m", vec![decl], actions, writes), &[(id.as_str(), answer)]);
    let provider: Arc<dyn controlplane::Provider> = rec.clone();
    let source = Source { peer: "m-svc".to_string(), face: Arc::new(controlplane::LocalFace::new(vec![provider])) };
    let engine = Engine::new();
    engine.install(Snapshot::seeded(vec![source], None));
    refresh_once(&engine).await;
    (engine, rec)
}

async fn resolve(engine: &Engine, decl: &ContributionDecl) -> panels::PanelView {
    let query = PanelQuery {
        module: "m".to_string(),
        contribution: decl.id.clone(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    };
    let operator = Operator { login: "ops".to_string(), role: Role::Admin };
    panels::resolve(engine, &engine.snapshot(), &operator, "m", decl, query).await
}

/// On a module that does not write, an implemented `TableRow` or `PanelTool` action that is
/// not a link makes its table panel readonly; the same action with `implemented: false`
/// leaves it ok.
#[tokio::test]
async fn an_unimplemented_row_or_tool_action_is_no_write_affordance() {
    let decl = contribution("list", Point::Page("players".to_string()), Role::Viewer);
    let points = [ActionPoint::TableRow("list".to_string()), ActionPoint::PanelTool("list".to_string())];
    for point in points {
        for (implemented, want) in [(false, PanelState::Ok), (true, PanelState::Readonly)] {
            let action = action_decl("ban", point.clone(), implemented);
            let table = answer(players_table(mixed_rows()));
            let (engine, _) = single(decl.clone(), vec![action], false, table).await;
            let view = resolve(&engine, &decl).await;
            assert_eq!(view.state, want, "{point:?} implemented: {implemented}");
        }
    }
}

/// A page panel answering `ColumnCells` is an error with nothing cached; with a cached
/// copy it is stale on that copy, the error kept.
#[tokio::test]
async fn a_page_panel_answering_column_cells_is_an_error_or_stale_on_its_cache() {
    let cells = || answer(Primitive::ColumnCells(BTreeMap::from([("p-1".to_string(), text("12"))])));
    let cold = contribution("summary", Point::Page("players".to_string()), Role::Viewer);
    let (engine, rec) = single(cold.clone(), Vec::new(), true, cells()).await;
    let view = resolve(&engine, &cold).await;
    assert_eq!(view.state, PanelState::Error);
    assert!(view.body.is_none());
    assert!(view.error.as_deref().is_some_and(|e| e.contains("answered ColumnCells")), "{:?}", view.error);
    assert_eq!(rec.calls().len(), 1);

    let warm = ContributionDecl { cache_ttl_s: Some(3600), ..cold };
    let props = Primitive::PropertyList(PropertyList {
        items: vec![("Level".to_string(), text("12"))],
        columns: 1,
        item: None,
        code: None,
    });
    let (engine, rec) = single(warm.clone(), Vec::new(), true, answer(props.clone())).await;
    assert_eq!(resolve(&engine, &warm).await.state, PanelState::Ok, "the first answer is cached");
    rec.answer("summary", cells());
    let view = resolve(&engine, &warm).await;
    assert_eq!(view.state, PanelState::Stale);
    assert_eq!(view.body.map(|b| b.primitive), Some(props), "the cached copy, never the cells");
    assert!(view.error.as_deref().is_some_and(|e| e.contains("answered ColumnCells")), "{:?}", view.error);
}

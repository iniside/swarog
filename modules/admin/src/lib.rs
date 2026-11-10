//! `admin` — the GameOps admin module: the operator identity, sessions and login.
//!
//! ## GameOps identity (session auth — replaces the old Basic-auth gate)
//!
//! The module owns schema **`admin`**: `admin.users` (argon2id PHC hashes, minted by
//! the `adminctl` operator CLI — [`USERS_DDL`] is `pub` so the CLI executes the SAME
//! DDL), `admin.sessions` (opaque token + per-session CSRF token, 12h TTL, cookie
//! `admin_session`: HttpOnly + SameSite=Strict + Path=/admin, `Secure` unless the
//! dev knob `ADMIN_COOKIE_SECURE=0` opts out — loud warn), and `admin.login_attempts`
//! (asymmetric lockout: a `user:<name>` row locks after 5 consecutive fails, an
//! `ip:<addr>` row after 20, backoff `least(2^fails, 900)` seconds; the client IP is
//! resolved trusted-proxy-aware via `core/httpmw` + `TRUSTED_PROXY_CIDRS`). Every
//! failed login — wrong password, unknown user, locked — answers the SAME generic
//! 401 body: no status/body/timing username oracle (unknown users still burn one
//! argon2 verify against a dummy hash).
//!
//! `POST /admin/logout` requires a `_csrf` field matching the session's CSRF token.
//!
//! Durable audit trail: `admin.action` (`adminevents::ACTION`) is emitted via
//! `emit_tx` for `login-succeeded` / `login-locked` (user-row threshold) / `logout`,
//! each atomic with its own domain write.
//!
//! `ADMIN_OPEN=1` (explicit-only dev knob, loud warn) disables sessions AND CSRF —
//! a deliberately open local portal. Zero admin users is a WARNED boot, not a
//! failure: run `./install.sh` (adminctl) to mint one.
//!
//! Routes (mounted via `ctx.mount`, security headers on this router only):
//! `GET /admin/theme.css`, `GET /admin/admin.js`, `GET /admin/htmx.min.js` (ungated),
//! `GET|POST /admin/login`, `POST /admin/logout`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use bus::{AnyTx, Bus};
use ipnet::IpNet;
use lifecycle::{Context, Module};
use rand::RngCore as _;
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub mod conformance;
mod password;
pub use password::{hash_password, verify_password};

/// The admin page template (adapted to minijinja). Named with a `.html` suffix so
/// minijinja auto-escapes value interpolations (player-supplied text in tables, the
/// session-derived `_csrf` value).
const TEMPLATE: &str = include_str!("admin.html.tmpl");

/// The login page (same theme, `.html` for auto-escape of the error line).
const LOGIN_TEMPLATE: &str = include_str!("login.html.tmpl");

/// The modal fragment template (`.html` for auto-escape). Rendered for
/// `GET /admin/{slug}?partial=modal` on an htmx request — modal chrome only, NO page
/// shell (the step-3 visual layer restyles it).
const MODAL_TEMPLATE: &str = include_str!("modal.html.tmpl");

/// The embedded dark GameOps theme. Served ungated at `/admin/theme.css`.
const THEME_CSS: &str = include_str!("theme.css");

/// The portal's only client script (vanilla: kebab-menu delegation + modal dismiss).
/// Served ungated at `/admin/admin.js`, same-origin so the strict CSP permits it.
const ADMIN_JS: &str = include_str!("admin.js");

/// Vendored, pinned htmx (2.0.4) — a single minified same-origin file (no npm/build
/// step). Drives the modal fragment swaps declaratively via `hx-*` attributes (no
/// `hx-on:`, which would need eval). Served ungated at `/admin/htmx.min.js`.
const HTMX_JS: &str = include_str!("htmx.min.js");

/// The session cookie name.
const SESSION_COOKIE: &str = "admin_session";

/// Session lifetime: 12 hours, mirrored in the cookie's `Max-Age` and the row's
/// `expires_at`.
const SESSION_TTL_SECS: i64 = 43_200;

/// Consecutive-failure thresholds — asymmetric: the per-user row locks first (5),
/// the per-IP row is a coarse many-usernames sweep net (20).
const USER_LOCK_THRESHOLD: i32 = 5;
const IP_LOCK_THRESHOLD: i32 = 20;

/// Lockout backoff ceiling: `least(2^fails, 900)` seconds.
const MAX_BACKOFF_SECS: i64 = 900;

/// Byte caps on the login form inputs, enforced BEFORE any argon2 work (the hash's
/// cost scales with input length — an unauthenticated caller must not choose it).
pub(crate) const MAX_USERNAME_BYTES: usize = 128;
pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;

/// The SHARED cap checks — the login handler and factual conformance probes
/// call these same pure fns, so the probe
/// proves what the handler actually enforces, never a const compared to itself.
/// Byte counts (`str::len()`), not characters. Username emptiness is checked
/// separately by the handler (a different rejection, not a cap).
pub(crate) fn username_within_cap(username: &str) -> bool {
    username.len() <= MAX_USERNAME_BYTES
}

pub(crate) fn password_within_cap(password: &str) -> bool {
    password.len() <= MAX_PASSWORD_BYTES
}

/// The ONE authority for turning raw username input into the value that gets
/// bound to `admin.users.username` — trims, then cap-checks the TRIMMED value
/// against [`MAX_USERNAME_BYTES`] via [`username_within_cap`]. Shared by the login
/// handler (`login_submit`, below) and every `tools/adminctl` mutation that binds a
/// username (`create_user`, `delete_user`), so a CLI-created account can never store
/// a username the login path would then reject as empty/over-cap — the zombie
/// account defect this fn closes. `pub` (not `pub(crate)`): `adminctl` is a
/// different crate and is the ONLY enforcement point for `install.sh`/`install.ps1`,
/// which pass argv straight through.
pub fn normalize_username(input: &str) -> Result<String, &'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("username must not be empty");
    }
    if !username_within_cap(trimmed) {
        return Err("username exceeds the 128-byte cap");
    }
    Ok(trimmed.to_string())
}

/// The ONE body every failed login answers with — wrong password, unknown user, and
/// locked are indistinguishable (no username/lock oracle).
const GENERIC_LOGIN_ERROR: &str = "Invalid credentials.";

/// How long a show-once reveal stays redeemable, enforced as a `created_at` predicate
/// in SQL, not an in-process clock.
const REVEAL_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Schema — owned by this module (migrate touches ONLY schema `admin`).
// ---------------------------------------------------------------------------

const SCHEMA_DDL: &str = "CREATE SCHEMA IF NOT EXISTS admin;";

/// The `admin.users` DDL — `pub` on purpose: `tools/adminctl` (the operator CLI
/// that mints admin users on a fresh database) executes this SAME const before its
/// upsert, so the installer and the module can never drift on the table shape.
pub const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS admin.users (
	username   text PRIMARY KEY,
	pass_hash  text NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now()
);"#;

/// Sessions + login-attempt bookkeeping (module-private tables).
///
/// LOCK ORDER (deliberate): `login_attempts` is created BEFORE `sessions`, matching
/// the order the login write path acquires table locks (`authenticate_and_mint`
/// DELETEs `login_attempts` before INSERTing `sessions`). A re-run of this idempotent
/// DDL (`CREATE INDEX IF NOT EXISTS` still takes a ShareLock even on an existing
/// index) therefore acquires the two tables in the SAME order as a concurrent login,
/// so the two transactions can only serialize — never form a deadlock cycle. The
/// reverse order deadlocked a live login against a concurrent migrate under parallel
/// test load (login killed → 500, or migrate killed → "deadlock detected").
const AUTH_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS admin.login_attempts (
	subject      text PRIMARY KEY,   -- 'user:<name>' | 'ip:<addr>'
	fails        int  NOT NULL DEFAULT 0,
	locked_until timestamptz,
	updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS admin_login_attempts_updated_idx
ON admin.login_attempts(updated_at);
CREATE TABLE IF NOT EXISTS admin.sessions (
	token      text PRIMARY KEY,
	username   text NOT NULL REFERENCES admin.users(username) ON DELETE CASCADE,
	csrf_token text NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS admin_sessions_expires_idx ON admin.sessions(expires_at);"#;

/// Show-once reveal redemption store. Shared Postgres, NOT process memory, so a
/// follow-up GET can LB-route to ANY replica.
const REVEAL_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS admin.reveals (
	token      text PRIMARY KEY,
	reveal     text NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS admin_reveals_created_idx ON admin.reveals(created_at);"#;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The admin portal module. `register` (phase 1) captures the pool + bus; `init`
/// (phase 2, wiring only — no I/O) compiles the templates, reads the dev knobs, and
/// mounts the router; `migrate` owns schema `admin`; `start` warns on a zero-user
/// boot (the first I/O).
#[derive(Default)]
pub struct Admin {
    deps: OnceLock<Deps>,
    /// Created during wiring, shared with the router, and retained here so lifecycle
    /// `start` can launch its idle-bucket reaper only after every fallible startup
    /// check has succeeded.
    login_limiter: OnceLock<Arc<httpmw::IpLimiter>>,
    /// Owned lifecycle task: `start` installs exactly one handle; `stop` takes, aborts,
    /// and awaits it, leaving the same module instance restartable after normal teardown
    /// or a later module's start-unwind.
    login_reaper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Phase-1 captures, shared into the [`AdminState`] built at `init`.
struct Deps {
    pool: PgPool,
    bus: Arc<Bus>,
}

impl Admin {
    pub fn new() -> Self {
        Admin::default()
    }

    fn deps(&self) -> anyhow::Result<&Deps> {
        self.deps
            .get()
            .ok_or_else(|| anyhow::anyhow!("admin.register must run before this phase"))
    }

    fn start_login_reaper(&self) -> anyhow::Result<()> {
        let login_limiter = self
            .login_limiter
            .get()
            .ok_or_else(|| anyhow::anyhow!("admin.init must run before start"))?;
        let mut reaper = self
            .login_reaper
            .lock()
            .map_err(|_| anyhow::anyhow!("admin login reaper lock poisoned"))?;
        if reaper.is_none() {
            *reaper = Some(login_limiter.spawn_eviction_task());
        }
        Ok(())
    }

    async fn stop_login_reaper(&self) -> anyhow::Result<()> {
        let reaper = self
            .login_reaper
            .lock()
            .map_err(|_| anyhow::anyhow!("admin login reaper lock poisoned"))?
            .take();
        let Some(reaper) = reaper else {
            return Ok(());
        };
        reaper.abort();
        match reaper.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(anyhow::anyhow!("admin login reaper failed: {error}")),
        }
    }
}

#[async_trait::async_trait]
impl Module for Admin {
    fn name(&self) -> &str {
        "admin"
    }

    /// Phase 1: captures the shared pool + bus. The admin now OWNS state (schema
    /// `admin`), so a process hosting it must be DB-backed.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("admin requires a DB pool (schema admin)"))?
            .clone();
        self.deps
            .set(Deps {
                pool,
                bus: ctx.bus().clone(),
            })
            .map_err(|_| anyhow::anyhow!("admin.register ran twice"))?;
        Ok(())
    }

    /// Creates this module's own schema (users / sessions / login_attempts).
    /// Idempotent; `USERS_DDL` is the same const `adminctl` executes.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("admin requires a DB pool (schema admin)"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        sqlx::raw_sql(USERS_DDL).execute(pool).await?;
        sqlx::raw_sql(AUTH_DDL).execute(pool).await?;
        sqlx::raw_sql(REVEAL_DDL).execute(pool).await?;
        Ok(())
    }

    /// Wiring only, no I/O: compiles the templates, reads the dev knobs
    /// (`ADMIN_OPEN`, `ADMIN_COOKIE_SECURE` — explicit opt-outs, loud warns) and the
    /// trusted-proxy set (`TRUSTED_PROXY_CIDRS`, same helpers the app-level rate
    /// limiter uses), and mounts the `/admin` routes with the security-headers layer
    /// applied to THIS router only. Session/user reads happen per request in the
    /// handlers, never here.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let deps = self.deps()?;
        let env = template_env()?;

        let open = admin_open_explicitly_on();
        if open {
            tracing::warn!(
                "admin portal is UNAUTHENTICATED (ADMIN_OPEN=1) — sessions AND CSRF disabled; intended for local use only"
            );
        }
        let cookie_secure = cookie_secure_on();
        if !cookie_secure {
            tracing::warn!(
                "admin session cookie is NOT Secure (ADMIN_COOKIE_SECURE=0) — dev/proof opt-out, never production"
            );
        }
        let trusted = httpmw::parse_cidrs(&std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default())
            .map_err(|e| anyhow::anyhow!("admin: parse TRUSTED_PROXY_CIDRS: {e}"))?;

        let login_limiter = httpmw::IpLimiter::new("admin_login", 5.0, 20);
        self.login_limiter
            .set(login_limiter.clone())
            .map_err(|_| anyhow::anyhow!("admin.init ran twice"))?;

        let state = Arc::new(AdminState {
            env,
            pool: deps.pool.clone(),
            bus: deps.bus.clone(),
            open,
            cookie_secure,
            trusted,
            login_slots: Arc::new(Semaphore::new(32)),
            argon_permits: Arc::new(Semaphore::new(2)),
            login_limiter,
            login_attempt_gc_requests: AtomicU64::new(0),
            verifier: Arc::new(ArgonVerifier),
        });
        ctx.mount(router(state));
        Ok(())
    }

    /// First I/O: a zero-user table is a WARNED boot (the operator runs
    /// `./install.sh` / `adminctl create-user`), never a startup failure — the old
    /// `ADMIN_USER` fail-closed env gate is gone. Also forces the argon2
    /// `DUMMY_HASH` LazyLock on a `spawn_blocking` thread (#8: first I/O/CPU
    /// belongs here), so the first unknown-user login never pays the 64 MiB
    /// argon2id init cost on an async Tokio worker.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(|| {
            std::sync::LazyLock::force(&DUMMY_HASH);
        })
        .await?;

        let deps = self.deps()?;
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM admin.users")
            .fetch_one(&deps.pool)
            .await?;
        if n == 0 {
            tracing::warn!(
                "admin: no admin users exist — run ./install.sh (tools/adminctl create-user) to mint one; every login will fail until then"
            );
        }

        // Keep this LAST: the module owns the returned task until `stop`, while an error
        // in this module's own `start` is not followed by `stop`. A later module's start
        // failure does stop this successfully-started prefix, aborting the task below.
        self.start_login_reaper()?;
        Ok(())
    }

    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.stop_login_reaper().await
    }
}

/// Compiles the two embedded templates (shared by `init` and the tests).
fn template_env() -> anyhow::Result<minijinja::Environment<'static>> {
    let mut env = minijinja::Environment::new();
    env.add_template("admin.html", TEMPLATE)
        .map_err(|e| anyhow::anyhow!("admin: template compile: {e}"))?;
    env.add_template("login.html", LOGIN_TEMPLATE)
        .map_err(|e| anyhow::anyhow!("admin: login template compile: {e}"))?;
    env.add_template("modal.html", MODAL_TEMPLATE)
        .map_err(|e| anyhow::anyhow!("admin: modal template compile: {e}"))?;
    Ok(env)
}

/// Per-request admin state captured by the router closures. The pool backs the
/// per-request session check + login flow; the bus appends the durable
/// `admin.action` trail.
struct AdminState {
    env: minijinja::Environment<'static>,
    pool: PgPool,
    bus: Arc<Bus>,
    /// `ADMIN_OPEN=1`: sessions AND CSRF disabled (deliberately open local portal).
    open: bool,
    /// Cookie `Secure` flag (default ON; `ADMIN_COOKIE_SECURE=0` opts out).
    cookie_secure: bool,
    /// Trusted-proxy CIDRs for the client-IP walk (lockout `ip:<addr>` subject).
    trusted: Vec<IpNet>,
    login_slots: Arc<Semaphore>,
    argon_permits: Arc<Semaphore>,
    login_limiter: Arc<httpmw::IpLimiter>,
    /// Request-path cadence only for bounded stale `login_attempts` row GC. Limiter
    /// bucket reclamation belongs exclusively to `IpLimiter`'s background reaper.
    login_attempt_gc_requests: AtomicU64,
    verifier: Arc<dyn PasswordVerifier>,
}

trait PasswordVerifier: Send + Sync {
    fn verify(&self, encoded: &str, password: &str) -> bool;
}

struct ArgonVerifier;

impl PasswordVerifier for ArgonVerifier {
    fn verify(&self, encoded: &str, password: &str) -> bool {
        password::verify_password(encoded, password)
    }
}

enum LoginOutcome {
    Success { username: String, token: String },
    Denied,
}

/// Builds the `/admin` router. The static assets are ungated (they leak nothing). The
/// security-headers layer wraps THIS router only.
fn router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/admin/theme.css", get(theme_css))
        .route("/admin/admin.js", get(admin_js))
        .route("/admin/htmx.min.js", get(htmx_js))
        .route("/admin/login", get(login_page).post(login_submit))
        .route("/admin/logout", post(logout))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// Hardening headers on every admin response. CSP keeps the shell functional (the
/// embedded theme uses inline `style=` attributes and the Google-Fonts stylesheet)
/// while forbidding scripts/frames from anywhere: `default-src 'self'` +
/// `frame-ancestors 'none'` per the plan, widened ONLY for styles/fonts.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src https://fonts.gstatic.com; frame-ancestors 'none'",
        ),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

// ---------------------------------------------------------------------------
// Auth: session gate, login/logout, lockout
// ---------------------------------------------------------------------------

/// The verified request identity: the session's user (or the "Local Admin" placeholder
/// when `ADMIN_OPEN=1`), the session's CSRF token (empty when open), and the raw session
/// token (logout deletes it).
struct Authed {
    username: String,
    csrf: String,
    token: String,
    user: UserView,
}

impl Authed {
    fn open() -> Authed {
        Authed {
            username: "local-admin".into(),
            csrf: String::new(),
            token: String::new(),
            user: UserView::new(""),
        }
    }
}

impl AdminState {
    /// The session gate. `ADMIN_OPEN=1` bypasses entirely; otherwise the
    /// `admin_session` cookie must match a live `admin.sessions` row. A miss is a
    /// 303 → `/admin/login` for a page GET, a 401 for a POST.
    async fn gate(&self, jar: &CookieJar, is_post: bool) -> Result<Authed, Response> {
        if self.open {
            return Ok(Authed::open());
        }
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
            return Err(deny(is_post));
        };
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT username, csrf_token FROM admin.sessions WHERE token = $1 AND expires_at > now()",
        )
        .bind(&token)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(err = %e, "admin session lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "session check failed").into_response()
        })?;
        match row {
            Some((username, csrf)) => Ok(Authed {
                user: UserView::new(&username),
                username,
                csrf,
                token,
            }),
            None => Err(deny(is_post)),
        }
    }

    /// CSRF check for mutating posts: `_csrf` in the form body must equal the
    /// session's token (constant-time). Skipped entirely under `ADMIN_OPEN=1`.
    fn check_csrf(&self, authed: &Authed, body: &HashMap<String, String>) -> Option<Response> {
        if self.open {
            return None;
        }
        let sent = body.get("_csrf").map(String::as_str).unwrap_or("");
        if ct_eq(sent.as_bytes(), authed.csrf.as_bytes()) {
            None
        } else {
            Some((StatusCode::FORBIDDEN, "invalid csrf token").into_response())
        }
    }

    /// Resolves the trustworthy client IP: the connection peer, honoring
    /// `X-Forwarded-For`/`X-Real-IP` only when the peer is a trusted proxy
    /// (`TRUSTED_PROXY_CIDRS` — the same walk the app-level rate limiter uses).
    fn resolve_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        let xri = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
        httpmw::client_ip(peer.ip(), xff, xri, &self.trusted)
    }

    /// Appends one `admin.action` event in its own small tx (the match `emit_tx`
    /// shape) — for actions whose domain write already committed (form-submit) or
    /// that have none.
    async fn emit_action(&self, evt: &adminevents::AdminAction) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &adminevents::ACTION, evt)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn cleanup_login_attempts(&self) {
        let result = sqlx::query(
            "WITH stale AS (
               SELECT ctid FROM admin.login_attempts
               WHERE updated_at < now() - interval '24 hours'
                 AND (locked_until IS NULL OR locked_until <= now())
               ORDER BY updated_at LIMIT 256 FOR UPDATE SKIP LOCKED
             )
             DELETE FROM admin.login_attempts a USING stale WHERE a.ctid = stale.ctid",
        )
        .execute(&self.pool)
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "admin login-attempt cleanup failed");
        }
    }

    async fn authenticate_and_mint(
        &self,
        username: String,
        submitted: String,
        ip: IpAddr,
        valid_input: bool,
        argon: OwnedSemaphorePermit,
    ) -> anyhow::Result<LoginOutcome> {
        const LOCK_NAMESPACE: i64 = 4_702_968_888_123_215_687;
        let effective_username = if valid_input { username.clone() } else { "<invalid>".to_string() };
        let user_subject = format!("user:{effective_username}");
        let ip_subject = format!("ip:{ip}");
        let mut subjects = [user_subject.clone(), ip_subject.clone()];
        subjects.sort();

        let mut tx = self.pool.begin().await?;
        let result: anyhow::Result<LoginOutcome> = async {
        for subject in &subjects {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
                .bind(subject)
                .bind(LOCK_NAMESPACE)
                .execute(&mut *tx)
                .await?;
        }

        let locked: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM admin.login_attempts
             WHERE subject = ANY($1) AND locked_until > now())",
        )
        .bind(&subjects)
        .fetch_one(&mut *tx)
        .await?;
        let row: Option<(String,)> = if valid_input {
            sqlx::query_as("SELECT pass_hash FROM admin.users WHERE username = $1")
                .bind(&effective_username)
                .fetch_optional(&mut *tx)
                .await?
        } else {
            None
        };
        let known_user = row.is_some();
        let (hash, candidate) = if locked.0 || !valid_input || !known_user {
            (DUMMY_HASH.clone(), "admin-invalid-credentials".to_string())
        } else {
            (row.expect("known row").0, submitted)
        };
        let verifier = self.verifier.clone();
        // The argon permit MUST live inside the blocking closure: spawn_blocking is
        // not cancelled when its JoinHandle drops, so a permit held in this async
        // frame would be released on client disconnect while the detached 64 MiB
        // hash keeps running — defeating the RAM cap.
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = argon;
            verifier.verify(&hash, &candidate)
        })
        .await
        .map_err(|error| anyhow::anyhow!("admin password verifier task failed: {error}"))?;
        let ok = verified && known_user && valid_input && !locked.0;

        if locked.0 {
            return Ok(LoginOutcome::Denied);
        }
        if !ok {
            let failures = if known_user && valid_input {
                vec![(&user_subject, USER_LOCK_THRESHOLD, true), (&ip_subject, IP_LOCK_THRESHOLD, false)]
            } else {
                vec![(&ip_subject, IP_LOCK_THRESHOLD, false)]
            };
            for (subject, threshold, is_user) in failures {
            let (fails,): (i32,) = sqlx::query_as(
                "INSERT INTO admin.login_attempts (subject, fails) VALUES ($1, 1)
                 ON CONFLICT (subject) DO UPDATE
                 SET fails = admin.login_attempts.fails + 1, updated_at = now()
                 RETURNING fails",
            )
            .bind(subject)
            .fetch_one(&mut *tx)
            .await?;
            if fails >= threshold {
                let backoff = backoff_secs(fails);
                sqlx::query(
                    "UPDATE admin.login_attempts
                     SET locked_until = now() + ($2::float8) * interval '1 second'
                     WHERE subject = $1",
                )
                .bind(subject)
                .bind(backoff as f64)
                .execute(&mut *tx)
                .await?;
                if is_user && fails == threshold {
                    let evt = adminevents::AdminAction {
                        actor: username.to_string(),
                        action: "login-locked".into(),
                        target: subject.clone(),
                        detail: format!("{fails} consecutive failures; locked for {backoff}s"),
                    };
                    self.bus
                        .emit_tx(AnyTx::new(&mut *tx), &adminevents::ACTION, &evt)
                        .await?;
                    }
                }
            }
            return Ok(LoginOutcome::Denied);
        }

        sqlx::query("DELETE FROM admin.login_attempts WHERE subject = ANY($1)")
            .bind(&subjects)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM admin.sessions WHERE expires_at <= now()")
            .execute(&mut *tx)
            .await?;
        let token = new_token();
        let csrf = new_token();
        sqlx::query(
            "INSERT INTO admin.sessions (token, username, csrf_token, expires_at)
             VALUES ($1, $2, $3, now() + ($4::float8) * interval '1 second')",
        )
        .bind(&token)
        .bind(&username)
        .bind(&csrf)
        .bind(SESSION_TTL_SECS as f64)
        .execute(&mut *tx)
        .await?;
        let evt = adminevents::AdminAction {
            actor: username.clone(),
            action: "login-succeeded".into(),
            target: user_subject,
            detail: format!("ip:{ip}"),
        };
        self.bus.emit_tx(AnyTx::new(&mut *tx), &adminevents::ACTION, &evt).await?;
        Ok(LoginOutcome::Success { username, token })
        }
        .await;
        match result {
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(error) => {
                if let Err(rollback_error) = tx.rollback().await {
                    tracing::error!(%rollback_error, "admin security transaction rollback failed");
                }
                Err(error)
            }
        }
    }
}

/// The session-miss response: page GETs bounce to the login form, POSTs get a bare
/// 401 (a browser form never posts without having loaded a page first).
fn deny(is_post: bool) -> Response {
    if is_post {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    } else {
        see_other("/admin/login")
    }
}

/// `least(2^fails, 900)` seconds, overflow-safe.
fn backoff_secs(fails: i32) -> i64 {
    if !(0..=9).contains(&fails) {
        return MAX_BACKOFF_SECS;
    }
    (1i64 << fails).min(MAX_BACKOFF_SECS)
}

/// A fresh opaque token: 32 random bytes, base64url without padding (43 chars).
fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Opportunistic GC of expired reveals (no background task). Best-effort: a prune
/// failure is logged, never fatal.
async fn prune_expired_reveals(pool: &PgPool) {
    if let Err(err) = sqlx::query(&format!(
        "DELETE FROM admin.reveals WHERE created_at <= now() - interval '{} seconds'",
        REVEAL_TTL.as_secs()
    ))
    .execute(pool)
    .await
    {
        tracing::warn!(%err, "admin reveal: expired-row prune failed");
    }
}

/// A PHC hash verified against for UNKNOWN usernames, so an unknown user costs the
/// same argon2 work as a wrong password (no timing oracle). Never matches: the
/// submitted password is compared against the hash of a fixed internal string.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| password::hash_password("admin-timing-equalizer").expect("static argon2 hash"));

/// Test-only: exposes this module's argon2 cost parameters so `cmd/server`'s
/// cross-module parity test can assert accounts' and admin's security-cost twins
/// never drift silently.
pub fn argon2_params_for_parity_test() -> (u32, u32, u32, usize) {
    password::argon2_params()
}

/// The `Set-Cookie` value minting a session (exact flags, no cookie-builder dep):
/// HttpOnly + SameSite=Strict + Path=/admin + Max-Age=12h, `Secure` per the knob.
fn session_set_cookie(token: &str, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/admin; Max-Age={SESSION_TTL_SECS}{secure}"
    ))
    .expect("cookie value is ASCII")
}

/// The clearing twin (logout): Max-Age=0 drops the cookie.
fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static("admin_session=; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=0")
}

fn see_other(loc: &str) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, HeaderValue::from_str(loc).unwrap())],
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers: login / logout
// ---------------------------------------------------------------------------

/// The login form view model.
#[derive(Serialize)]
struct LoginView {
    error: String,
}

/// Renders the login page. Every FAILED login funnels here with the SAME
/// `GENERIC_LOGIN_ERROR` + 401 — wrong password, unknown user, and locked produce
/// byte-identical bodies. (The locked path does skip the 1-2 attempt-row writes,
/// a marginal sub-millisecond, non-body timing asymmetry we accept.)
fn render_login(st: &AdminState, status: StatusCode, error: &str) -> Response {
    let view = LoginView { error: error.into() };
    match st.env.get_template("login.html").and_then(|t| t.render(&view)) {
        Ok(html) => (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            .into_response(),
        Err(e) => {
            tracing::error!(err = %e, "admin login render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
        }
    }
}

/// `GET /admin/login` — the form; an already-authenticated (or open) visitor is
/// bounced straight to the portal.
async fn login_page(State(st): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    match st.gate(&jar, false).await {
        Ok(_) => see_other("/admin"),
        Err(_) => render_login(&st, StatusCode::OK, ""),
    }
}

/// `POST /admin/login` — the whole flow: trusted-proxy client IP → lockout check
/// (user 5 / IP 20) → argon2 verify (dummy hash for unknown users) → on failure
/// increment + maybe lock (+ durable `login-locked`), generic 401; on success reset
/// the attempt rows, GC expired sessions, mint the session + emit `login-succeeded`
/// in ONE tx, set the cookie, 303 → `/admin`.
async fn login_submit(
    State(st): State<Arc<AdminState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(body): Form<HashMap<String, String>>,
) -> Response {
    if st.open {
        return see_other("/admin"); // no sessions to mint on an open portal
    }
    let raw_username = body.get("username").map(String::as_str).unwrap_or("");
    let submitted = body.get("password").cloned().unwrap_or_default();
    let ip = st.resolve_ip(peer, &headers);
    if !st.login_limiter.allow(ip) {
        return too_many_logins();
    }
    let gc_request = st
        .login_attempt_gc_requests
        .fetch_add(1, Ordering::Relaxed);
    let Ok(_slot) = st.login_slots.clone().try_acquire_owned() else {
        return too_many_logins();
    };
    if gc_request % 256 == 255 {
        st.cleanup_login_attempts().await;
    }
    let Ok(argon) = st.argon_permits.clone().acquire_owned().await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "login failed").into_response();
    };
    // Route trim + cap through the SAME authority the CLI uses (`normalize_username`)
    // so login and adminctl agree bit-for-bit; a rejection here maps to
    // `valid_input=false` and `authenticate_and_mint` still burns the dummy-hash
    // argon2 verify below (no username-validity timing oracle) exactly as before.
    let (username, username_valid) = match normalize_username(raw_username) {
        Ok(name) => (name, true),
        Err(_) => (String::new(), false),
    };
    let valid_input = username_valid && password_within_cap(&submitted);
    match st.authenticate_and_mint(username, submitted, ip, valid_input, argon).await {
        Ok(LoginOutcome::Success { username, token }) => {
            tracing::debug!(%username, "admin login succeeded");
            let mut resp = see_other("/admin");
            resp.headers_mut().insert(
                header::SET_COOKIE,
                session_set_cookie(&token, st.cookie_secure),
            );
            resp
        }
        Ok(LoginOutcome::Denied) => {
            render_login(&st, StatusCode::UNAUTHORIZED, GENERIC_LOGIN_ERROR)
        }
        Err(error) => {
            tracing::error!(%error, "admin login transaction failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "login failed").into_response()
        }
    }
}

fn too_many_logins() -> Response {
    let mut response = (StatusCode::TOO_MANY_REQUESTS, "too many login attempts").into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// `POST /admin/logout` — session + CSRF gated; deletes the session row and appends
/// the durable `logout` in ONE tx, clears the cookie, 303 → `/admin/login`.
async fn logout(
    State(st): State<Arc<AdminState>>,
    jar: CookieJar,
    Form(body): Form<HashMap<String, String>>,
) -> Response {
    let authed = match st.gate(&jar, true).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    if st.open {
        return see_other("/admin"); // no session to end
    }
    if let Some(resp) = st.check_csrf(&authed, &body) {
        return resp;
    }

    let ended: anyhow::Result<()> = async {
        let mut tx = st.pool.begin().await?;
        sqlx::query("DELETE FROM admin.sessions WHERE token = $1")
            .bind(&authed.token)
            .execute(&mut *tx)
            .await?;
        let evt = adminevents::AdminAction {
            actor: authed.username.clone(),
            action: "logout".into(),
            target: format!("user:{}", authed.username),
            detail: String::new(),
        };
        st.bus.emit_tx(AnyTx::new(&mut *tx), &adminevents::ACTION, &evt).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = ended {
        tracing::error!(err = %e, "admin logout failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "logout failed").into_response();
    }

    let mut resp = see_other("/admin/login");
    resp.headers_mut().insert(header::SET_COOKIE, clear_session_cookie());
    resp
}

// ---------------------------------------------------------------------------
// Handlers: static assets
// ---------------------------------------------------------------------------

/// `GET /admin/theme.css` — the embedded stylesheet, ungated.
async fn theme_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        THEME_CSS,
    )
        .into_response()
}

/// `GET /admin/admin.js` — the embedded portal script, ungated like the stylesheet
/// (a static asset leaks nothing; the CSP `default-src 'self'` requires it be a file).
async fn admin_js() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        ADMIN_JS,
    )
        .into_response()
}

/// `GET /admin/htmx.min.js` — the vendored htmx runtime, ungated.
async fn htmx_js() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        HTMX_JS,
    )
        .into_response()
}

/// Length-checked constant-time byte compare (Go's `subtle.ConstantTimeCompare`):
/// differing lengths are unequal, equal lengths compared without an early exit.
/// Used for the CSRF token check.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `true` only when `ADMIN_OPEN` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` — an unauthenticated admin portal is a
/// trust decision, so this follows the explicit-only convention (apikeys'
/// `dev_seed_explicitly_on`, gateway's `dev_auth_explicitly_on`), NOT a default-open.
fn admin_open_explicitly_on() -> bool {
    matches!(
        std::env::var("ADMIN_OPEN"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

/// The cookie `Secure` flag: ON unless `ADMIN_COOKIE_SECURE` is EXPLICITLY set
/// falsy (`0`/`false`/`off`, case-insensitive) — a fail-closed dev knob (the proof
/// scripts run over plain http, whose clients refuse Secure cookies).
fn cookie_secure_on() -> bool {
    !matches!(
        std::env::var("ADMIN_COOKIE_SECURE"),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    )
}

// ---------------------------------------------------------------------------
// Template view models (serde → minijinja)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct UserView {
    name: String,
    initials: String,
}

impl UserView {
    /// The footer/avatar identity: the session user's name + up-to-2-char initials,
    /// else the "Local Admin"/"LA" default under `ADMIN_OPEN=1`.
    fn new(name: &str) -> UserView {
        if name.is_empty() {
            return UserView {
                name: "Local Admin".into(),
                initials: "LA".into(),
            };
        }
        let mut ini = name.to_uppercase();
        if ini.chars().count() > 2 {
            ini = ini.chars().take(2).collect();
        }
        UserView {
            name: name.to_string(),
            initials: ini,
        }
    }
}

// ============================================================================
// Tests. Pure helpers run with no DB; the session/lockout/CSRF/durable-emit matrix
// targets the local Postgres (the test DB) through `testdb`, which FAILS the run when it
// is unreachable.
// ============================================================================
#[cfg(test)]
mod tests;

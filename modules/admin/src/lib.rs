//! `admin` — the GameOps admin PORTAL module. It owns the LOOK (the embedded dark
//! theme + the sidebar/header shell) and composes a navigable model from the items
//! modules CONTRIBUTE to [`adminapi::SLOT`]: items are grouped by
//! [`adminapi::Item::section`] into the sidebar, and each opens its own page
//! (`GET /admin/{slug}`). A module appears here without the admin being edited — it
//! reads CONTRIBUTIONS, never a module's implementation or another schema.
//!
//! Two item kinds, resolved by [`resolve_items`]:
//!   - **LOCAL** (`render` set) — the module's in-process closure, called lazily at
//!     page render, carrying the request's query params so a `Render` can switch on a
//!     drill-down key (`?owner=…`).
//!   - **REMOTE** (`remote_fetch` set) — fetched now over the QUIC edge (in a split
//!     process each provider stub contributes one). Its Section/Label/Content come
//!     from the peer's [`adminapi::ItemData`]; [`adminapi::ItemError::Absent`] drops
//!     the item silently, any other failure keeps it as an error card (a down peer
//!     never blanks `/admin`).
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
//! Mutating posts (`POST /admin/{slug}`, `POST /admin/logout`) require a `_csrf`
//! field matching the session's CSRF token; the check runs BEFORE the local/remote
//! editability decision. The template injects the hidden `_csrf` input from the
//! verified session — contract crates untouched.
//!
//! Durable audit trail: `admin.action` (`adminevents::ACTION`) is emitted via
//! `emit_tx` for `login-succeeded` / `login-locked` (user-row threshold) / `logout`
//! (each atomic with its own domain write) and `form-submit` after a LOCAL form
//! submit succeeds (best-effort: the owner module's mutation is an opaque closure,
//! so an emit failure surfaces as an error card, never a rollback).
//!
//! `ADMIN_OPEN=1` (explicit-only dev knob, loud warn) disables sessions AND CSRF —
//! a deliberately open local portal. Zero admin users is a WARNED boot, not a
//! failure: run `./install.sh` (adminctl) to mint one.
//!
//! Routes (mounted via `ctx.mount`, security headers on this router only):
//! `GET /admin/theme.css` (ungated), `GET|POST /admin/login`, `POST /admin/logout`,
//! `GET /admin`, `GET /admin/{slug}`, `POST /admin/{slug}` (LOCAL form submit only;
//! 405 for remote/non-form).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use bus::{AnyTx, Bus};
use contrib::Slots;
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

/// The embedded dark GameOps theme. Served ungated at `/admin/theme.css`.
const THEME_CSS: &str = include_str!("theme.css");

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

/// Hidden optimistic-concurrency evidence is intentionally self-describing so a
/// POST can retain tokens for rows that disappeared since the corresponding GET.
/// The values are authenticated by the admin session + CSRF boundary, but are not
/// secrets and remain subject to the owning module's authoritative comparison.
const EXPECTED_FIELD_PREFIX: &str = "_expected_";

/// Stable operator-facing response for an optimistic-concurrency miss.
const STALE_FORM_ERROR: &str = "This form is stale. Reload the page and try again.";

/// Show-once reveal flash: TTL + entry cap. A reveal is minted by a NON-idempotent,
/// non-CAS create (e.g. an API-key add), so the success page must NOT be a POST render
/// a browser refresh would re-submit. Instead the value is stashed here and the POST
/// 303s to a GET carrying a one-shot token — a refresh re-issues the (idempotent) GET,
/// finds the token consumed, and renders no reveal. Bounded + short-lived: this is a
/// single-operator dev portal, so a few minutes and a small cap suffice.
const REVEAL_TTL: Duration = Duration::from_secs(300);
const REVEAL_MAX_ENTRIES: usize = 256;

/// The in-memory one-shot reveal store backing PRG-with-flash. Owned by [`AdminState`]
/// — the admin module handles the POST in BOTH topologies (monolith + admin-svc), so no
/// cross-process store is needed. Every access sweeps expired entries; an insert past
/// [`REVEAL_MAX_ENTRIES`] evicts the soonest-expiring entry to stay bounded.
#[derive(Default)]
struct RevealStore {
    entries: HashMap<String, RevealEntry>,
}

struct RevealEntry {
    reveal: Vec<adminapi::RevealItem>,
    expires_at: Instant,
}

impl RevealStore {
    /// Stashes `reveal` under `token` with a fresh TTL, after sweeping expired entries
    /// and (if still at the cap) evicting the soonest-to-expire.
    fn insert(&mut self, token: String, reveal: Vec<adminapi::RevealItem>) {
        let now = Instant::now();
        self.entries.retain(|_, e| e.expires_at > now);
        if self.entries.len() >= REVEAL_MAX_ENTRIES {
            if let Some(evict) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&evict);
            }
        }
        self.entries.insert(
            token,
            RevealEntry {
                reveal,
                expires_at: now + REVEAL_TTL,
            },
        );
    }

    /// CONSUMES the reveal for `token` (one-shot): removes and returns it iff present
    /// and unexpired. A second call for the same token — or a refresh after consumption
    /// — returns `None`.
    fn take(&mut self, token: &str) -> Option<Vec<adminapi::RevealItem>> {
        let now = Instant::now();
        self.entries.retain(|_, e| e.expires_at > now);
        self.entries.remove(token).map(|e| e.reveal)
    }
}

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
const AUTH_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS admin.sessions (
	token      text PRIMARY KEY,
	username   text NOT NULL REFERENCES admin.users(username) ON DELETE CASCADE,
	csrf_token text NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS admin_sessions_expires_idx ON admin.sessions(expires_at);
CREATE TABLE IF NOT EXISTS admin.login_attempts (
	subject      text PRIMARY KEY,   -- 'user:<name>' | 'ip:<addr>'
	fails        int  NOT NULL DEFAULT 0,
	locked_until timestamptz,
	updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS admin_login_attempts_updated_idx
ON admin.login_attempts(updated_at);"#;

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

        let login_limiter = httpmw::IpLimiter::new(5.0, 20);
        self.login_limiter
            .set(login_limiter.clone())
            .map_err(|_| anyhow::anyhow!("admin.init ran twice"))?;

        let state = Arc::new(AdminState {
            env,
            slots: ctx.slots().clone(),
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
            reveals: Arc::new(Mutex::new(RevealStore::default())),
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
    Ok(env)
}

/// Per-request admin state captured by the router closures. `slots` is read on each
/// request so newly-contributed items appear without a restart; the pool backs the
/// per-request session check + login flow; the bus appends the durable
/// `admin.action` trail.
struct AdminState {
    env: minijinja::Environment<'static>,
    slots: Arc<Slots>,
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
    /// One-shot show-once reveal flash (PRG-with-flash): a successful create stashes its
    /// reveal here and 303s to a token GET, so a POST-refresh can never re-mint.
    reveals: Arc<Mutex<RevealStore>>,
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

/// Builds the `/admin` router. `theme.css` is ungated (a stylesheet leaks nothing);
/// static routes (`/admin/login`, `/admin/logout`, `/admin/theme.css`) are
/// registered alongside the `/admin/:slug` param route — matchit prefers static at
/// the same position. The security-headers layer wraps THIS router only.
fn router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/admin/theme.css", get(theme_css))
        .route("/admin/login", get(login_page).post(login_submit))
        .route("/admin/logout", post(logout))
        .route("/admin", get(index))
        .route("/admin/:slug", get(item).post(item_post))
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

/// The verified request identity a handler renders under: the session's user (or
/// the "Local Admin" placeholder when `ADMIN_OPEN=1`), the CSRF token the template
/// injects (empty when open — the hidden input is omitted), and the raw session
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
    /// Runs BEFORE any item resolution / editability decision.
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

    /// Stashes a show-once `reveal` and returns its one-shot token (for the PRG
    /// redirect target `?reveal=<token>`).
    fn stash_reveal(&self, reveal: Vec<adminapi::RevealItem>) -> String {
        let token = new_token();
        self.reveals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(token.clone(), reveal);
        token
    }

    /// CONSUMES the reveal for `token` (removes it): `Some` exactly once, `None` on a
    /// refresh/replay — the property that makes the flash safe against a POST re-submit.
    fn take_reveal(&self, token: &str) -> Option<Vec<adminapi::RevealItem>> {
        self.reveals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(token)
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
// Handlers: portal pages
// ---------------------------------------------------------------------------

/// `GET /admin/theme.css` — the embedded stylesheet, ungated.
async fn theme_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        THEME_CSS,
    )
        .into_response()
}

/// `GET /admin` — redirect to the first resolved item's page, or render an empty
/// shell when nothing is contributed. 302 (Go's `StatusFound`).
async fn index(
    State(st): State<Arc<AdminState>>,
    jar: CookieJar,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let authed = match st.gate(&jar, false).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let items = resolve_items(&st, &params).await;
    if items.is_empty() {
        return render_page(
            &st,
            PageData {
                crumb: "Admin".into(),
                title: "Admin".into(),
                env: "Local".into(),
                user: authed.user.clone(),
                csrf: authed.csrf.clone(),
                groups: Vec::new(),
                page: None,
            },
        );
    }
    let loc = format!("/admin/{}", items[0].slug);
    (
        StatusCode::FOUND,
        [(header::LOCATION, HeaderValue::from_str(&loc).unwrap())],
    )
        .into_response()
}

/// `GET /admin/{slug}` — render one item's page. A LOCAL item's `render` is called
/// here (lazily, with the query params); a REMOTE item's content was already fetched
/// in [`resolve_items`].
async fn item(
    State(st): State<Arc<AdminState>>,
    Path(slug): Path<String>,
    jar: CookieJar,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let authed = match st.gate(&jar, false).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let items = resolve_items(&st, &params).await;
    let Some(cur) = items.iter().find(|r| r.slug == slug) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let mut page = page_view(cur, &params, &slug);
    // PRG-with-flash: a `?reveal=<token>` from a just-completed submit is CONSUMED here
    // (one-shot) and rendered once. A refresh of this GET re-issues without a live token
    // (already consumed) and shows no reveal — and, being a GET, mints nothing.
    if let Some(token) = params.get("reveal") {
        if let Some(reveal) = st.take_reveal(token) {
            page.reveal = reveal;
        }
    }
    let groups = build_groups(&items, &slug);
    render_page(
        &st,
        PageData {
            crumb: cur.section.clone(),
            title: cur.label.clone(),
            env: "Local".into(),
            user: authed.user.clone(),
            csrf: authed.csrf.clone(),
            groups,
            page: Some(page),
        },
    )
}

/// `POST /admin/{slug}` — apply an item's editable form, LOCAL or REMOTE. Order matters
/// and is a contract the split-proof asserts: session gate → CSRF (403, BEFORE the
/// local/remote editability decision — a remote item with a bad token is 403, not 405,
/// and its `remote_submit` is never dialed) → resolve → editability →
///   • LOCAL: in-process `submit` closure → conflict (409, no audit) OR durable
///     `form-submit` (best-effort: the mutation already committed inside the opaque
///     closure, so an emit failure is an error card, not a rollback);
///   • REMOTE: dispatch over the edge via the per-provider `remote_submit` → `NotFound`
///     (peer has no write surface) is 405 read-only, `Conflict` is 409; the provider's
///     OWN co-hosted process emits its `admin.action`, so the admin never fabricates one
///     for a write it merely forwarded.
/// On success a [`adminapi::SubmitOutcome`] carrying show-once `reveal` values renders
/// INLINE (200) so they are seen exactly once; an empty outcome 303s back to the GET.
async fn item_post(
    State(st): State<Arc<AdminState>>,
    Path(slug): Path<String>,
    jar: CookieJar,
    Query(params): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    // Parse the urlencoded body preserving DUPLICATE keys: a CheckboxGroup field posts
    // its shared name once per checked option, and `Form<HashMap>` would collapse those
    // to one. `pairs` is the ordered multimap (checkbox collection); `single` is the
    // last-wins map for the CSRF token, hidden evidence, and Text/Select fields.
    let pairs: Vec<(String, String)> = form_urlencoded::parse(&body)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut single: HashMap<String, String> = HashMap::new();
    for (k, v) in &pairs {
        single.insert(k.clone(), v.clone());
    }

    let authed = match st.gate(&jar, true).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    // CSRF BEFORE the local/remote editability decision (ordering contract, split-proof
    // AD4). A remote item with a bad token is 403, never 405, and never reaches the edge.
    if let Some(resp) = st.check_csrf(&authed, &single) {
        return resp;
    }
    let items = resolve_items(&st, &params).await;
    let Some(cur) = items.iter().find(|r| r.slug == slug) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    // LOCAL item: an in-process render + submit closure, applied here.
    if let Some(render) = cur.item.render.clone() {
        let content = match render(&params) {
            Ok(c) => c,
            Err(e) => {
                return render_error(&st, cur, &slug, &items, &authed, format!("failed to load: {e}"))
            }
        };
        let Some(form) = content.form else {
            return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
        };
        let Some(submit) = form.submit.clone() else {
            return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
        };
        let values = collect_submit_params(&form.fields, &form.hidden, &pairs, &single);
        return match submit(values).await {
            Ok(outcome) => {
                // Durable trail AFTER the mutation committed (LOCAL, co-hosted). Field
                // NAMES only — never submitted values (they may hold secrets).
                let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
                let evt = adminevents::AdminAction {
                    actor: authed.username.clone(),
                    action: "form-submit".into(),
                    target: slug.clone(),
                    detail: names.join(","),
                };
                if let Err(e) = st.emit_action(&evt).await {
                    tracing::error!(err = %e, slug, "admin.action form-submit append failed");
                    return render_error(
                        &st,
                        cur,
                        &slug,
                        &items,
                        &authed,
                        "action applied but audit append failed".to_string(),
                    );
                }
                render_after_submit(&st, &slug, outcome)
            }
            Err(adminapi::SubmitError::Conflict) => {
                render_conflict(&st, cur, &slug, &items, &authed)
            }
            Err(adminapi::SubmitError::Other(e)) => {
                render_error(&st, cur, &slug, &items, &authed, format!("save failed: {e}"))
            }
        };
    }

    // REMOTE item: no in-process render. Dispatch the edit over the edge iff the peer
    // exposed a write surface (`remote_submit`) AND returned an editable form; otherwise
    // it is read-only (405).
    let Some(remote_submit) = cur.item.remote_submit.clone() else {
        return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
    };
    let form = match &cur.remote {
        Some(RemoteResult::Ok(content)) => content.form.clone(),
        _ => None,
    };
    let Some(form) = form else {
        return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
    };
    let values = collect_submit_params(&form.fields, &form.hidden, &pairs, &single);
    match remote_submit(values).await {
        Ok(outcome) => render_after_submit(&st, &slug, outcome),
        // The peer never registered `admin.adminSubmit` (UnknownMethod → NotFound): the
        // item has no write surface, so it degrades to read-only exactly like a local
        // non-editable item — the graceful-absent contract, no bespoke signalling.
        Err(e) if e.status == opsapi::Status::NotFound => {
            (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response()
        }
        Err(e) if e.status == opsapi::Status::Conflict => {
            render_conflict(&st, cur, &slug, &items, &authed)
        }
        Err(e) => render_error(&st, cur, &slug, &items, &authed, format!("save failed: {e}")),
    }
}

/// Builds the allowlisted submit [`adminapi::Params`] from the posted body, given the
/// (freshly rendered or freshly fetched) form's declared `fields`/`hidden`. Visible
/// fields are allowlisted by name: a [`adminapi::FieldKind::CheckboxGroup`] comma-joins
/// every value posted under its shared name (checked boxes only — the ordered `pairs`
/// preserve the duplicates a HashMap would collapse), and every other kind takes the
/// single last-wins value. Hidden fields use the POSTED value (the browser echoes the
/// evidence it received on GET, never the fresh render's), and every reserved
/// `_expected_*` entry is retained even when its row vanished before the re-render —
/// otherwise deleting a row would also delete the token needed to detect it. `_csrf`
/// matches neither set and never reaches the owning module.
fn collect_submit_params(
    fields: &[adminapi::Field],
    hidden: &[adminapi::HiddenField],
    pairs: &[(String, String)],
    single: &HashMap<String, String>,
) -> adminapi::Params {
    let mut values = adminapi::Params::new();
    for f in fields {
        if f.kind == adminapi::FieldKind::CheckboxGroup {
            let joined = pairs
                .iter()
                .filter(|(k, _)| *k == f.name)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join(",");
            values.insert(f.name.clone(), joined);
        } else {
            values.insert(f.name.clone(), single.get(&f.name).cloned().unwrap_or_default());
        }
    }
    for h in hidden {
        values.insert(h.name.clone(), single.get(&h.name).cloned().unwrap_or_default());
    }
    for (name, value) in single {
        if name.starts_with(EXPECTED_FIELD_PREFIX) {
            values.insert(name.clone(), value.clone());
        }
    }
    values
}

/// After a successful submit (LOCAL or REMOTE), ALWAYS Post/Redirect/Get — never an
/// inline POST render. An empty outcome 303s straight back to the item's GET. A
/// [`adminapi::SubmitOutcome`] carrying show-once `reveal` values is stashed in the
/// one-shot flash store and the 303 targets `GET /admin/{slug}?reveal=<token>`: the GET
/// consumes the token and renders the reveal exactly once. This closes the POST-refresh
/// double-mint — a reveal is minted by a NON-idempotent create, so an inline 200 would
/// let a browser refresh re-submit the identical body and mint a second one; a GET
/// refresh (token already consumed) mints nothing.
fn render_after_submit(st: &AdminState, slug: &str, outcome: adminapi::SubmitOutcome) -> Response {
    if outcome.reveal.is_empty() {
        return see_other(&format!("/admin/{slug}"));
    }
    let token = st.stash_reveal(outcome.reveal);
    see_other(&format!("/admin/{slug}?reveal={token}"))
}

/// Renders the stable stale-form card with HTTP 409. A template failure remains the
/// underlying 500 instead of being masked as a successful conflict render.
fn render_conflict(
    st: &AdminState,
    cur: &Resolved,
    slug: &str,
    items: &[Resolved],
    authed: &Authed,
) -> Response {
    let mut response = render_error(st, cur, slug, items, authed, STALE_FORM_ERROR.to_string());
    if response.status().is_success() {
        *response.status_mut() = StatusCode::CONFLICT;
    }
    response
}

/// Re-renders the current page with an error card (the POST failure path).
fn render_error(
    st: &AdminState,
    cur: &Resolved,
    slug: &str,
    items: &[Resolved],
    authed: &Authed,
    msg: String,
) -> Response {
    let groups = build_groups(items, slug);
    render_page(
        st,
        PageData {
            crumb: cur.section.clone(),
            title: cur.label.clone(),
            env: "Local".into(),
            user: authed.user.clone(),
            csrf: authed.csrf.clone(),
            groups,
            page: Some(PageView {
                title: cur.label.clone(),
                err: msg,
                kpis: Vec::new(),
                table: None,
                form: None,
                reveal: Vec::new(),
            }),
        },
    )
}

// ---------------------------------------------------------------------------
// Item resolution (the fan-out) + pure view helpers
// ---------------------------------------------------------------------------

/// A remote item's fetched outcome: the content, or the transport error string that
/// becomes an "unavailable" error card.
enum RemoteResult {
    Ok(adminapi::Content),
    Err(String),
}

/// One resolved sidebar entry ready to render (Go's `resolvedItem`). `item` carries
/// the original contribution (its `render`/`submit` closures for the LOCAL path);
/// `remote` is `Some` for a REMOTE item (already fetched).
struct Resolved {
    section: String,
    label: String,
    slug: String,
    item: adminapi::Item,
    remote: Option<RemoteResult>,
}

/// Resolves the contributed admin items into ordered [`Resolved`] entries with unique
/// slugs (first-seen order; collisions get `-2`, `-3`, …; empty→`item`). A LOCAL item
/// keeps its `render` closure; a REMOTE item is fetched now over the edge — an
/// [`adminapi::ItemError::Absent`] drops it silently, any other error keeps it as an
/// error card (Label falls back to ID). Fetching per request is fine: `/admin` is
/// low-traffic.
async fn resolve_items(st: &AdminState, params: &adminapi::Params) -> Vec<Resolved> {
    let items: Vec<adminapi::Item> = st.slots.contributions(adminapi::SLOT);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Resolved> = Vec::new();

    for it in items {
        let (section, label, remote) = if let Some(fetch) = it.remote_fetch.clone() {
            match fetch(params.clone()).await {
                Err(adminapi::ItemError::Absent) => continue, // no admin surface → skip
                Err(e) => (it.id.clone(), it.id.clone(), Some(RemoteResult::Err(format!("{e}")))),
                Ok(data) => (data.section, data.label, Some(RemoteResult::Ok(data.content))),
            }
        } else {
            (it.section.clone(), it.label.clone(), None)
        };

        let mut base = slugify(&label);
        if base.is_empty() {
            base = "item".into();
        }
        let mut slug = base.clone();
        let mut n = 2;
        while seen.contains(&slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        seen.insert(slug.clone());

        out.push(Resolved {
            section,
            label,
            slug,
            item: it,
            remote,
        });
    }
    out
}

/// Builds the [`PageView`] for one resolved item: the remote content (or its fetch
/// error), else the LOCAL render closure called with the request's query params.
fn page_view(cur: &Resolved, params: &adminapi::Params, slug: &str) -> PageView {
    match &cur.remote {
        Some(RemoteResult::Err(msg)) => PageView {
            title: cur.label.clone(),
            err: format!("unavailable: {msg}"),
            kpis: Vec::new(),
            table: None,
            form: None,
            reveal: Vec::new(),
        },
        // A remote item's form arrives with `submit == None` (a closure can't marshal),
        // but the peer may still expose a WRITE surface (`admin.adminSubmit`): render its
        // typed fields with the POST action `/admin/{slug}`, exactly like a local form.
        // The POST is dispatched over the edge via the item's `remote_submit`; a peer that
        // never registered the wire method answers 405 (read-only). A remote content with
        // `form: None` still renders read-only (KPIs + table only), as before.
        Some(RemoteResult::Ok(content)) => {
            let form = content.form.clone().map(|mut f| {
                f.action = format!("/admin/{slug}");
                f
            });
            PageView {
                title: cur.label.clone(),
                err: String::new(),
                kpis: content.kpis.clone(),
                table: content.table.clone(),
                form,
                reveal: Vec::new(),
            }
        }
        None => match &cur.item.render {
            Some(render) => match render(params) {
                Ok(content) => {
                    let form = content.form.map(|mut f| {
                        f.action = format!("/admin/{slug}");
                        f
                    });
                    PageView {
                        title: cur.label.clone(),
                        err: String::new(),
                        kpis: content.kpis,
                        table: content.table,
                        form,
                        reveal: Vec::new(),
                    }
                }
                Err(e) => PageView {
                    title: cur.label.clone(),
                    err: format!("failed to load: {e}"),
                    kpis: Vec::new(),
                    table: None,
                    form: None,
                    reveal: Vec::new(),
                },
            },
            // Neither a closure nor a remote result (a metadata-only local item).
            None => PageView {
                title: cur.label.clone(),
                err: String::new(),
                kpis: Vec::new(),
                table: None,
                form: None,
                reveal: Vec::new(),
            },
        },
    }
}

/// Groups items by section preserving first-seen section order, marking the item
/// whose slug matches `active` (Go's `buildGroups`).
fn build_groups(items: &[Resolved], active: &str) -> Vec<NavGroup> {
    let mut groups: Vec<NavGroup> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    for it in items {
        let i = match idx.get(&it.section) {
            Some(&i) => i,
            None => {
                let i = groups.len();
                idx.insert(it.section.clone(), i);
                groups.push(NavGroup {
                    section: it.section.clone(),
                    items: Vec::new(),
                });
                i
            }
        };
        groups[i].items.push(NavItem {
            label: it.label.clone(),
            slug: it.slug.clone(),
            active: it.slug == active,
        });
    }
    groups
}

/// Lowercases `s`, keeps `[a-z0-9]`, maps space/`-`/`_`→`-`, drops other runes, and
/// trims leading/trailing `-` (Go's `slugify`, byte-for-byte on the ASCII cases).
fn slugify(s: &str) -> String {
    let mut b = String::new();
    for r in s.to_lowercase().chars() {
        if r.is_ascii_lowercase() || r.is_ascii_digit() {
            b.push(r);
        } else if r == ' ' || r == '-' || r == '_' {
            b.push('-');
        }
    }
    b.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Rendering + small shared helpers
// ---------------------------------------------------------------------------

/// Renders the template with `data` into an HTML response; a template error becomes a
/// 500 (should never happen — the template is compile-time embedded).
fn render_page(st: &AdminState, data: PageData) -> Response {
    match st.env.get_template("admin.html").and_then(|t| t.render(&data)) {
        Ok(html) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(err = %e, "admin render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
        }
    }
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

#[derive(Serialize)]
struct NavItem {
    label: String,
    slug: String,
    active: bool,
}

#[derive(Serialize)]
struct NavGroup {
    section: String,
    items: Vec<NavItem>,
}

#[derive(Serialize)]
struct PageView {
    title: String,
    err: String,
    kpis: Vec<adminapi::Kpi>,
    table: Option<adminapi::Table>,
    form: Option<adminapi::Form>,
    /// SHOW-ONCE values surfaced right after a successful submit (e.g. a freshly minted
    /// API-key secret). Rendered inline — never persisted, never re-derivable — which is
    /// why a submit that carries a reveal renders the page INLINE (200) instead of the
    /// usual 303 redirect (a redirect would drop these values). Empty on every plain page.
    #[serde(default)]
    reveal: Vec<adminapi::RevealItem>,
}

#[derive(Serialize)]
struct PageData {
    crumb: String,
    title: String,
    env: String,
    user: UserView,
    /// The verified session's CSRF token; the template injects it as the hidden
    /// `_csrf` input on the edit + logout forms. Empty (inputs omitted) under
    /// `ADMIN_OPEN=1` — the CSRF check is skipped there too.
    csrf: String,
    groups: Vec<NavGroup>,
    page: Option<PageView>,
}

// ============================================================================
// Tests. Pure helpers (slugify, build_groups, resolve_items, templates) run with
// no DB; the session/lockout/CSRF/durable-emit matrix targets the local Postgres
// (the test DB) and SKIPs cleanly when it is unreachable.
// ============================================================================
#[cfg(test)]
mod tests;

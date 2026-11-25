//! The hosted browser pages under `/accounts/oidc`: the email-and-password sign-in form,
//! the signed-in landing page and logout. A successful sign-in mints a browser session
//! (`session.rs`), never a player bearer.

pub(crate) mod session;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::{normalize_email, Service};

pub(crate) const PREFIX: &str = "/accounts/oidc";
pub(crate) const LANDING: &str = "/accounts/oidc/";
const LOGIN: &str = "/accounts/oidc/login";
pub(crate) const SESSION_COOKIE: &str = "swarog_session";
pub(crate) const LOGIN_CSRF_COOKIE: &str = "swarog_login_csrf";
const LOGIN_CSRF_MAX_AGE_SECS: u32 = 600;
pub(crate) const MAX_RETURN_TO_BYTES: usize = 2048;
const GENERIC_LOGIN_ERROR: &str = "Invalid email or password.";
const EXPIRED_FORM_ERROR: &str = "The sign-in form expired. Please try again.";
const BUSY_ERROR: &str = "Sign-in is busy. Please try again in a moment.";

const TOKENS_CSS: &str = include_str!("../../../../UILayout/design-system/tokens.css");
const BUNDLE_CSS: &str = include_str!("../../../../UILayout/design-system/components/bundle.css");
const BASE_TMPL: &str = include_str!("templates/base.html.tmpl");
const LOGIN_TMPL: &str = include_str!("templates/login.html.tmpl");
const LANDING_TMPL: &str = include_str!("templates/landing.html.tmpl");

/// The one `Secure` rule for every accounts cookie: ON unless `ACCOUNTS_COOKIE_SECURE` is
/// explicitly falsy (`0`/`false`/`off`, case-insensitive).
pub(crate) fn cookie_secure_from_env() -> bool {
    !matches!(
        std::env::var("ACCOUNTS_COOKIE_SECURE"),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    )
}

/// What the hosted pages share: the service, the templates and the per-IP login limiter.
pub(crate) struct Web {
    svc: Arc<Service>,
    env: minijinja::Environment<'static>,
    cookie_secure: bool,
    trusted: Vec<ipnet::IpNet>,
    limiter: Arc<httpmw::IpLimiter>,
}

impl Web {
    pub(crate) fn new(
        svc: Arc<Service>,
        cookie_secure: bool,
        trusted: Vec<ipnet::IpNet>,
        limiter: Arc<httpmw::IpLimiter>,
    ) -> anyhow::Result<Web> {
        let mut env = minijinja::Environment::new();
        for (name, source) in [
            ("base.html", BASE_TMPL),
            ("login.html", LOGIN_TMPL),
            ("landing.html", LANDING_TMPL),
        ] {
            env.add_template(name, source)?;
        }
        Ok(Web { svc, env, cookie_secure, trusted, limiter })
    }

    fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        let xri = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
        httpmw::client_ip(peer.ip(), xff, xri, &self.trusted)
    }

    fn render(&self, name: &str, status: StatusCode, view: impl Serialize) -> Response {
        match self.env.get_template(name).and_then(|t| t.render(view)) {
            Ok(html) => {
                (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
            }
            Err(e) => {
                tracing::error!(err = %e, template = name, "accounts page render failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
            }
        }
    }

    /// The sign-in form with a login-CSRF token: the browser's current one when it is
    /// well formed, so two open tabs share it, else a fresh one set on the response.
    fn render_login(&self, jar: &CookieJar, status: StatusCode, error: &str, return_to: &str) -> Response {
        let existing = jar.get(LOGIN_CSRF_COOKIE).map(|c| c.value()).filter(|v| well_formed_token(v));
        let (csrf, set) = match existing {
            Some(v) => (v.to_string(), false),
            None => (crate::store::new_token(), true),
        };
        let view = LoginView { error, csrf: &csrf, return_to };
        let mut resp = self.render("login.html", status, view);
        if set {
            resp.headers_mut().append(header::SET_COOKIE, self.login_csrf_cookie(&csrf));
        }
        resp
    }

    fn cookie(&self, name: &str, value: &str, same_site: &str, max_age: u64) -> HeaderValue {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{name}={value}; HttpOnly; SameSite={same_site}; Path={PREFIX}; Max-Age={max_age}{secure}"
        ))
        .expect("cookie value is ASCII")
    }

    fn login_csrf_cookie(&self, token: &str) -> HeaderValue {
        self.cookie(LOGIN_CSRF_COOKIE, token, "Strict", u64::from(LOGIN_CSRF_MAX_AGE_SECS))
    }

    fn session_cookie(&self, token: &str) -> HeaderValue {
        let max_age = u64::from(session::BROWSER_SESSION_TTL_HOURS.unsigned_abs()) * 3600;
        self.cookie(SESSION_COOKIE, token, "Lax", max_age)
    }

    /// The signed-in browser behind the request's session cookie, if any.
    async fn signed_in(&self, jar: &CookieJar) -> Result<Option<session::BrowserSession>, sqlx::Error> {
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
            return Ok(None);
        };
        self.svc.store.browser_session(&token).await
    }
}

#[derive(Serialize)]
struct LoginView<'a> {
    error: &'a str,
    csrf: &'a str,
    return_to: &'a str,
}

#[derive(Serialize)]
struct LandingView<'a> {
    handle: &'a str,
    csrf: &'a str,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReturnToQuery {
    return_to: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginForm {
    email: String,
    password: String,
    csrf: String,
    return_to: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LogoutForm {
    csrf: String,
}

/// The hosted pages as a sub-router the module `ctx.mount`s; the security headers wrap
/// these routes only.
pub(crate) fn router(web: Arc<Web>) -> Router {
    Router::new()
        .route(PREFIX, get(|| async { see_other(LANDING) }))
        .route(LANDING, get(landing))
        .route(LOGIN, get(login_page).post(login_submit))
        .route("/accounts/oidc/logout", post(logout))
        .route("/accounts/oidc/assets/tokens.css", get(|| css(TOKENS_CSS)))
        .route("/accounts/oidc/assets/bundle.css", get(|| css(BUNDLE_CSS)))
        .layer(middleware::from_fn(security_headers))
        .with_state(web)
}

/// No inline script, no framing, no caching; inline `style=` attributes as in `core/admin`.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn css(body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], body).into_response()
}

fn session_lookup_failed(e: sqlx::Error) -> Response {
    tracing::error!(err = %e, "browser session lookup failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed").into_response()
}

fn see_other(location: &str) -> Response {
    let location = HeaderValue::from_str(location).unwrap_or_else(|_| HeaderValue::from_static(LANDING));
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}

/// Where a sign-in may send the browser: a path under `/accounts/oidc/` with its query
/// kept, at most [`MAX_RETURN_TO_BYTES`], printable ASCII, with no `//`, no backslash and
/// no dot segment (a browser would resolve one out of the prefix). Anything else is the
/// landing page.
pub(crate) fn safe_return_to(raw: Option<&str>) -> &str {
    let Some(raw) = raw else { return LANDING };
    let path = raw.split_once('?').map_or(raw, |(path, _)| path);
    let ok = raw.len() <= MAX_RETURN_TO_BYTES
        && raw.starts_with(LANDING)
        && raw.bytes().all(|b| b.is_ascii_graphic())
        && !raw.contains("//")
        && !raw.contains('\\')
        && !path.split('/').any(is_dot_segment);
    if ok {
        raw
    } else {
        LANDING
    }
}

fn is_dot_segment(segment: &str) -> bool {
    let decoded = segment.to_ascii_lowercase().replace("%2e", ".");
    decoded == "." || decoded == ".."
}

fn well_formed_token(value: &str) -> bool {
    value.len() == 43
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

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

/// `GET /accounts/oidc/` — the signed-in landing page; a signed-out browser goes to the form.
async fn landing(State(web): State<Arc<Web>>, jar: CookieJar) -> Response {
    let session = match web.signed_in(&jar).await {
        Ok(Some(session)) => session,
        Ok(None) => return see_other(LOGIN),
        Err(e) => return session_lookup_failed(e),
    };
    match web.svc.store.player_summary(&session.player_id).await {
        Ok(Some(player)) => web.render(
            "landing.html",
            StatusCode::OK,
            LandingView { handle: &player.handle, csrf: &session.csrf_token },
        ),
        Ok(None) => see_other(LOGIN),
        Err(e) => {
            tracing::error!(err = %e, "signed-in player lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "player lookup failed").into_response()
        }
    }
}

/// `GET /accounts/oidc/login` — the form; an already signed-in browser goes straight on.
async fn login_page(
    State(web): State<Arc<Web>>,
    jar: CookieJar,
    Query(q): Query<ReturnToQuery>,
) -> Response {
    let return_to = safe_return_to(q.return_to.as_deref());
    match web.signed_in(&jar).await {
        Ok(Some(_)) => see_other(return_to),
        Ok(None) => web.render_login(&jar, StatusCode::OK, "", return_to),
        Err(e) => session_lookup_failed(e),
    }
}

/// `POST /accounts/oidc/login` — per-IP limiter, login-CSRF double submit, then the
/// service's `verify_password`; success mints a browser session and follows `return_to`.
async fn login_submit(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Response {
    let return_to = safe_return_to(Some(&form.return_to));
    if !web.limiter.allow(web.client_ip(peer, &headers)) {
        let mut resp = web.render_login(&jar, StatusCode::TOO_MANY_REQUESTS, BUSY_ERROR, return_to);
        resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return resp;
    }
    let cookie_csrf = jar.get(LOGIN_CSRF_COOKIE).map(|c| c.value()).unwrap_or("");
    if cookie_csrf.is_empty() || !ct_eq(cookie_csrf.as_bytes(), form.csrf.as_bytes()) {
        return web.render_login(&jar, StatusCode::FORBIDDEN, EXPIRED_FORM_ERROR, return_to);
    }
    let email = normalize_email(&form.email);
    let player = match web.svc.verify_password(&email, form.password).await {
        Ok(player) => player,
        Err(e) if e.status == opsapi::Status::Unauthorized => {
            return web.render_login(&jar, StatusCode::UNAUTHORIZED, GENERIC_LOGIN_ERROR, return_to);
        }
        Err(e) if e.status == opsapi::Status::Unavailable => {
            return web.render_login(&jar, StatusCode::SERVICE_UNAVAILABLE, BUSY_ERROR, return_to);
        }
        Err(e) => {
            tracing::error!(err = %e, "browser sign-in failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "sign-in failed").into_response();
        }
    };
    let minted = match web.svc.store.create_browser_session(&player.id).await {
        Ok(Some(minted)) => minted,
        Ok(None) => {
            return web.render_login(&jar, StatusCode::UNAUTHORIZED, GENERIC_LOGIN_ERROR, return_to);
        }
        Err(e) => {
            tracing::error!(err = %e, "browser session mint failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "sign-in failed").into_response();
        }
    };
    let mut resp = see_other(return_to);
    let h = resp.headers_mut();
    h.append(header::SET_COOKIE, web.session_cookie(&minted.token));
    h.append(header::SET_COOKIE, web.cookie(LOGIN_CSRF_COOKIE, "", "Strict", 0));
    resp
}

/// `POST /accounts/oidc/logout` — session and its CSRF token required; deletes the
/// session and clears the cookie.
async fn logout(
    State(web): State<Arc<Web>>,
    jar: CookieJar,
    Form(form): Form<LogoutForm>,
) -> Response {
    let session = match web.signed_in(&jar).await {
        Ok(Some(session)) => session,
        Ok(None) => return (StatusCode::FORBIDDEN, "not signed in").into_response(),
        Err(e) => return session_lookup_failed(e),
    };
    if !ct_eq(form.csrf.as_bytes(), session.csrf_token.as_bytes()) {
        return (StatusCode::FORBIDDEN, "invalid csrf token").into_response();
    }
    let token = jar.get(SESSION_COOKIE).map(|c| c.value()).unwrap_or("");
    if let Err(e) = web.svc.store.delete_browser_session(token).await {
        tracing::error!(err = %e, "browser session delete failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "logout failed").into_response();
    }
    let mut resp = see_other(LOGIN);
    resp.headers_mut().append(header::SET_COOKIE, web.cookie(SESSION_COOKIE, "", "Lax", 0));
    resp
}

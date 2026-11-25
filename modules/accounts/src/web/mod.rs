//! The hosted browser pages under `/accounts/oidc`: the email-and-password sign-in form
//! with "Continue with Epic" when Epic web OAuth is configured, the signed-in landing page
//! and logout. A successful sign-in mints a browser session (`session.rs`), never a
//! player bearer.

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

use crate::epic_oauth::{self, EpicOAuth, Intent};
use crate::{normalize_email, Service};

pub(crate) const PREFIX: &str = "/accounts/oidc";
pub(crate) const LANDING: &str = "/accounts/oidc/";
pub(crate) const LOGIN: &str = "/accounts/oidc/login";
pub(crate) const SESSION_COOKIE: &str = "swarog_session";
pub(crate) const LOGIN_CSRF_COOKIE: &str = "swarog_login_csrf";
const LOGIN_CSRF_MAX_AGE_SECS: u32 = 600;
pub(crate) const MAX_RETURN_TO_BYTES: usize = 2048;
const GENERIC_LOGIN_ERROR: &str = "Invalid email or password.";
const EXPIRED_FORM_ERROR: &str = "The sign-in form expired. Please try again.";
const BUSY_ERROR: &str = "Sign-in is busy. Please try again in a moment.";
const EPIC_ERROR: &str = "Sign-in with Epic failed. Please try again.";

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

/// An accounts cookie scoped to the hosted pages.
pub(crate) fn cookie(secure: bool, name: &str, value: &str, same_site: &str, max_age: u64) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; HttpOnly; SameSite={same_site}; Path={PREFIX}; Max-Age={max_age}{secure}"
    ))
    .expect("cookie value is ASCII")
}

/// The `swarog_session` cookie carrying a browser session token.
pub(crate) fn session_cookie(secure: bool, token: &str) -> HeaderValue {
    let max_age = u64::from(session::BROWSER_SESSION_TTL_HOURS.unsigned_abs()) * 3600;
    cookie(secure, SESSION_COOKIE, token, "Lax", max_age)
}

/// The one per-IP sign-in bucket: the hosted form POSTs and the JSON Epic start check it
/// first, keyed by the client address behind `TRUSTED_PROXY_CIDRS`.
pub(crate) struct LoginLimiter {
    limiter: Arc<httpmw::IpLimiter>,
    trusted: Vec<ipnet::IpNet>,
}

impl LoginLimiter {
    pub(crate) fn new(limiter: Arc<httpmw::IpLimiter>, trusted: Vec<ipnet::IpNet>) -> Arc<LoginLimiter> {
        Arc::new(LoginLimiter { limiter, trusted })
    }

    fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        let xri = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
        httpmw::client_ip(peer.ip(), xff, xri, &self.trusted)
    }

    /// Takes a token from the request's client bucket; `false` is answered with 429.
    pub(crate) fn allow(&self, peer: SocketAddr, headers: &HeaderMap) -> bool {
        self.limiter.allow(self.client_ip(peer, headers))
    }
}

/// What the hosted pages share: the service, the templates, the per-IP login limiter and
/// the Epic web OAuth flow when it is configured.
pub(crate) struct Web {
    svc: Arc<Service>,
    env: minijinja::Environment<'static>,
    cookie_secure: bool,
    limiter: Arc<LoginLimiter>,
    epic: Option<Arc<EpicOAuth>>,
}

impl Web {
    pub(crate) fn new(
        svc: Arc<Service>,
        cookie_secure: bool,
        limiter: Arc<LoginLimiter>,
        epic: Option<Arc<EpicOAuth>>,
    ) -> anyhow::Result<Web> {
        let mut env = minijinja::Environment::new();
        for (name, source) in [
            ("base.html", BASE_TMPL),
            ("login.html", LOGIN_TMPL),
            ("landing.html", LANDING_TMPL),
        ] {
            env.add_template(name, source)?;
        }
        Ok(Web { svc, env, cookie_secure, limiter, epic })
    }

    /// The per-IP sign-in limiter both form POSTs check first: `Some` is the 429 form.
    fn limited(&self, peer: SocketAddr, headers: &HeaderMap, jar: &CookieJar, return_to: &str) -> Option<Response> {
        if self.limiter.allow(peer, headers) {
            return None;
        }
        let mut resp = self.render_login(jar, StatusCode::TOO_MANY_REQUESTS, BUSY_ERROR, return_to);
        resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        Some(resp)
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
    /// well formed, so two open tabs share it, else a fresh one. Either way the cookie is
    /// set on the response, so its `Max-Age` runs from the form the browser now shows.
    fn render_login(&self, jar: &CookieJar, status: StatusCode, error: &str, return_to: &str) -> Response {
        let existing = jar.get(LOGIN_CSRF_COOKIE).map(|c| c.value()).filter(|v| well_formed_token(v));
        let csrf = existing.map_or_else(crate::store::new_token, str::to_string);
        let view = LoginView { error, csrf: &csrf, return_to, epic: self.epic.is_some() };
        let mut resp = self.render("login.html", status, view);
        resp.headers_mut().append(header::SET_COOKIE, self.login_csrf_cookie(&csrf));
        resp
    }

    fn cookie(&self, name: &str, value: &str, same_site: &str, max_age: u64) -> HeaderValue {
        cookie(self.cookie_secure, name, value, same_site, max_age)
    }

    fn login_csrf_cookie(&self, token: &str) -> HeaderValue {
        self.cookie(LOGIN_CSRF_COOKIE, token, "Strict", u64::from(LOGIN_CSRF_MAX_AGE_SECS))
    }

    fn session_cookie(&self, token: &str) -> HeaderValue {
        session_cookie(self.cookie_secure, token)
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
    epic: bool,
}

#[derive(Serialize)]
struct LandingView<'a> {
    handle: &'a str,
    csrf: &'a str,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginQuery {
    return_to: Option<String>,
    epic: Option<String>,
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
struct EpicForm {
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
        .route("/accounts/oidc/epic", post(epic_start))
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
            "default-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'",
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

pub(crate) fn see_other(location: &str) -> Response {
    let location = HeaderValue::from_str(location).unwrap_or_else(|_| HeaderValue::from_static(LANDING));
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}

/// Where a sign-in may send the browser: a path under `/accounts/oidc/` with its query
/// kept, at most [`MAX_RETURN_TO_BYTES`], printable ASCII, with no `//`, no backslash, no
/// `#` and no dot segment (a browser would resolve one out of the prefix, and drops a
/// fragment before resolving). Anything else is the landing page.
pub(crate) fn safe_return_to(raw: Option<&str>) -> &str {
    let Some(raw) = raw else { return LANDING };
    let path = raw.split_once('?').map_or(raw, |(path, _)| path);
    let ok = raw.len() <= MAX_RETURN_TO_BYTES
        && raw.starts_with(LANDING)
        && raw.bytes().all(|b| b.is_ascii_graphic())
        && !raw.contains("//")
        && !raw.contains('\\')
        && !raw.contains('#')
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
    Query(q): Query<LoginQuery>,
) -> Response {
    let return_to = safe_return_to(q.return_to.as_deref());
    let error = if q.epic.as_deref() == Some("error") { EPIC_ERROR } else { "" };
    match web.signed_in(&jar).await {
        Ok(Some(_)) => see_other(return_to),
        Ok(None) => web.render_login(&jar, StatusCode::OK, error, return_to),
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
    if let Some(busy) = web.limited(peer, &headers, &jar, return_to) {
        return busy;
    }
    let cookie_csrf = jar.get(LOGIN_CSRF_COOKIE).map(|c| c.value()).unwrap_or("");
    if cookie_csrf.is_empty() || !ct_eq(cookie_csrf.as_bytes(), form.csrf.as_bytes()) {
        return web.render_login(&jar, StatusCode::FORBIDDEN, EXPIRED_FORM_ERROR, return_to);
    }
    let email = normalize_email(&form.email);
    let verified = match web.svc.verify_password(&email, form.password).await {
        Ok(verified) => verified,
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
    // A browser already signed in gets a new session; the one it held ends here, not at expiry.
    if let Some(old) = jar.get(SESSION_COOKIE) {
        if let Err(e) = web.svc.store.delete_browser_session(old.value()).await {
            tracing::error!(err = %e, "replaced browser session delete failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "sign-in failed").into_response();
        }
    }
    let minted = match web.svc.issue_password_browser_session(&verified).await {
        Ok(minted) => minted,
        Err(e) if e.status == opsapi::Status::Unauthorized => {
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

/// `POST /accounts/oidc/epic` — per-IP limiter, login-CSRF double submit, then a `browser`
/// state row and a 303 to the Epic authorize URL. 404 when Epic web OAuth is not configured.
async fn epic_start(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<EpicForm>,
) -> Response {
    let return_to = safe_return_to(Some(&form.return_to));
    if let Some(busy) = web.limited(peer, &headers, &jar, return_to) {
        return busy;
    }
    let Some(epic) = web.epic.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cookie_csrf = jar.get(LOGIN_CSRF_COOKIE).map(|c| c.value()).unwrap_or("");
    if cookie_csrf.is_empty() || !ct_eq(cookie_csrf.as_bytes(), form.csrf.as_bytes()) {
        return web.render_login(&jar, StatusCode::FORBIDDEN, EXPIRED_FORM_ERROR, return_to);
    }
    // The callback path never receives this cookie, so the state carries the session it replaces.
    let replaces = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value())
        .filter(|token| crate::session_token_within_cap(token))
        .map(session::digest);
    let binding = epic_oauth::browser_binding(&jar);
    let intent = Intent::Browser { return_to: return_to.to_string(), replaces };
    let state = match epic.new_state(intent, binding.clone()).await {
        Ok(state) => state,
        Err(e) => {
            tracing::error!(err = %e, "epic browser start: state persistence failed");
            return web.render_login(&jar, StatusCode::SERVICE_UNAVAILABLE, BUSY_ERROR, return_to);
        }
    };
    let location = match epic.authorize_url_for(&state).map(|url| HeaderValue::from_str(&url)) {
        Ok(Ok(location)) => location,
        Ok(Err(e)) => {
            tracing::error!(err = %e, "epic browser start: authorize url is not a header value");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
        Err(e) => {
            tracing::error!(err = %e, "epic browser start: authorize url build failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };
    let mut resp = (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
    resp.headers_mut().append(header::SET_COOKIE, epic_oauth::binding_set_cookie(&binding, web.cookie_secure));
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

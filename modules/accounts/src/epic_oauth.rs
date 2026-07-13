//! The Epic Account Services authorization-code flow (port of Go's
//! `modules/accounts/epic_oauth.go`). The backend is the confidential client (holds
//! the secret); the browser only ever sees the redirect. A short-lived in-memory
//! state store binds an in-flight authorization to the session that started it, so
//! the callback knows whether to LINK (bearer present at start) or LOG IN.
//!
//! These two routes are HTTP-NATIVE (a browser redirect flow with an external
//! contract), NOT typed operations: they are mounted on the shared `Context` router
//! (`ctx.mount`, the Rust twin of Go's `ctx.Mux`) by the module's `init`, so they
//! serve on whichever process hosts the accounts module — monolith and accounts-svc
//! alike (the gateway HTTP passthrough for the split front lands in Step 7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use url::Host;

use crate::epic::{short_id, OidcVerifier};
use crate::Service;

/// How long an issued OAuth `state` stays redeemable (Go's `stateTTL`).
const STATE_TTL: Duration = Duration::from_secs(10 * 60);
const BINDING_COOKIE: &str = "epic_oauth_binding";
const BINDING_TOKEN_LEN: usize = 43;

/// One in-flight authorization. An empty `session_token` is a LOGIN flow; a set one
/// is a LINK flow bound to that session's player.
struct OauthState {
    session_token: String,
    browser_binding: String,
    created_at: Instant,
}

/// The confidential-client half of the EAS web OAuth flow: builds authorize URLs,
/// tracks states, exchanges codes for id_tokens.
pub(crate) struct EpicOAuth {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub authorize_url: String,
    pub token_url: String,
    pub verifier: Arc<OidcVerifier>,
    pub http: reqwest::Client,
    cookie_secure: bool,
    states: Mutex<HashMap<String, OauthState>>,
}

impl EpicOAuth {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        authorize_url: String,
        token_url: String,
        verifier: Arc<OidcVerifier>,
    ) -> anyhow::Result<EpicOAuth> {
        let parsed_redirect = url::Url::parse(&redirect_uri)
            .map_err(|err| anyhow::anyhow!("invalid EPIC_REDIRECT_URI: {err}"))?;
        if parsed_redirect.host().is_none() {
            anyhow::bail!("invalid EPIC_REDIRECT_URI: host is required");
        }
        if parsed_redirect.path() != "/accounts/epic/callback" {
            anyhow::bail!(
                "invalid EPIC_REDIRECT_URI: path must be /accounts/epic/callback"
            );
        }
        if parsed_redirect.fragment().is_some() {
            anyhow::bail!("invalid EPIC_REDIRECT_URI: fragments are not allowed");
        }
        let cookie_secure = match parsed_redirect.scheme() {
            "https" => true,
            "http" if is_loopback(&parsed_redirect) => false,
            "http" => anyhow::bail!(
                "invalid EPIC_REDIRECT_URI: HTTP is allowed only for localhost or a loopback IP"
            ),
            _ => anyhow::bail!("invalid EPIC_REDIRECT_URI: scheme must be HTTPS or loopback HTTP"),
        };

        Ok(EpicOAuth {
            client_id,
            client_secret,
            redirect_uri,
            authorize_url,
            token_url,
            verifier,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?,
            cookie_secure,
            states: Mutex::new(HashMap::new()),
        })
    }

    /// Issues a fresh `state` bound to `session_token` (empty = login flow),
    /// opportunistically GCing expired entries (Go's `newState`).
    pub(crate) fn new_state(&self, session_token: String, browser_binding: String) -> String {
        let s = crate::store::new_token();
        let mut states = self.states.lock().unwrap();
        states.retain(|_, v| v.created_at.elapsed() <= STATE_TTL);
        states.insert(
            s.clone(),
            OauthState {
                session_token,
                browser_binding,
                created_at: Instant::now(),
            },
        );
        s
    }

    /// Redeems a `state` exactly once from the browser that started it. A missing or
    /// wrong binding does not consume an otherwise valid state.
    pub(crate) fn take_state(&self, s: &str, browser_binding: Option<&str>) -> Option<String> {
        self.take_state_at_inner(s, browser_binding, Instant::now())
    }

    fn take_state_at_inner(
        &self,
        s: &str,
        browser_binding: Option<&str>,
        now: Instant,
    ) -> Option<String> {
        let mut states = self.states.lock().unwrap();
        let st = states.get(s)?;
        if browser_binding != Some(st.browser_binding.as_str()) {
            return None;
        }
        if now.saturating_duration_since(st.created_at) > STATE_TTL {
            states.remove(s);
            return None;
        }
        states.remove(s).map(|st| st.session_token)
    }

    #[cfg(test)]
    pub(crate) fn take_state_at(
        &self,
        s: &str,
        browser_binding: Option<&str>,
        now: Instant,
    ) -> Option<String> {
        self.take_state_at_inner(s, browser_binding, now)
    }

    /// The full authorize URL the page redirects the browser to.
    fn authorize_url_for(&self, state: &str) -> anyhow::Result<String> {
        let mut u = url::Url::parse(&self.authorize_url)?;
        u.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("scope", "openid basic_profile")
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("state", state);
        Ok(u.into())
    }

    /// Swaps an authorization code for tokens at the token endpoint (client-secret
    /// basic auth, form body) and returns the `id_token` (Go's `exchangeCode`).
    pub(crate) async fn exchange_code(&self, code: &str) -> anyhow::Result<String> {
        let resp = self
            .http
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect_uri),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("token endpoint returned {status}: {}", truncate(&body, 2048));
        }
        #[derive(serde::Deserialize)]
        struct TokenResp {
            #[serde(default)]
            id_token: String,
        }
        let tr: TokenResp = resp.json().await?;
        if tr.id_token.is_empty() {
            anyhow::bail!("no id_token in token response (is the openid scope enabled for the app?)");
        }
        Ok(tr.id_token)
    }
}

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn valid_binding(value: &str) -> bool {
    value.len() == BINDING_TOKEN_LEN
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

fn binding_set_cookie(binding: &str, secure: bool) -> axum::http::HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    axum::http::HeaderValue::from_str(&format!(
        "{BINDING_COOKIE}={binding}; HttpOnly; SameSite=Lax; Path=/accounts/epic; Max-Age=600{secure}"
    ))
    .expect("generated OAuth binding is ASCII")
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The two HTTP-native routes, as an axum sub-router the module `ctx.mount`s:
/// `POST /accounts/epic/start` and `GET /accounts/epic/callback`.
pub(crate) fn router(oauth: Arc<EpicOAuth>, svc: Arc<Service>) -> Router {
    let start_oauth = oauth.clone();
    let start_svc = svc.clone();
    Router::new()
        .route(
            "/accounts/epic/start",
            post(move |jar: CookieJar, headers: HeaderMap| {
                let oauth = start_oauth.clone();
                let svc = start_svc.clone();
                async move { handle_start(oauth, svc, jar, headers).await }
            }),
        )
        .route(
            "/accounts/epic/callback",
            get(move |jar: CookieJar, Query(q): Query<HashMap<String, String>>| {
                let oauth = oauth.clone();
                let svc = svc.clone();
                async move { handle_callback(oauth, svc, jar, q).await }
            }),
        )
}

/// Builds the authorize URL. Called via fetch with the user's bearer token: if
/// present and valid, this becomes a LINK flow bound to that player; otherwise a
/// plain login flow. Returns JSON so the page can redirect (Go's `handleEpicStart`).
async fn handle_start(
    oauth: Arc<EpicOAuth>,
    svc: Arc<Service>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let mut session_token = String::new();
    if let Some(tok) = bearer(&headers) {
        if matches!(svc.store.player_by_session(&tok).await, Ok(Some(_))) {
            session_token = tok;
        }
    }
    let browser_binding = jar
        .get(BINDING_COOKIE)
        .map(|cookie| cookie.value())
        .filter(|value| valid_binding(value))
        .map(str::to_owned)
        .unwrap_or_else(crate::store::new_token);
    let state = oauth.new_state(session_token, browser_binding.clone());
    let mut response = match oauth.authorize_url_for(&state) {
        Ok(u) => Json(serde_json::json!({ "authorize_url": u })).into_response(),
        Err(err) => {
            tracing::error!(%err, "epic start: authorize url build failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        binding_set_cookie(&browser_binding, oauth.cookie_secure),
    );
    response
}

/// Where Epic redirects back. Exchanges the code, verifies the id_token, then LINKS
/// to the originating session's player or LOGS IN (provisioning on first sight —
/// which emits `player.registered` durably inside the store tx). Failures redirect
/// to `/?epic=error` exactly as Go's `handleEpicCallback`.
async fn handle_callback(
    oauth: Arc<EpicOAuth>,
    svc: Arc<Service>,
    jar: CookieJar,
    q: HashMap<String, String>,
) -> Response {
    let code = q.get("code").cloned().unwrap_or_default();
    let state = q.get("state").cloned().unwrap_or_default();
    if code.is_empty() || state.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing code or state").into_response();
    }
    let browser_binding = jar.get(BINDING_COOKIE).map(|cookie| cookie.value());
    let Some(session_token) = oauth.take_state(&state, browser_binding) else {
        return (StatusCode::BAD_REQUEST, "invalid or expired state").into_response();
    };

    let id_token = match oauth.exchange_code(&code).await {
        Ok(t) => t,
        Err(err) => {
            tracing::error!(%err, "epic code exchange failed");
            return Redirect::to("/?epic=error").into_response();
        }
    };
    let subject = match oauth.verifier.verify(&id_token).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "epic id_token rejected");
            return Redirect::to("/?epic=error").into_response();
        }
    };

    // LINK flow: attach the Epic identity to the already-logged-in player.
    if !session_token.is_empty() {
        let Ok(Some(p)) = svc.store.player_by_session(&session_token).await else {
            return Redirect::to("/?epic=error").into_response();
        };
        match svc.store.link_identity(&p.id, "epic", &subject).await {
            Ok(()) => {}
            Err(crate::store::StoreError::Taken) => {
                tracing::warn!(player_id = %p.id, "epic link: identity already linked to a different player");
                return Redirect::to("/?epic=error").into_response();
            }
            Err(err) => {
                tracing::error!(%err, "epic link failed");
                return Redirect::to("/?epic=error").into_response();
            }
        }
        return Redirect::to("/?epic=linked").into_response();
    }

    // LOGIN flow: find or create a player for this Epic identity (durable
    // player.registered on first sight), mint a session, hand the token back via the
    // URL fragment for the page to pick up.
    let session = match svc
        .external_login("epic", &subject, &format!("epic:{}", short_id(&subject)))
        .await
    {
        Ok((session, _created)) => session,
        Err(err) => {
            tracing::error!(%err, "epic login failed");
            return Redirect::to("/?epic=error").into_response();
        }
    };
    Redirect::to(&format!("/#token={}", session.token)).into_response()
}

/// Extracts the token from an `Authorization: Bearer <token>` header, or `None`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(str::to_string)
}

//! Epic's own surface on the neutral OAuth flow ([`crate::oauth_flow`], port of Go's
//! `modules/accounts/epic_oauth.go`): the JSON start for the demo page, the LOGIN (no
//! bearer) and LINK (bearer present at the start) intents, and the `/accounts/epic/*`
//! routes. The hosted sign-in form's `Browser` intent runs in the neutral flow.
//!
//! These two routes are HTTP-NATIVE (a browser redirect flow with an external
//! contract), NOT typed operations: they are mounted on the shared `Context` router
//! (`ctx.mount`, the Rust twin of Go's `ctx.Mux`) by the module's `init`, so they
//! serve on whichever process hosts the accounts module — monolith and accounts-svc
//! alike.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Query};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use sqlx::PgPool;

use crate::oauth_flow::{
    self, browser_binding, binding_set_cookie, ClientConfig, HookIntent, Intent, OAuthFlow, Outcome, ProviderSpec,
};
use crate::oidc::OidcVerifier;
use crate::providers::{EpicOAuthConfig, EPIC};
use crate::{web, Service};

pub(crate) static SPEC: ProviderSpec = ProviderSpec {
    name: EPIC,
    scope: "openid basic_profile",
    callback_path: "/accounts/epic/callback",
    error_key: "epic",
    display_prefix: "epic",
};

/// The EAS web OAuth flow: the neutral flow under Epic's [`SPEC`].
pub(crate) struct EpicOAuth(OAuthFlow);

impl Deref for EpicOAuth {
    type Target = OAuthFlow;

    fn deref(&self) -> &OAuthFlow {
        &self.0
    }
}

impl EpicOAuth {
    /// Takes an ALREADY-VALIDATED configuration: every URL rule belongs to
    /// `providers::EpicOAuthConfig::new`, so there is exactly one spelling of it and
    /// a value cannot be accepted here after being rejected there. `cookie_secure` is the
    /// accounts-wide `ACCOUNTS_COOKIE_SECURE` rule.
    pub fn new(
        client_id: String,
        oauth: EpicOAuthConfig,
        verifier: Arc<OidcVerifier>,
        pool: PgPool,
        cookie_secure: bool,
    ) -> anyhow::Result<EpicOAuth> {
        let client = ClientConfig {
            client_id,
            client_secret: oauth.client_secret,
            redirect_uri: oauth.redirect_uri,
            authorize_url: oauth.authorize_url,
            token_url: oauth.token_url,
        };
        Ok(EpicOAuth(OAuthFlow::new(&SPEC, client, verifier, pool, cookie_secure)?))
    }
}

/// The two HTTP-native routes, as an axum sub-router `web::hosted_browser_router` merges in:
/// `POST /accounts/epic/start` and `GET /accounts/epic/callback`. The start draws from
/// `limiter`, the hosted sign-in form's bucket.
pub(crate) fn router(oauth: Arc<EpicOAuth>, svc: Arc<Service>, limiter: Arc<web::LoginLimiter>) -> Router {
    let start_oauth = oauth.clone();
    let start_svc = svc.clone();
    Router::new()
        .route(
            "/accounts/epic/start",
            post(
                move |ConnectInfo(peer): ConnectInfo<SocketAddr>,
                      jar: CookieJar,
                      headers: HeaderMap,
                      Query(q): Query<HashMap<String, String>>| {
                    let oauth = start_oauth.clone();
                    let svc = start_svc.clone();
                    let limiter = limiter.clone();
                    async move {
                        if !limiter.allow(peer, &headers) {
                            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")], "too many requests")
                                .into_response();
                        }
                        let client_id = q.get("client_id").map(String::as_str).unwrap_or_default();
                        handle_start(oauth, svc, jar, headers, client_id).await
                    }
                },
            ),
        )
        .route(
            SPEC.callback_path,
            get(move |jar: CookieJar, Query(q): Query<HashMap<String, String>>| {
                let oauth = oauth.clone();
                let svc = svc.clone();
                async move {
                    oauth_flow::handle_callback(&oauth, &svc, &jar, &q, |outcome| complete(&svc, outcome)).await
                }
            })
            .layer(middleware::map_response(oauth_flow::no_store)),
        )
}

/// Builds the authorize URL. Called via fetch with the user's bearer token: if
/// present and valid, this becomes a LINK flow bound to that player; otherwise a
/// plain login flow for `client_id`. Returns JSON so the page can redirect (Go's
/// `handleEpicStart`). A missing, over-cap or unknown `client_id` is 401 before any state.
async fn handle_start(
    oauth: Arc<EpicOAuth>,
    svc: Arc<Service>,
    jar: CookieJar,
    headers: HeaderMap,
    client_id: &str,
) -> Response {
    if !crate::client_id_within_cap(client_id) {
        return invalid_client();
    }
    let mut intent = Intent::Login { client_id: client_id.to_string() };
    if let Some(tok) = bearer(&headers) {
        match svc.store.player_by_session(&tok).await {
            // Valid session ⇒ LINK flow bound to that player.
            Ok(Some(_)) => intent = Intent::Link { session_token: tok },
            // No such session ⇒ genuine LOGIN flow.
            Ok(None) => {}
            // Store outage: DO NOT fold into "no session" — that would silently
            // downgrade a LINK request (bearer present) into a LOGIN that
            // PROVISIONS A NEW PLAYER at callback (duplicate-account risk). A
            // transient lookup failure is retryable, never bad credentials, so
            // fail closed with 503 (the gateway verifier.rs "503-not-401"
            // precedent) BEFORE any state is minted — an unresolvable request
            // leaves no redeemable state behind.
            Err(err) => {
                tracing::error!(%err, "epic start: session lookup failed");
                return (StatusCode::SERVICE_UNAVAILABLE, "internal error, try again")
                    .into_response();
            }
        }
    }
    match svc.store.client(client_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return invalid_client(),
        Err(err) => {
            tracing::error!(%err, "epic start: client lookup failed");
            return (StatusCode::SERVICE_UNAVAILABLE, "internal error, try again").into_response();
        }
    }
    let browser_binding = browser_binding(&jar);
    let state = match oauth.new_state(intent, browser_binding.clone()).await {
        Ok(s) => s,
        // Persisting the state failed (store outage) — retryable, never bad input.
        // Fail closed with 503 (the session-lookup 503 precedent above) BEFORE any
        // authorize redirect, so the browser never leaves with an un-redeemable state.
        Err(err) => {
            tracing::error!(%err, "epic start: state persistence failed");
            return (StatusCode::SERVICE_UNAVAILABLE, "internal error, try again")
                .into_response();
        }
    };
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

/// Epic's own intents once the neutral callback has redeemed the state and verified the
/// id_token: LINK to the originating session's player or LOG IN (provisioning on first
/// sight, which emits `player.registered` durably inside the store tx). Every failure
/// redirects to `/?epic=error` exactly as Go's `handleEpicCallback`.
async fn complete(svc: &Service, outcome: Outcome) -> Response {
    let (intent, subject) = match outcome {
        Outcome::Verified { intent, subject } => (intent, subject),
        Outcome::Failed => return Redirect::to("/?epic=error").into_response(),
    };
    let session_token = match intent {
        HookIntent::Link { session_token } => session_token,
        HookIntent::Login { client_id } => return login(svc, &subject, &client_id).await,
    };
    // LINK flow: attach the Epic identity to the already-logged-in player.
    let p = match svc.store.player_by_session(&session_token).await {
        Ok(Some(p)) => p,
        // Session gone (expired/revoked between start and callback): the
        // link target no longer exists — visibly an error redirect.
        Ok(None) => return Redirect::to("/?epic=error").into_response(),
        // Store outage: the visible behavior stays an error redirect — a
        // browser callback can't render a 503 meaningfully and there is no
        // safe way to complete the LINK, so the redirect is correct; the
        // LOG is the fix (the old `let Ok(Some(_)) else` swallowed this
        // silently, indistinguishable from a truly-missing session).
        Err(err) => {
            tracing::error!(%err, "epic link: session lookup failed");
            return Redirect::to("/?epic=error").into_response();
        }
    };
    match svc.link_identity(&p.id, SPEC.name, &subject).await {
        Ok(_) => {}
        Err(err) if err.status == opsapi::Status::Conflict => {
            tracing::warn!(player_id = %p.id, "epic link: identity already linked to a different player");
            return Redirect::to("/?epic=error").into_response();
        }
        Err(err) => {
            tracing::error!(%err, "epic link failed");
            return Redirect::to("/?epic=error").into_response();
        }
    }
    Redirect::to("/?epic=linked").into_response()
}

/// LOGIN flow: find or create a player for this Epic identity (durable
/// player.registered on first sight), mint a session for the client that started the
/// login, hand BOTH tokens back via the URL fragment for the page to pick up — the
/// access token alone would strand the browser at its 60-minute expiry with nothing to
/// renew from. A client deleted since the start fails the mint.
async fn login(svc: &Service, subject: &str, client_id: &str) -> Response {
    let session = match svc
        .external_login(SPEC.name, subject, &SPEC.display_name(subject), None, client_id)
        .await
    {
        Ok((session, _created)) => session,
        Err(err) => {
            tracing::error!(%err, "epic login failed");
            return Redirect::to("/?epic=error").into_response();
        }
    };
    Redirect::to(&format!(
        "/#token={}&refresh={}",
        session.token, session.refresh_token
    ))
    .into_response()
}

fn invalid_client() -> Response {
    (StatusCode::UNAUTHORIZED, "invalid client").into_response()
}

/// Extracts the token from an `Authorization: Bearer <token>` header, or `None`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(str::to_string)
}

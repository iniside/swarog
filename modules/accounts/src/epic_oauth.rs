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

use crate::epic::{short_id, OidcVerifier};
use crate::Service;

/// How long an issued OAuth `state` stays redeemable (Go's `stateTTL`).
const STATE_TTL: Duration = Duration::from_secs(10 * 60);

/// One in-flight authorization. An empty `session_token` is a LOGIN flow; a set one
/// is a LINK flow bound to that session's player.
struct OauthState {
    session_token: String,
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
            states: Mutex::new(HashMap::new()),
        })
    }

    /// Issues a fresh `state` bound to `session_token` (empty = login flow),
    /// opportunistically GCing expired entries (Go's `newState`).
    pub(crate) fn new_state(&self, session_token: String) -> String {
        let s = crate::store::new_token();
        let mut states = self.states.lock().unwrap();
        states.retain(|_, v| v.created_at.elapsed() <= STATE_TTL);
        states.insert(
            s.clone(),
            OauthState {
                session_token,
                created_at: Instant::now(),
            },
        );
        s
    }

    /// Redeems a `state` exactly once; an unknown or expired state is `None`.
    pub(crate) fn take_state(&self, s: &str) -> Option<String> {
        let st = self.states.lock().unwrap().remove(s)?;
        if st.created_at.elapsed() > STATE_TTL {
            return None;
        }
        Some(st.session_token)
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
            post(move |headers: HeaderMap| {
                let oauth = start_oauth.clone();
                let svc = start_svc.clone();
                async move { handle_start(oauth, svc, headers).await }
            }),
        )
        .route(
            "/accounts/epic/callback",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let oauth = oauth.clone();
                let svc = svc.clone();
                async move { handle_callback(oauth, svc, q).await }
            }),
        )
}

/// Builds the authorize URL. Called via fetch with the user's bearer token: if
/// present and valid, this becomes a LINK flow bound to that player; otherwise a
/// plain login flow. Returns JSON so the page can redirect (Go's `handleEpicStart`).
async fn handle_start(oauth: Arc<EpicOAuth>, svc: Arc<Service>, headers: HeaderMap) -> Response {
    let mut session_token = String::new();
    if let Some(tok) = bearer(&headers) {
        if matches!(svc.store.player_by_session(&tok).await, Ok(Some(_))) {
            session_token = tok;
        }
    }
    let state = oauth.new_state(session_token);
    match oauth.authorize_url_for(&state) {
        Ok(u) => Json(serde_json::json!({ "authorize_url": u })).into_response(),
        Err(err) => {
            tracing::error!(%err, "epic start: authorize url build failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}

/// Where Epic redirects back. Exchanges the code, verifies the id_token, then LINKS
/// to the originating session's player or LOGS IN (provisioning on first sight —
/// which emits `player.registered` durably inside the store tx). Failures redirect
/// to `/?epic=error` exactly as Go's `handleEpicCallback`.
async fn handle_callback(
    oauth: Arc<EpicOAuth>,
    svc: Arc<Service>,
    q: HashMap<String, String>,
) -> Response {
    let code = q.get("code").cloned().unwrap_or_default();
    let state = q.get("state").cloned().unwrap_or_default();
    if code.is_empty() || state.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing code or state").into_response();
    }
    let Some(session_token) = oauth.take_state(&state) else {
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
                // The (provider, subject) is already claimed. Report success ONLY when
                // it is this same player's own identity (an idempotent re-link); an
                // Epic account bound to a DIFFERENT player must not read as linked.
                match svc.store.player_by_identity("epic", &subject).await {
                    Ok(Some(owner)) if owner.id == p.id => {}
                    Ok(Some(_)) => {
                        tracing::warn!(player_id = %p.id, "epic link: identity already linked to a different player");
                        return Redirect::to("/?epic=error").into_response();
                    }
                    Ok(None) => {
                        tracing::warn!(player_id = %p.id, "epic link: taken identity vanished (race)");
                        return Redirect::to("/?epic=error").into_response();
                    }
                    Err(err) => {
                        tracing::warn!(%err, player_id = %p.id, "epic link: owner lookup failed after taken");
                        return Redirect::to("/?epic=error").into_response();
                    }
                }
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
    let p = match svc
        .find_or_create_external("epic", &subject, &format!("epic:{}", short_id(&subject)))
        .await
    {
        Ok((p, _created)) => p,
        Err(err) => {
            tracing::error!(%err, "epic login failed");
            return Redirect::to("/?epic=error").into_response();
        }
    };
    match svc.store.new_session(&p.id).await {
        Ok(token) => Redirect::to(&format!("/#token={token}")).into_response(),
        Err(_) => Redirect::to("/?epic=error").into_response(),
    }
}

/// Extracts the token from an `Authorization: Bearer <token>` header, or `None`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(str::to_string)
}

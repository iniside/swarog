//! The provider-neutral half of an OAuth authorization-code flow: the state store, the
//! browser binding cookie, the authorize URL, the code exchange and the hosted-page
//! (`Browser`) callback. A [`ProviderSpec`] names what differs per provider; every other
//! intent goes back to the provider through the callback's hook.
//!
//! The state store is shared Postgres (`accounts.oauth_states`), NOT process memory, so
//! the callback can LB-route to ANY replica: `new_state` INSERTs and `take_state` is a
//! `DELETE ... RETURNING` (cross-replica exactly-once single-redemption).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use sqlx::PgPool;

use crate::oidc::{short_id, truncate, OidcVerifier};
use crate::{web, Service};

/// How long an issued OAuth `state` stays redeemable (Go's `stateTTL`). Enforced as
/// a `created_at` predicate in SQL (`STATE_TTL_SQL`), not an in-process clock.
const STATE_TTL_SQL: &str = "10 minutes";
const BINDING_COOKIE: &str = "epic_oauth_binding";
const BINDING_TOKEN_LEN: usize = 43;
/// Covers every start (`/accounts/epic/start`, the hosted form's `/accounts/oidc/<provider>`)
/// and every callback, so every start in one browser reuses the same binding.
const BINDING_PATH: &str = "/accounts";

/// What differs between providers in the neutral flow.
pub(crate) struct ProviderSpec {
    /// The identity provider name, also `accounts.oauth_states.provider`.
    pub name: &'static str,
    pub scope: &'static str,
    /// The callback route, and the path every configured redirect URI must have.
    pub callback_path: &'static str,
    /// The query key the sign-in page reads a failed browser sign-in from.
    pub error_key: &'static str,
    pub display_prefix: &'static str,
}

impl ProviderSpec {
    /// The display name a player first created by this provider gets.
    pub(crate) fn display_name(&self, subject: &str) -> String {
        format!("{}:{}", self.display_prefix, short_id(subject))
    }
}

/// What the callback does with a redeemed state (`accounts.oauth_states.intent`).
#[derive(Debug, PartialEq)]
pub(crate) enum Intent {
    /// Find or create the player and hand a player session to the demo page.
    Login,
    /// Attach the identity to the player behind this bearer session.
    Link { session_token: String },
    /// Find or create the player, mint a browser session ending the one whose digest is
    /// `replaces` in the same commit, and go to `return_to`.
    Browser { return_to: String, replaces: Option<Vec<u8>> },
}

/// The intents the callback hands the provider's hook: every [`Intent`] but `Browser`,
/// which the callback signs in itself.
#[derive(Debug, PartialEq)]
pub(crate) enum HookIntent {
    /// [`Intent::Login`].
    Login,
    /// [`Intent::Link`].
    Link { session_token: String },
}

/// What the callback hands the provider's hook for an intent other than `Browser`.
pub(crate) enum Outcome {
    /// The code was exchanged and its id_token verified to `subject`.
    Verified { intent: HookIntent, subject: String },
    /// The exchange or the verify failed; the hook only renders the failure.
    Failed,
}

/// The confidential client's credentials and endpoints, already validated by the
/// provider's config parse.
#[derive(Clone)]
pub(crate) struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub authorize_url: String,
    pub token_url: String,
}

/// The confidential-client half of one provider's web OAuth flow: builds authorize URLs,
/// persists/redeems that provider's states in shared Postgres, exchanges codes for id_tokens.
pub(crate) struct OAuthFlow {
    spec: &'static ProviderSpec,
    client: ClientConfig,
    verifier: Arc<OidcVerifier>,
    http: reqwest::Client,
    pub(crate) cookie_secure: bool,
    /// The shared Postgres pool backing `accounts.oauth_states` — the redemption
    /// authority. Any replica INSERTs on start and any replica redeems on callback.
    pool: PgPool,
}

impl OAuthFlow {
    /// The identity provider this flow signs in with.
    pub(crate) fn provider(&self) -> &'static str {
        self.spec.name
    }

    /// `cookie_secure` is the accounts-wide `ACCOUNTS_COOKIE_SECURE` rule.
    pub(crate) fn new(
        spec: &'static ProviderSpec,
        client: ClientConfig,
        verifier: Arc<OidcVerifier>,
        pool: PgPool,
        cookie_secure: bool,
    ) -> anyhow::Result<OAuthFlow> {
        Ok(OAuthFlow {
            spec,
            client,
            verifier,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?,
            cookie_secure,
            pool,
        })
    }

    /// Issues a fresh `state` carrying `intent` by INSERTing it into shared Postgres,
    /// opportunistically pruning expired rows first (piggyback GC — no background task;
    /// Go's `newState`). Any replica can later redeem it. A persistence failure is a
    /// transient/retryable condition the caller surfaces as 503.
    pub(crate) async fn new_state(
        &self,
        intent: Intent,
        browser_binding: String,
    ) -> anyhow::Result<String> {
        self.prune_expired().await;
        let s = crate::store::new_token();
        let (name, session_token, return_to, replaces) = match intent {
            Intent::Login => ("login", None, None, None),
            Intent::Link { session_token } => ("link", Some(session_token), None, None),
            Intent::Browser { return_to, replaces } => ("browser", None, Some(return_to), replaces),
        };
        sqlx::query(
            "INSERT INTO accounts.oauth_states \
               (state, provider, intent, session_token, return_to, replaces_session, browser_binding) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&s)
        .bind(self.spec.name)
        .bind(name)
        .bind(session_token)
        .bind(return_to)
        .bind(replaces)
        .bind(&browser_binding)
        .execute(&self.pool)
        .await?;
        Ok(s)
    }

    /// Redeems a `state` this provider minted, exactly once, from the browser that started
    /// it via `DELETE ... RETURNING`: whichever replica runs the DELETE first wins, the rest
    /// get zero rows → `None`. A missing binding never consumes a state (early `None`); a
    /// wrong binding, an expired row or another provider's row fails the `WHERE` so no row
    /// is deleted and the state survives. The 10-min TTL is the `created_at` predicate. A
    /// `WHERE`-clause miss is `Ok(None)` — legitimately bad input the caller answers with
    /// 400. A genuine query error is `Err`: a transient store condition the caller surfaces
    /// as 503, never as bad input.
    pub(crate) async fn take_state(
        &self,
        s: &str,
        browser_binding: Option<&str>,
    ) -> Result<Option<Intent>, sqlx::Error> {
        let Some(binding) = browser_binding else {
            return Ok(None);
        };
        // Deliberately NOT logged here: the terminal handler logs this error once as
        // it renders the response, exactly as `new_state` above leaves its persistence
        // failure to its caller. Logging at both ends would make one store outage
        // count twice in the ERROR rate.
        type Row = (String, Option<String>, Option<String>, Option<Vec<u8>>);
        let row: Option<Row> = sqlx::query_as(&format!(
            "DELETE FROM accounts.oauth_states \
             WHERE state = $1 AND browser_binding = $2 AND provider = $3 \
               AND created_at > now() - interval '{STATE_TTL_SQL}' \
             RETURNING intent, session_token, return_to, replaces_session"
        ))
        .bind(s)
        .bind(binding)
        .bind(self.spec.name)
        .fetch_optional(&self.pool)
        .await?;
        let Some((intent, session_token, return_to, replaces)) = row else {
            return Ok(None);
        };
        match (intent.as_str(), session_token, return_to) {
            ("login", None, None) => Ok(Some(Intent::Login)),
            ("link", Some(session_token), None) => Ok(Some(Intent::Link { session_token })),
            ("browser", None, Some(return_to)) => Ok(Some(Intent::Browser { return_to, replaces })),
            _ => Err(sqlx::Error::Protocol(format!("oauth state row with intent {intent:?} is malformed"))),
        }
    }

    /// Opportunistic GC of every provider's expired states (piggybacked on `new_state`, no
    /// background task). Best-effort: a prune failure is logged, never fatal to minting.
    async fn prune_expired(&self) {
        if let Err(err) = sqlx::query(&format!(
            "DELETE FROM accounts.oauth_states \
             WHERE created_at <= now() - interval '{STATE_TTL_SQL}'"
        ))
        .execute(&self.pool)
        .await
        {
            tracing::warn!(%err, "{} oauth: expired-state prune failed", self.spec.name);
        }
    }

    /// The full authorize URL the page redirects the browser to.
    pub(crate) fn authorize_url_for(&self, state: &str) -> anyhow::Result<String> {
        let mut u = url::Url::parse(&self.client.authorize_url)?;
        u.query_pairs_mut()
            .append_pair("client_id", &self.client.client_id)
            .append_pair("response_type", "code")
            .append_pair("scope", self.spec.scope)
            .append_pair("redirect_uri", &self.client.redirect_uri)
            .append_pair("state", state);
        Ok(u.into())
    }

    /// Swaps an authorization code for tokens at the token endpoint (client-secret
    /// basic auth, form body) and returns the `id_token` (Go's `exchangeCode`).
    pub(crate) async fn exchange_code(&self, code: &str) -> anyhow::Result<String> {
        let resp = self
            .http
            .post(&self.client.token_url)
            .basic_auth(&self.client.client_id, Some(&self.client.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.client.redirect_uri),
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

    /// Exchanges `code` and verifies its id_token, logging the failure: the subject, or
    /// `None` when either step failed.
    async fn verified_subject(&self, code: &str) -> Option<String> {
        let id_token = match self.exchange_code(code).await {
            Ok(t) => t,
            Err(err) => {
                tracing::error!(%err, "{} code exchange failed", self.spec.name);
                return None;
            }
        };
        match self.verifier.verify(&id_token).await {
            Ok(subject) => Some(subject),
            Err(err) => {
                tracing::warn!(%err, "{} id_token rejected", self.spec.name);
                None
            }
        }
    }

    /// Where a failed browser sign-in sends the browser: the sign-in page with its `return_to`.
    fn browser_failed(&self, return_to: &str) -> Response {
        let return_to: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
        web::see_other(&format!("{}?{}=error&return_to={return_to}", web::LOGIN, self.spec.error_key))
    }
}

fn valid_binding(value: &str) -> bool {
    value.len() == BINDING_TOKEN_LEN
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

/// The browser's binding for a new state: its valid binding cookie, else a fresh one. Reusing
/// it keeps every state already started in this browser redeemable.
pub(crate) fn browser_binding(jar: &CookieJar) -> String {
    jar.get(BINDING_COOKIE)
        .map(|cookie| cookie.value())
        .filter(|value| valid_binding(value))
        .map(str::to_owned)
        .unwrap_or_else(crate::store::new_token)
}

pub(crate) fn binding_set_cookie(binding: &str, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{BINDING_COOKIE}={binding}; HttpOnly; SameSite=Lax; Path={BINDING_PATH}; Max-Age=600{secure}"
    ))
    .expect("generated OAuth binding is ASCII")
}

/// The callback route of a provider whose only intent is `Browser` (Google). The
/// `oauth_states_epic_only` CHECK keeps every other intent out of its rows, so its hook
/// answers only a row that got past that CHECK.
pub(crate) fn browser_router(flow: Arc<OAuthFlow>, svc: Arc<Service>) -> Router {
    let path = flow.spec.callback_path;
    Router::new().route(
        path,
        get(move |jar: CookieJar, Query(q): Query<HashMap<String, String>>| {
            let flow = flow.clone();
            let svc = svc.clone();
            async move {
                let provider = flow.spec.name;
                handle_callback(&flow, &svc, &jar, &q, |_| async move {
                    tracing::error!("{provider} callback: state row with a non-browser intent");
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
                })
                .await
            }
        }),
    )
}

/// Where the provider redirects back. Redeems the state, exchanges the code and verifies
/// the id_token once, then signs a `Browser` intent in here and hands any other intent,
/// or its failure, to `hook`.
pub(crate) async fn handle_callback<H, F>(
    flow: &OAuthFlow,
    svc: &Service,
    jar: &CookieJar,
    q: &HashMap<String, String>,
    hook: H,
) -> Response
where
    H: FnOnce(Outcome) -> F,
    F: Future<Output = Response>,
{
    let code = q.get("code").map(String::as_str).unwrap_or_default();
    let state = q.get("state").map(String::as_str).unwrap_or_default();
    if code.is_empty() || state.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing code or state").into_response();
    }
    let browser_binding = jar.get(BINDING_COOKIE).map(|cookie| cookie.value());
    let intent = match flow.take_state(state, browser_binding).await {
        Ok(Some(intent)) => intent,
        // A `WHERE`-clause miss (unknown/expired/wrong-binding/other-provider/already-redeemed)
        // is genuinely bad input — 400.
        Ok(None) => {
            return (StatusCode::BAD_REQUEST, "invalid or expired state").into_response();
        }
        // A store outage is transient/retryable, never the browser's fault: 503, the
        // same precedent as a `new_state` persistence failure at the start.
        //
        // KNOWN GAP (deliberate): the callback renders the SAME condition — Postgres
        // down — two ways depending on which query the outage hits. Here it is a 503
        // body; Epic's LINK-flow session lookup answers an error redirect on `Err`,
        // reasoning that a browser callback cannot render a status body meaningfully.
        // Picking ONE failure-rendering policy for the callback is a separate decision.
        Err(err) => {
            tracing::error!(%err, "{} callback: state redemption failed", flow.spec.name);
            return (StatusCode::SERVICE_UNAVAILABLE, "internal error, try again")
                .into_response();
        }
    };

    let subject = flow.verified_subject(code).await;
    match (intent, subject) {
        (Intent::Browser { return_to, replaces }, Some(subject)) => {
            browser_sign_in(flow, svc, &subject, &return_to, replaces.as_deref()).await
        }
        (Intent::Browser { return_to, .. }, None) => flow.browser_failed(&return_to),
        (Intent::Login, Some(subject)) => hook(Outcome::Verified { intent: HookIntent::Login, subject }).await,
        (Intent::Link { session_token }, Some(subject)) => {
            hook(Outcome::Verified { intent: HookIntent::Link { session_token }, subject }).await
        }
        (Intent::Login | Intent::Link { .. }, None) => hook(Outcome::Failed).await,
    }
}

/// BROWSER flow: find or create the player without a player session, end the session
/// the browser held when it started (its cookie is not sent to this path), mint a
/// browser session and follow `return_to`.
async fn browser_sign_in(
    flow: &OAuthFlow,
    svc: &Service,
    subject: &str,
    return_to: &str,
    replaces: Option<&[u8]>,
) -> Response {
    let return_to = web::safe_return_to(Some(return_to));
    let fail = || flow.browser_failed(return_to);
    let player = match svc
        .external_browser_login(flow.spec.name, subject, &flow.spec.display_name(subject))
        .await
    {
        Ok((player, _created)) => player,
        Err(err) => {
            tracing::error!(%err, "{} browser sign-in failed", flow.spec.name);
            return fail();
        }
    };
    let mut tx = match svc.store.pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(%err, "browser session mint failed");
            return fail();
        }
    };
    let minted = match svc.store.create_browser_session_tx(&mut tx, &player.id).await {
        Ok(Some(minted)) => minted,
        // The player was deleted between the find and the mint.
        Ok(None) => return fail(),
        Err(err) => {
            tracing::error!(%err, "browser session mint failed");
            return fail();
        }
    };
    // After the insert, whose foreign key takes the player row first (lock order in
    // `Store::kill_player_families_tx`), so a refused mint leaves the old session alive.
    if let Some(old) = replaces {
        if let Err(err) = svc.store.delete_browser_session_digest_tx(&mut tx, old).await {
            tracing::error!(%err, "replaced browser session delete failed");
            return fail();
        }
    }
    if let Err(err) = tx.commit().await {
        tracing::error!(%err, "browser session mint failed");
        return fail();
    }
    let mut resp = web::see_other(return_to);
    let h = resp.headers_mut();
    h.append(header::SET_COOKIE, web::session_cookie(flow.cookie_secure, &minted.token));
    h.append(header::SET_COOKIE, web::cookie(flow.cookie_secure, web::LOGIN_CSRF_COOKIE, "", "Strict", 0));
    resp
}

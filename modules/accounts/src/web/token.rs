//! `POST /accounts/oidc/token`: the token endpoint. A client authenticates (a confidential one
//! with its secret, a public one by `client_id` alone), then exchanges an authorization code
//! with its PKCE verifier, polls a device code (RFC 8628 §3.4), or rotates a refresh token of
//! its own families.

use std::collections::HashMap;
use std::sync::Arc;

use accountsapi::grant_types::{AUTHORIZATION_CODE, DEVICE_CODE, REFRESH_TOKEN};
use accountsapi::scopes::OPENID;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use opsapi::{Error, Status};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use super::Web;
use crate::clients::TokenClient;
use crate::codes::RedeemedGrant;
use crate::device_codes::DevicePoll;
use crate::secret::{ct_eq, digest};
use crate::{client_id_within_cap, internal, RefreshPresenter, Service};

pub(crate) const TOKEN: &str = "/accounts/oidc/token";
pub(crate) const MAX_TOKEN_BODY_BYTES: usize = 8 * 1024;
const MIN_VERIFIER_CHARS: usize = 43;
const MAX_VERIFIER_CHARS: usize = 128;

/// An RFC 6749 §5.2 error answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenError {
    InvalidRequest,
    /// `basic`: the request authenticated with a Basic header, so the 401 challenges it.
    InvalidClient { basic: bool },
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    /// RFC 8628 §3.5: the device code is not approved or denied yet.
    AuthorizationPending,
    /// RFC 8628 §3.5: polled sooner than its interval.
    SlowDown,
    /// RFC 8628 §3.5: the player denied the device code.
    AccessDenied,
    /// RFC 8628 §3.5: the device code expired, or its approving browser session ended.
    ExpiredToken,
    /// The database failed; never folded into a client error.
    Unavailable,
}

impl IntoResponse for TokenError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            TokenError::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid_request"),
            TokenError::InvalidClient { .. } => (StatusCode::UNAUTHORIZED, "invalid_client"),
            TokenError::InvalidGrant => (StatusCode::BAD_REQUEST, "invalid_grant"),
            TokenError::UnauthorizedClient => (StatusCode::BAD_REQUEST, "unauthorized_client"),
            TokenError::UnsupportedGrantType => (StatusCode::BAD_REQUEST, "unsupported_grant_type"),
            TokenError::InvalidScope => (StatusCode::BAD_REQUEST, "invalid_scope"),
            TokenError::AuthorizationPending => (StatusCode::BAD_REQUEST, "authorization_pending"),
            TokenError::SlowDown => (StatusCode::BAD_REQUEST, "slow_down"),
            TokenError::AccessDenied => (StatusCode::BAD_REQUEST, "access_denied"),
            TokenError::ExpiredToken => (StatusCode::BAD_REQUEST, "expired_token"),
            TokenError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
        let mut resp = no_store((status, Json(serde_json::json!({ "error": error }))).into_response());
        if self == (TokenError::InvalidClient { basic: true }) {
            resp.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"accounts\""));
        }
        resp
    }
}

pub(crate) fn no_store(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    resp
}

pub(crate) fn unavailable(what: &str, e: impl std::fmt::Display) -> TokenError {
    tracing::error!(err = %e, "token: {what} failed");
    TokenError::Unavailable
}

/// A failure of the mint core or the rotation: a store failure is 503, every refusal
/// `invalid_grant`.
fn grant_error(what: &str, e: Error) -> TokenError {
    match e.status {
        Status::Internal | Status::Unavailable => unavailable(what, e),
        _ => TokenError::InvalidGrant,
    }
}

/// The parameters of a form body; an empty value counts as absent (RFC 6749 §3.1), and a
/// parameter sent twice is `None` (§3.2).
pub(crate) fn parse_form(headers: &HeaderMap, body: &[u8]) -> Option<HashMap<String, String>> {
    let form_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"));
    if !form_type {
        return None;
    }
    let mut form = HashMap::new();
    for (name, value) in url::form_urlencoded::parse(body) {
        if value.is_empty() {
            continue;
        }
        if form.insert(name.into_owned(), value.into_owned()).is_some() {
            return None;
        }
    }
    Some(form)
}

/// One `application/x-www-form-urlencoded` component: `+` is a space, `%XX` a byte.
fn form_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = char::from(bytes.next()?).to_digit(16)?;
                let lo = char::from(bytes.next()?).to_digit(16)?;
                out.push(u8::try_from(hi * 16 + lo).ok()?);
            }
            _ => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

/// The `client_secret_basic` credentials of `headers`: `None` without a Basic header,
/// `Some(None)` for one that does not decode.
fn basic_credentials(headers: &HeaderMap) -> Option<Option<(String, String)>> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Basic") {
        return None;
    }
    let decoded = STANDARD.decode(encoded.trim()).ok().and_then(|b| String::from_utf8(b).ok());
    Some(decoded.and_then(|pair| {
        let (id, secret) = pair.split_once(':')?;
        Some((form_decode(id)?, form_decode(secret)?))
    }))
}

/// Authenticates the client of a `/token` or `/revoke` request (RFC 6749 §2.3): a
/// confidential client by `client_secret_basic` or `client_secret_post`, a public one by
/// `client_id` alone. Two methods in one request is `invalid_request`; an unknown client, a
/// missing or wrong secret, or a secret sent for a public client is `invalid_client`.
pub(crate) async fn authenticate_client(
    svc: &Service,
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Result<TokenClient, TokenError> {
    let body_id = form.get("client_id");
    let body_secret = form.get("client_secret");
    let (client_id, secret, basic) = match basic_credentials(headers) {
        Some(_) if body_secret.is_some() => return Err(TokenError::InvalidRequest),
        Some(None) => return Err(TokenError::InvalidClient { basic: true }),
        Some(Some((id, _))) if body_id.is_some_and(|b| *b != id) => return Err(TokenError::InvalidRequest),
        Some(Some((id, secret))) => (id, Some(secret), true),
        None => {
            let Some(id) = body_id else { return Err(TokenError::InvalidClient { basic: false }) };
            (id.clone(), body_secret.cloned(), false)
        }
    };
    let refused = TokenError::InvalidClient { basic };
    if !client_id_within_cap(&client_id) {
        return Err(refused);
    }
    let client = match svc.store.token_client(&client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return Err(refused),
        Err(e) => return Err(unavailable("client lookup", e)),
    };
    let authenticated = match (&client.secret_digest, &secret) {
        (Some(stored), Some(secret)) => ct_eq(digest(secret).as_bytes(), stored.as_bytes()),
        (None, None) => true,
        _ => false,
    };
    if authenticated {
        Ok(client)
    } else {
        Err(refused)
    }
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
}

impl TokenResponse {
    fn new(session: accountsapi::Session) -> TokenResponse {
        TokenResponse {
            access_token: session.token,
            token_type: "Bearer",
            expires_in: session.access_expires_in_secs,
            refresh_token: Some(session.refresh_token).filter(|t| !t.is_empty()),
            scope: None,
            id_token: None,
        }
    }
}

/// `POST /accounts/oidc/token`.
pub(crate) async fn token(State(web): State<Arc<Web>>, headers: HeaderMap, body: Bytes) -> Response {
    let answer = match parse_form(&headers, &body) {
        Some(form) => grant(&web.svc, &headers, &form).await,
        None => Err(TokenError::InvalidRequest),
    };
    match answer {
        Ok(ok) => no_store(Json(ok).into_response()),
        Err(error) => error.into_response(),
    }
}

async fn grant(svc: &Service, headers: &HeaderMap, form: &HashMap<String, String>) -> Result<TokenResponse, TokenError> {
    let Some(grant_type) = form.get("grant_type") else { return Err(TokenError::InvalidRequest) };
    let client = authenticate_client(svc, headers, form).await?;
    let listed = |grant: &str| client.grant_types.iter().any(|g| g == grant);
    match grant_type.as_str() {
        AUTHORIZATION_CODE if listed(AUTHORIZATION_CODE) => code_grant(svc, &client, form).await,
        REFRESH_TOKEN if listed(REFRESH_TOKEN) => refresh_grant(svc, &client, form).await,
        DEVICE_CODE if listed(DEVICE_CODE) && client.secret_digest.is_none() => device_grant(svc, &client, form).await,
        AUTHORIZATION_CODE | REFRESH_TOKEN | DEVICE_CODE => Err(TokenError::UnauthorizedClient),
        _ => Err(TokenError::UnsupportedGrantType),
    }
}

async fn refresh_grant(
    svc: &Service,
    client: &TokenClient,
    form: &HashMap<String, String>,
) -> Result<TokenResponse, TokenError> {
    let Some(refresh_token) = form.get("refresh_token") else { return Err(TokenError::InvalidRequest) };
    let session = svc
        .refresh_as(refresh_token.clone(), RefreshPresenter::Client(&client.client_id))
        .await
        .map_err(|e| grant_error("refresh", e))?;
    Ok(TokenResponse::new(session))
}

/// RFC 7636 §4.1: 43 to 128 unreserved characters.
fn well_formed_verifier(verifier: &str) -> bool {
    (MIN_VERIFIER_CHARS..=MAX_VERIFIER_CHARS).contains(&verifier.len())
        && verifier.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Redeems the code in its own committed statement, then checks it against the request and
/// mints; every refusal after the redemption leaves the code consumed. A code found already
/// redeemed revokes what its first redemption minted.
async fn code_grant(
    svc: &Service,
    client: &TokenClient,
    form: &HashMap<String, String>,
) -> Result<TokenResponse, TokenError> {
    let Some(code) = form.get("code") else { return Err(TokenError::InvalidRequest) };
    let redeemed = match svc.store.redeem_oidc_code(&svc.store.pool, code).await {
        Ok(Some(redeemed)) => redeemed,
        Ok(None) => return Err(refuse_unredeemable(svc, code).await),
        Err(e) => return Err(unavailable("code redemption", e)),
    };
    if redeemed.client_id != client.client_id
        || form.get("redirect_uri").is_none_or(|uri| *uri != redeemed.redirect_uri)
    {
        return Err(TokenError::InvalidGrant);
    }
    let Some(verifier) = form.get("code_verifier").filter(|v| well_formed_verifier(v)) else {
        return Err(TokenError::InvalidRequest);
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if !ct_eq(challenge.as_bytes(), redeemed.code_challenge.as_bytes()) {
        return Err(TokenError::InvalidGrant);
    }
    let (session, id_token) = mint_code(svc, &redeemed).await.map_err(|e| grant_error("code mint", e))?;
    let mut answer = TokenResponse::new(session);
    answer.scope = Some(redeemed.scope).filter(|s| !s.is_empty());
    answer.id_token = id_token;
    Ok(answer)
}

/// Polls a device code (D6): the store answers every check before the redemption, revoking a
/// replay's first family itself; an approved code is redeemed in its own committed statement
/// and minted in a separate transaction, as an authorization code is.
async fn device_grant(
    svc: &Service,
    client: &TokenClient,
    form: &HashMap<String, String>,
) -> Result<TokenResponse, TokenError> {
    let Some(device_code) = form.get("device_code") else { return Err(TokenError::InvalidRequest) };
    let poll = svc
        .store
        .poll_device_code(device_code, &client.client_id)
        .await
        .map_err(|e| unavailable("device poll", e))?;
    let redeemed = match poll {
        DevicePoll::Redeemed(redeemed) => redeemed,
        DevicePoll::Unknown => return Err(TokenError::InvalidGrant),
        DevicePoll::Replayed { family_id } => {
            tracing::warn!(%family_id, "device code replayed — revoked the tokens of its first redemption");
            return Err(TokenError::InvalidGrant);
        }
        DevicePoll::Expired => return Err(TokenError::ExpiredToken),
        DevicePoll::SlowDown => return Err(TokenError::SlowDown),
        DevicePoll::Pending => return Err(TokenError::AuthorizationPending),
        DevicePoll::Denied => return Err(TokenError::AccessDenied),
    };
    let (session, id_token) = mint_code(svc, &redeemed).await.map_err(|e| grant_error("device mint", e))?;
    let mut answer = TokenResponse::new(session);
    answer.scope = Some(redeemed.scope).filter(|s| !s.is_empty());
    answer.id_token = id_token;
    Ok(answer)
}

/// The answer to a code the redemption did not take: `invalid_grant`, after revoking the
/// first redemption's tokens when the code is a replay (RFC 6749 §4.1.2).
async fn refuse_unredeemable(svc: &Service, code: &str) -> TokenError {
    let family_id = match svc.store.replayed_code_family(&svc.store.pool, code).await {
        Ok(Some(family_id)) => family_id,
        Ok(None) => return TokenError::InvalidGrant,
        Err(e) => return unavailable("replay lookup", e),
    };
    tracing::warn!(%family_id, "authorization code replayed — revoking the tokens of its first redemption");
    let revoked = async {
        let mut tx = svc.store.pool.begin().await?;
        svc.store.revoke_replayed_family_tx(&mut tx, &family_id).await?;
        tx.commit().await
    };
    match revoked.await {
        Ok(()) => TokenError::InvalidGrant,
        Err(e) => unavailable("replay revocation", e),
    }
}

/// Mints the tokens of a redeemed authorization code or device code in one transaction, under
/// the family id its redemption chose, with an id_token when the granted scope has `openid`.
pub(crate) async fn mint_code<'a>(
    svc: &Service,
    grant: impl Into<RedeemedGrant<'a>>,
) -> Result<(accountsapi::Session, Option<String>), Error> {
    let grant = grant.into();
    let mut tx = svc.store.pool.begin().await.map_err(internal)?;
    match mint_code_tx(svc, &mut tx, &grant).await {
        Ok(minted) => {
            tx.commit().await.map_err(internal)?;
            Ok(minted)
        }
        Err(e) => {
            tx.rollback().await.ok();
            Err(e)
        }
    }
}

/// Lock order: the code family's advisory lock (which a replay's revocation takes first
/// too), then the client row and the player row FOR KEY SHARE, the order of every login.
/// The client must list the grant's name. After the player's key share it re-reads the
/// grant's row in its own table, which a reset or Revoke all that held the player first has
/// deleted, so no grant redeemed before one mints after it.
async fn mint_code_tx(
    svc: &Service,
    conn: &mut PgConnection,
    code: &RedeemedGrant<'_>,
) -> Result<(accountsapi::Session, Option<String>), Error> {
    let invalid_grant = || Error::unauthorized("invalid grant");
    svc.store.lock_code_family_tx(conn, code.family_id).await.map_err(internal)?;
    if svc.store.family_revoked_tx(conn, code.family_id).await.map_err(internal)? {
        return Err(invalid_grant());
    }
    let Some(grants) = svc
        .store
        .client_allows_tx(conn, code.client_id, Some(code.grant))
        .await
        .map_err(internal)?
    else {
        return Err(invalid_grant());
    };
    if !svc.store.key_share_player_tx(conn, code.player_id).await.map_err(internal)? {
        return Err(invalid_grant());
    }
    if !svc.store.redeemed_grant_kept_tx(conn, code).await.map_err(internal)? {
        return Err(invalid_grant());
    }
    let (session, minted) = svc
        .mint_session_tx(conn, code.player_id, code.client_id, grants, Some(code.family_id))
        .await?;
    if !code.scope.split(' ').any(|s| s == OPENID) {
        return Ok((session, None));
    }
    let claims = svc.tokens.id_claims(
        code.player_id,
        code.client_id,
        minted.iat,
        code.auth_time,
        code.nonce,
        &minted.handle,
    );
    let id_token = svc.tokens.sign_id(conn, &claims).await.map_err(|e| internal(format!("{e:#}")))?;
    Ok((session, Some(id_token)))
}

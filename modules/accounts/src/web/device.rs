//! `POST /accounts/oidc/device_authorization`: the device authorization endpoint (RFC 8628
//! §3.1-3.2). A public client that lists the `device_code` grant gets a code pair; the device
//! shows the `user_code` and polls `/token` with the `device_code` (`token.rs`).
//!
//! `GET /accounts/oidc/device` and `POST .../device/approve`, `.../device/deny`: the verification
//! pages, where a signed-in browser enters a `user_code` and approves or denies its pending code.
//! Every lookup and decision draws from the sign-in limiter, and every code that is not pending
//! and unexpired gets the same error.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use accountsapi::grant_types::{DEVICE_CODE, REFRESH_TOKEN};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};

use super::authorize::encode;
use super::session::BrowserSession;
use super::token::{no_store, parse_form, unavailable, TokenError};
use super::{held_session_digest, see_other, session_lookup_failed, Web, LOGIN};
use crate::clients::{canonical_scopes, MAX_DEVICE_CODE_TTL_SECS};
use crate::client_id_within_cap;
use crate::device_codes::{format_user_code, normalize_user_code, NewDeviceCode, POLL_INTERVAL_SECS};
use crate::secret::ct_eq;
use crate::store::REFRESH_TTL_DAYS;

pub(crate) const DEVICE_AUTHORIZATION: &str = "/accounts/oidc/device_authorization";
pub(crate) const MAX_DEVICE_BODY_BYTES: usize = 8 * 1024;
pub(crate) const DEVICE: &str = "/accounts/oidc/device";
pub(crate) const DEVICE_APPROVE: &str = "/accounts/oidc/device/approve";
pub(crate) const DEVICE_DENY: &str = "/accounts/oidc/device/deny";
pub(crate) const INVALID_CODE_ERROR: &str =
    "That code is not valid or has expired. Check the code on your device, or start the sign-in there again.";
pub(crate) const EXPIRED_PAGE_ERROR: &str = "This page expired. Enter the code again.";
const BUSY_ERROR: &str = "Too many tries. Please wait a moment and try again.";
const UNAVAILABLE_ERROR: &str = "Connecting a device is unavailable. Please try again in a moment.";

#[derive(Serialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: i32,
}

/// `POST /accounts/oidc/device_authorization`.
pub(crate) async fn device_authorization(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let answer = match parse_form(&headers, &body) {
        Some(form) => issue(&web, peer, &headers, &form).await,
        None => Err(TokenError::InvalidRequest),
    };
    match answer {
        Ok(ok) => no_store(Json(ok).into_response()),
        Err(error) => error.into_response(),
    }
}

/// An unknown client is `invalid_client`; a confidential one, or one without the device grant,
/// `unauthorized_client`; a scope the client does not list, `invalid_scope`. The verification
/// URIs are built from the issuer, never from the request's `Host`, and the requesting IP is
/// read through the trusted proxies.
async fn issue(
    web: &Web,
    peer: SocketAddr,
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Result<DeviceAuthorization, TokenError> {
    let refused = TokenError::InvalidClient { basic: false };
    let Some(client_id) = form.get("client_id").filter(|id| client_id_within_cap(id)) else { return Err(refused) };
    let store = &web.svc.store;
    let client = match store.client(client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return Err(refused),
        Err(e) => return Err(unavailable("client lookup", e)),
    };
    if client.confidential || !client.grant_types.iter().any(|g| g == DEVICE_CODE) {
        return Err(TokenError::UnauthorizedClient);
    }
    let scope = form.get("scope").map(String::as_str).unwrap_or_default();
    let requested: Vec<String> = scope.split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect();
    if requested.iter().any(|s| !client.scopes.contains(s)) {
        return Err(TokenError::InvalidScope);
    }
    if let Err(e) = store.prune_device_codes(&store.pool).await {
        tracing::warn!(err = %e, "device authorization: code prune failed");
    }
    let scope = canonical_scopes(&requested).join(" ");
    let new = NewDeviceCode {
        client_id,
        scope: &scope,
        requested_ip: web.limiter.client_ip(peer, headers),
        user_agent: headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()),
    };
    let issued = match store.insert_device_code(&new).await {
        Ok(Some(issued)) => issued,
        Ok(None) => return Err(refused),
        Err(e) => return Err(unavailable("device code insert", e)),
    };
    let verification_uri = format!("{}/device", web.svc.tokens.issuer());
    Ok(DeviceAuthorization {
        verification_uri_complete: format!("{verification_uri}?user_code={}", issued.user_code),
        verification_uri,
        device_code: issued.device_code,
        user_code: issued.user_code,
        expires_in: issued.expires_in,
        interval: POLL_INTERVAL_SECS,
    })
}

#[derive(Serialize)]
struct EnterView<'a> {
    error: &'a str,
    ttl: String,
}

#[derive(Serialize)]
struct ConfirmView<'a> {
    client: &'a str,
    device: Option<&'a str>,
    user: &'a str,
    code: &'a str,
    ip: &'a str,
    age: String,
    lasts: String,
    csrf: &'a str,
}

#[derive(Serialize)]
struct ResultView<'a> {
    approved: bool,
    client: &'a str,
    device: Option<&'a str>,
    user: &'a str,
    code: &'a str,
}

/// A pending, unexpired code and what the screens show of it.
struct Found {
    /// Normalised, as the store looks it up.
    code: String,
    client: String,
    refresh: bool,
    device: Option<String>,
    ip: String,
    age_secs: i64,
    user: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct DecisionForm {
    user_code: String,
    csrf: String,
}

/// The code a request typed: its `user_code` (the `verification_uri_complete` link), else the
/// eight boxes `c1`..`c8` of the enter-code form joined in order.
fn typed_code(query: &str) -> String {
    let mut user_code = String::new();
    let mut boxes: [String; 8] = Default::default();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if name == "user_code" {
            user_code = value.into_owned();
        } else if let Some(i) = name.strip_prefix('c').and_then(|n| n.parse::<usize>().ok()) {
            if let Some(slot) = i.checked_sub(1).and_then(|i| boxes.get_mut(i)) {
                *slot = value.into_owned();
            }
        }
    }
    if user_code.is_empty() { boxes.concat() } else { user_code }
}

/// The hosted login, returning to the enter-code page with the typed code kept when it is one.
fn to_login(typed: &str) -> Response {
    let return_to = match normalize_user_code(typed) {
        Some(code) => format!("{DEVICE}?user_code={}", format_user_code(&code)),
        None => DEVICE.to_string(),
    };
    see_other(&format!("{LOGIN}?return_to={}", encode(&return_to)))
}

fn enter(web: &Web, status: StatusCode, error: &str) -> Response {
    let ttl = format!("{} min", MAX_DEVICE_CODE_TTL_SECS / 60);
    web.render("device_enter.html", status, EnterView { error, ttl })
}

fn busy(web: &Web) -> Response {
    let mut resp = enter(web, StatusCode::TOO_MANY_REQUESTS, BUSY_ERROR);
    resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    resp
}

fn invalid(web: &Web) -> Response {
    enter(web, StatusCode::BAD_REQUEST, INVALID_CODE_ERROR)
}

/// Why a lookup has no screen to show: the player is gone, or the store failed (logged).
enum Refused {
    SignedOut,
    Unavailable,
}

fn store_failed(what: &str, e: sqlx::Error) -> Refused {
    tracing::error!(err = %e, "device approval: {what} failed");
    Refused::Unavailable
}

fn refused(web: &Web, why: Refused) -> Response {
    match why {
        Refused::SignedOut => see_other(LOGIN),
        Refused::Unavailable => enter(web, StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_ERROR),
    }
}

/// "Session lasts": a refresh family's lifetime when the client can refresh, else one access token's.
fn session_lasts(refresh: bool) -> String {
    if refresh {
        format!("{REFRESH_TTL_DAYS} d")
    } else {
        format!("{} min", accountsapi::ACCESS_TTL_MINUTES)
    }
}

/// The pending, unexpired code `typed` names with its client and the signed-in player; `Ok(None)`
/// for any other code.
async fn found(web: &Web, session: &BrowserSession, typed: &str) -> Result<Option<Found>, Refused> {
    let Some(code) = normalize_user_code(typed) else { return Ok(None) };
    let store = &web.svc.store;
    let pending = match store.pending_device_code(&code).await {
        Ok(Some(pending)) => pending,
        Ok(None) => return Ok(None),
        Err(e) => return Err(store_failed("code lookup", e)),
    };
    let client = match store.client(&pending.client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return Ok(None),
        Err(e) => return Err(store_failed("client lookup", e)),
    };
    let user = match store.player_summary(&session.player_id).await {
        Ok(Some(player)) => player.display_name,
        Ok(None) => return Err(Refused::SignedOut),
        Err(e) => return Err(store_failed("player lookup", e)),
    };
    Ok(Some(Found {
        code,
        client: client.display_name,
        refresh: client.grant_types.iter().any(|g| g == REFRESH_TOKEN),
        device: pending.user_agent,
        ip: pending.requested_ip,
        age_secs: pending.age_secs,
        user,
    }))
}

/// `GET /accounts/oidc/device[?user_code=]`: a signed-out browser goes to the login and comes
/// back; no code is the enter-code form; a typed code, looked up under the limiter, is the
/// confirm screen when it is pending and unexpired.
pub(crate) async fn device_page(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    RawQuery(query): RawQuery,
) -> Response {
    let typed = typed_code(query.as_deref().unwrap_or_default());
    let session = match web.signed_in(&jar).await {
        Ok(Some(session)) => session,
        Ok(None) => return to_login(&typed),
        Err(e) => return session_lookup_failed(e),
    };
    if typed.is_empty() {
        return enter(&web, StatusCode::OK, "");
    }
    if !web.limiter.allow(peer, &headers) {
        return busy(&web);
    }
    let found = match found(&web, &session, &typed).await {
        Ok(Some(found)) => found,
        Ok(None) => return invalid(&web),
        Err(why) => return refused(&web, why),
    };
    let code = format_user_code(&found.code);
    let view = ConfirmView {
        client: &found.client,
        device: found.device.as_deref(),
        user: &found.user,
        code: &code,
        ip: &found.ip,
        age: controlplane::ago(found.age_secs),
        lasts: session_lasts(found.refresh),
        csrf: &session.csrf_token,
    };
    web.render("device_confirm.html", StatusCode::OK, view)
}

/// `POST /accounts/oidc/device/approve`.
pub(crate) async fn approve(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<DecisionForm>,
) -> Response {
    decide(&web, peer, &headers, &jar, &form, true).await
}

/// `POST /accounts/oidc/device/deny`.
pub(crate) async fn deny(
    State(web): State<Arc<Web>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<DecisionForm>,
) -> Response {
    decide(&web, peer, &headers, &jar, &form, false).await
}

/// The limiter, the session and its CSRF token, then the guarded update: only a pending,
/// unexpired code moves, and any other answers the one invalid-code error.
async fn decide(
    web: &Web,
    peer: SocketAddr,
    headers: &HeaderMap,
    jar: &CookieJar,
    form: &DecisionForm,
    allow: bool,
) -> Response {
    if !web.limiter.allow(peer, headers) {
        return busy(web);
    }
    let session = match web.signed_in(jar).await {
        Ok(Some(session)) => session,
        Ok(None) => return to_login(&form.user_code),
        Err(e) => return session_lookup_failed(e),
    };
    if !ct_eq(form.csrf.as_bytes(), session.csrf_token.as_bytes()) {
        return enter(web, StatusCode::FORBIDDEN, EXPIRED_PAGE_ERROR);
    }
    let found = match found(web, &session, &form.user_code).await {
        Ok(Some(found)) => found,
        Ok(None) => return invalid(web),
        Err(why) => return refused(web, why),
    };
    let store = &web.svc.store;
    let moved = if allow {
        let Some(digest) = held_session_digest(jar) else { return to_login(&form.user_code) };
        store.approve_device_code(&found.code, &digest).await
    } else {
        store.deny_device_code(&found.code).await
    };
    match moved {
        Ok(true) => {}
        Ok(false) => return invalid(web),
        Err(e) => return refused(web, store_failed("decision", e)),
    }
    let code = format_user_code(&found.code);
    let view = ResultView {
        approved: allow,
        client: &found.client,
        device: found.device.as_deref(),
        user: &found.user,
        code: &code,
    };
    web.render("device_result.html", StatusCode::OK, view)
}

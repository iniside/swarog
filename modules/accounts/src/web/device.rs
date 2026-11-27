//! `POST /accounts/oidc/device_authorization`: the device authorization endpoint (RFC 8628
//! §3.1-3.2). A public client that lists the `device_code` grant gets a code pair; the device
//! shows the `user_code` and polls `/token` with the `device_code` (`token.rs`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use accountsapi::grant_types::DEVICE_CODE;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::token::{no_store, parse_form, unavailable, TokenError};
use super::Web;
use crate::clients::canonical_scopes;
use crate::client_id_within_cap;
use crate::device_codes::{NewDeviceCode, POLL_INTERVAL_SECS};

pub(crate) const DEVICE_AUTHORIZATION: &str = "/accounts/oidc/device_authorization";
pub(crate) const MAX_DEVICE_BODY_BYTES: usize = 8 * 1024;

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

//! `POST /accounts/oidc/revoke` (RFC 7009): a client revokes a refresh token or an access
//! token of its own, which takes the token family they belong to, by the path and the locks
//! of the other family revocations (`Store::kill_family_tx` under the player row lock, as the
//! admin's revoke-session). A token the server does not know, or another client's, is a 200
//! that revokes nothing: the answer never says which.

use std::collections::HashMap;
use std::sync::Arc;

use accountsapi::grant_types::REFRESH_TOKEN;
use accountsapi::MAX_ACCESS_TOKEN_BYTES;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::token::{authenticate_client, no_store, parse_form, unavailable, TokenError};
use super::Web;
use crate::clients::TokenClient;
use crate::Service;

pub(crate) const REVOKE: &str = "/accounts/oidc/revoke";
pub(crate) const MAX_REVOKE_BODY_BYTES: usize = 8 * 1024;

/// The family a presented token belongs to, and the player it is the login of.
struct Target {
    player_id: String,
    family_id: String,
}

/// `POST /accounts/oidc/revoke`.
pub(crate) async fn revoke(State(web): State<Arc<Web>>, headers: HeaderMap, body: Bytes) -> Response {
    let answer = match parse_form(&headers, &body) {
        Some(form) => revoke_token(&web.svc, &headers, &form).await,
        None => Err(TokenError::InvalidRequest),
    };
    match answer {
        Ok(()) => no_store(StatusCode::OK.into_response()),
        Err(error) => error.into_response(),
    }
}

async fn revoke_token(svc: &Service, headers: &HeaderMap, form: &HashMap<String, String>) -> Result<(), TokenError> {
    let client = authenticate_client(svc, headers, form).await?;
    let Some(token) = form.get("token") else { return Err(TokenError::InvalidRequest) };
    // `token_type_hint` is only a hint (RFC 7009 section 2.1), and both kinds are tried.
    match locate(svc, &client, token).await? {
        Some(target) => revoke_family(svc, &client, &target).await,
        None => Ok(()),
    }
}

/// The family of `token` when it is a refresh token, or an access token signed by this
/// issuer for the client's audience and minted for the client; `None` for anything else.
async fn locate(svc: &Service, client: &TokenClient, token: &str) -> Result<Option<Target>, TokenError> {
    if token.len() > MAX_ACCESS_TOKEN_BYTES {
        return Ok(None);
    }
    let mut conn = svc.store.pool.acquire().await.map_err(|e| unavailable("revoke connection", e))?;
    let row = svc.store.refresh_row_tx(&mut conn, token).await.map_err(|e| unavailable("refresh token lookup", e))?;
    drop(conn);
    if let Some(row) = row {
        return Ok(Some(Target { player_id: row.player_id, family_id: row.family_id }));
    }
    let claims = svc
        .tokens
        .verify_signed(token, &client.audience)
        .await
        .map_err(|e| unavailable("access token verify", e))?;
    Ok(claims
        .filter(|c| c.client_id == client.client_id)
        .map(|c| Target { player_id: c.sub, family_id: c.sid }))
}

/// Kills the family when it is the client's: the player row first (a refresh takes it before
/// its token, so one rotating inside the family either committed before and goes with it, or
/// waits and finds its token gone), then the family's owner, then the family. A vanished
/// player or family, or another client's, revokes nothing.
async fn revoke_family(svc: &Service, client: &TokenClient, target: &Target) -> Result<(), TokenError> {
    let store = &svc.store;
    let mut tx = store.pool.begin().await.map_err(|e| unavailable("revoke begin", e))?;
    let player = store
        .lock_player_row_tx(&mut tx, &target.player_id)
        .await
        .map_err(|e| unavailable("revoke player lock", e))?;
    if !player {
        return Ok(());
    }
    let owner = store
        .family_client_tx(&mut tx, &target.family_id, REFRESH_TOKEN)
        .await
        .map_err(|e| unavailable("revoke family owner", e))?;
    if !owner.is_some_and(|o| o.client_id == client.client_id) {
        return Ok(());
    }
    store
        .kill_family_tx(&mut tx, &target.family_id)
        .await
        .map_err(|e| unavailable("revoke family", e))?;
    tx.commit().await.map_err(|e| unavailable("revoke commit", e))?;
    Ok(())
}

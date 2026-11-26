//! The OIDC issuer's discovery document and JWKS, on the issuer path `/accounts/oidc`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::signing_keys::SigningKeys;

pub(crate) const DISCOVERY: &str = "/accounts/oidc/.well-known/openid-configuration";
pub(crate) const JWKS: &str = "/accounts/oidc/jwks";

struct Issuer {
    discovery: serde_json::Value,
    keys: Arc<SigningKeys>,
}

/// Both routes; `issuer` is the parsed `ACCOUNTS_OIDC_ISSUER`, whose path is always
/// `/accounts/oidc`, so its `/jwks` is [`JWKS`].
pub(crate) fn router(issuer: &str, keys: Arc<SigningKeys>) -> Router {
    let discovery = serde_json::json!({
        "issuer": issuer,
        "jwks_uri": format!("{issuer}/jwks"),
    });
    Router::new()
        .route(DISCOVERY, get(discovery_document))
        .route(JWKS, get(jwks))
        .with_state(Arc::new(Issuer { discovery, keys }))
}

async fn discovery_document(State(issuer): State<Arc<Issuer>>) -> Response {
    Json(&issuer.discovery).into_response()
}

async fn jwks(State(issuer): State<Arc<Issuer>>) -> Response {
    match issuer.keys.jwks().await {
        Ok(set) => Json(&*set).into_response(),
        Err(e) => {
            tracing::error!(err = %format!("{e:#}"), "accounts: read the signing keys for the JWKS");
            (StatusCode::SERVICE_UNAVAILABLE, "signing keys unavailable").into_response()
        }
    }
}

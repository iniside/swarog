//! The OIDC issuer's discovery document and JWKS, on the issuer path `/accounts/oidc`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use accountsapi::grant_types::{AUTHORIZATION_CODE, DEVICE_CODE, REFRESH_TOKEN};
use accountsapi::scopes;

use crate::signing_keys::SigningKeys;
pub(crate) const DISCOVERY: &str = "/accounts/oidc/.well-known/openid-configuration";
pub(crate) const JWKS: &str = "/accounts/oidc/jwks";

struct Issuer {
    discovery: serde_json::Value,
    keys: Arc<SigningKeys>,
}

/// The client authentication methods of `/token` and `/revoke` (RFC 8414 section 2): a public
/// client sends no secret.
const CLIENT_AUTH_METHODS: [&str; 3] = ["none", "client_secret_basic", "client_secret_post"];

/// Both routes; `issuer` is the parsed `ACCOUNTS_OIDC_ISSUER`, whose path is always
/// `/accounts/oidc`, so its `/jwks`, `/authorize`, `/token` and `/revoke` are [`JWKS`] and the
/// hosted endpoints of `web`. `grant_types_supported` lists the `/token` grants only, the device grant included.
pub(crate) fn router(issuer: &str, keys: Arc<SigningKeys>) -> Router {
    let discovery = serde_json::json!({
        "issuer": issuer,
        "jwks_uri": format!("{issuer}/jwks"),
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "revocation_endpoint": format!("{issuer}/revoke"),
        "device_authorization_endpoint": format!("{issuer}/device_authorization"),
        "response_types_supported": ["code"],
        "grant_types_supported": [AUTHORIZATION_CODE, REFRESH_TOKEN, DEVICE_CODE],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": CLIENT_AUTH_METHODS,
        "revocation_endpoint_auth_methods_supported": CLIENT_AUTH_METHODS,
        "id_token_signing_alg_values_supported": ["ES256"],
        "subject_types_supported": ["public"],
        "scopes_supported": scopes::ALL,
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

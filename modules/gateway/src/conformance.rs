//! Minimal factual outage probes consumed by `tools/conformance`.
//!
//! Expected classifications live in the tool. The fakes here only force the
//! real gateway adapters down their dependency-failure paths.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;

use crate::verifier::{SessionVerifier, VerifyUnavailable};
use crate::{KeyVerifier as _, LookupUnavailable, RealKeyVerifier};

pub(crate) struct UnavailableVerifier;

#[async_trait]
impl SessionVerifier for UnavailableVerifier {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        Err(VerifyUnavailable)
    }
}

struct UnavailableKeys;

#[async_trait]
impl apikeysapi::Keys for UnavailableKeys {
    async fn lookup_key(
        &self,
        _key: String,
    ) -> Result<Option<apikeysapi::KeyRecord>, opsapi::Error> {
        Err(opsapi::Error::unavailable("conformance: apikeys dependency down"))
    }
}

#[doc(hidden)]
pub async fn conformance_key_outage(
) -> Result<Option<apikeysapi::KeyRecord>, LookupUnavailable> {
    RealKeyVerifier::new(Arc::new(UnavailableKeys))
        .lookup("conformance-probe-key")
        .await
}

/// Drives the front door's ONE bearer admission (`crate::verify_bearer`, the same
/// function `admit_inner` and `/push` call) over a verifier that cannot answer, and
/// reports the HTTP status the denial renders as.
#[doc(hidden)]
pub async fn conformance_session_outage_status() -> StatusCode {
    match crate::verify_bearer(
        &UnavailableVerifier,
        Some("conformance-probe-token"),
        opsapi::AuthReq::Player,
    )
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(denial) => crate::admission_denial_response(&denial).status(),
    }
}

//! Minimal factual outage probes consumed by `tools/conformance`.
//!
//! Expected classifications live in the tool. The fakes here only force the
//! real gateway adapters down their dependency-failure paths.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};

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

#[doc(hidden)]
pub async fn conformance_session_outage_status() -> StatusCode {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer conformance-probe-token"),
    );
    match crate::authenticate(&headers, &UnavailableVerifier).await {
        Ok(_) => StatusCode::OK,
        Err(response) => response.status(),
    }
}

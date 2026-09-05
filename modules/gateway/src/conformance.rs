//! Minimal factual outage probes consumed by `tools/conformance`.
//!
//! Expected classifications live in the tool. The fakes here only force the
//! real gateway adapters down their dependency-failure paths.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;

use crate::verifier::{SessionVerifier, SessionsVerifier, VerifyUnavailable};
use crate::{KeyVerifier as _, LookupUnavailable, RealKeyVerifier};

/// The group-name bound this front owns, re-exported so the conformance gate's CapCase
/// names the const rather than restating its number.
pub use crate::push_ws::MAX_GROUP_NAME_BYTES;

pub(crate) struct UnavailableVerifier;

#[async_trait]
impl SessionVerifier for UnavailableVerifier {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        Err(VerifyUnavailable)
    }
}

struct UnavailableSessions;

#[async_trait]
impl accountsapi::Sessions for UnavailableSessions {
    async fn verify_session(&self, _token: String) -> Result<Option<String>, opsapi::Error> {
        Err(opsapi::Error::unavailable("conformance: accounts dependency down"))
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

/// Drives the real `/push` group-verb name cap (`MAX_GROUP_NAME_BYTES`) over a hub with
/// one accepted connection: `true` when a name of `len` bytes is refused.
#[doc(hidden)]
pub fn conformance_group_name_rejected(len: usize) -> bool {
    crate::push_ws::PushHub::conformance_group_name_rejected(len)
}

/// A current-thread runtime for the byte-cap probes, which the gate calls SYNCHRONOUSLY
/// (`Fixture::InputByteCaps`) while the async paths they drive are the production ones.
fn probe_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

/// Whether the front REFUSES a bearer of `len` bytes as a credential verdict — the
/// gateway's OWN cap (`SessionsVerifier::verify`), ahead of the accounts capability.
///
/// The unavailable `Sessions` fake is what makes the cap executable, because a 401 has a
/// second producer: an at-cap token is simply unknown, and an unknown token is a 401 too.
/// Against a dependency that can only fail, the 401 is reachable ONLY before the call —
/// at the cap this reaches the fake and renders 503 (false, as required), over the cap
/// the guard answers `Ok(None)` and renders 401 (true). Delete the guard and the over-cap
/// case 503s as well, turning this case red.
///
/// It drives `crate::verify_bearer` — the one bearer authority `/push` and `admit_inner`
/// share — not the verifier in isolation.
#[doc(hidden)]
pub fn conformance_session_token_rejected(len: usize) -> bool {
    let rt = probe_runtime();
    let verifier = {
        let _guard = rt.enter();
        SessionsVerifier::new(Arc::new(UnavailableSessions))
    };
    let outcome = rt.block_on(crate::verify_bearer(
        &verifier,
        Some(&"a".repeat(len)),
        opsapi::AuthReq::Player,
    ));
    match outcome {
        Err(denial) => {
            crate::admission_denial_response(&denial).status() == StatusCode::UNAUTHORIZED
        }
        Ok(_) => false,
    }
}

/// Whether the front REFUSES a presented api key of `len` bytes as a credential verdict —
/// the gateway's OWN cap (`RealKeyVerifier::lookup`), ahead of the apikeys capability and
/// its cache.
///
/// Same construction as the bearer case above and for the same reason: `Ok(None)` is also
/// what an unknown key produces. The unavailable `Keys` fake removes that second producer,
/// so at the cap the lookup reaches the fake and sheds (`Err`, false) while over the cap
/// the guard answers `Ok(None)` (true). `apikeys`'s own `MAX_KEY_BYTES` case restates the
/// comparison arithmetically; this one executes the enforcement point.
#[doc(hidden)]
pub fn conformance_api_key_rejected(len: usize) -> bool {
    let rt = probe_runtime();
    let verifier = {
        let _guard = rt.enter();
        RealKeyVerifier::new(Arc::new(UnavailableKeys))
    };
    matches!(rt.block_on(verifier.lookup(&"a".repeat(len))), Ok(None))
}

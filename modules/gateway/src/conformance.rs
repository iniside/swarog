//! This module's convention-conformance entry — the explicit stance `gateway`
//! declares for every [`conformance::Convention`], executed by the
//! `conformancecheck` harness (`tools/conformance`). Always compiled (the
//! `asyncevents::testing` precedent — no feature flags): the always-failing fakes
//! below live here rather than in `tests.rs` so the harness can construct the REAL
//! verifiers around them; the in-crate tests re-import them from here. Nothing in
//! this module runs on a production path.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use conformance::{Convention, Entry, Fixture, OutageCase, OutageClass, Stance};

use crate::keys::RealKeyVerifier;
use crate::verifier::{SessionVerifier, VerifyUnavailable};
use crate::KeyVerifier as _;

/// A [`SessionVerifier`] that always reports its dependency unreachable — the
/// gateway stand-in for an accounts-svc outage. Distinct from `Ok(None)` (a
/// definitively invalid token), it must surface as 503 / `Status::Unavailable`.
/// Moved here from `tests.rs` so the conformance probe and the unit tests share
/// one fixture.
pub struct UnavailableVerifier;

#[async_trait]
impl SessionVerifier for UnavailableVerifier {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        Err(VerifyUnavailable)
    }
}

/// An `apikeysapi::Keys` whose every lookup fails — the stand-in for an
/// apikeys-svc/store outage behind the REAL [`RealKeyVerifier`].
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

/// The `gateway` conformance entry. Stances per the decided matrix
/// (`docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`).
pub fn entry() -> Entry {
    Entry {
        module: "gateway",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "the gateway module reads no parsed env values: peer addresses \
                          and passthrough origins are injected by the cmd/* roots, and \
                          ACCOUNTS_DEV_AUTH / APIKEYS_DEV_ALLOW are boolean gates that \
                          fail closed by absence",
                },
            ),
            (
                Convention::InputByteCaps,
                Stance::NotApplicable {
                    why: "the gateway parses no player input fields of its own — its \
                          MAX_BODY_BYTES cap and the rate limits are transport guards \
                          (core/httpmw class), and field-level byte caps belong to the \
                          modules owning the operations",
                },
            ),
            (
                Convention::InfraOutage503,
                Stance::Applies(Fixture::InfraOutage503(vec![
                    OutageCase {
                        name: "gateway RealKeyVerifier over a failing apikeys capability",
                        probe: Arc::new(|| {
                            Box::pin(async {
                                let verifier = RealKeyVerifier::new(Arc::new(UnavailableKeys));
                                match verifier.lookup("conformance-probe-key").await {
                                    Err(crate::LookupUnavailable) => OutageClass::Unavailable,
                                    Ok(None) => OutageClass::Rejected,
                                    Ok(Some(_)) => OutageClass::Other(
                                        "lookup returned a record from a down dependency"
                                            .into(),
                                    ),
                                }
                            })
                        }),
                    },
                    OutageCase {
                        name: "gateway authenticate over a failing session verifier",
                        probe: Arc::new(|| {
                            Box::pin(async {
                                let mut headers = HeaderMap::new();
                                headers.insert(
                                    header::AUTHORIZATION,
                                    HeaderValue::from_static("Bearer conformance-probe-token"),
                                );
                                match crate::authenticate(&headers, &UnavailableVerifier).await {
                                    Err(resp)
                                        if resp.status() == StatusCode::SERVICE_UNAVAILABLE =>
                                    {
                                        OutageClass::Unavailable
                                    }
                                    Err(resp) if resp.status() == StatusCode::UNAUTHORIZED => {
                                        OutageClass::Rejected
                                    }
                                    Err(resp) => OutageClass::Other(format!(
                                        "unexpected status {}",
                                        resp.status()
                                    )),
                                    Ok(_) => OutageClass::Other(
                                        "authenticated against a down verifier".into(),
                                    ),
                                }
                            })
                        }),
                    },
                ])),
            ),
            (
                Convention::ArgonParity,
                Stance::NotApplicable {
                    why: "the gateway performs no password hashing — bearer and API-key \
                          verification are delegated to the accounts/apikeys capabilities",
                },
            ),
        ],
    }
}

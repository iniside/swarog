//! The OIDC id_token verifier (port of Go's `modules/accounts/epic.go`). It is
//! configured, not hardcoded: Epic is the first user, Google (also OIDC) is the
//! known next one. The backend is a trusted VERIFIER — it never holds the user's
//! credentials, only checks the IdP's signed token (the EOS Connect model).
//!
//! Divergence from Go's shape (not semantics): Go's `keyfunc.NewDefault` fetched the
//! JWKS eagerly inside `Init`; Rust `init` must do no I/O (constraint #8), so the
//! JWKS is fetched LAZILY on first verify and cached, with one refetch when a
//! token's `kid` is absent from the cached set (the keyfunc refresh behaviour).

use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

/// The signature algorithms accepted — excludes `none` and every HMAC variant by
/// construction (Go's `jwt.WithValidMethods({"RS256","ES256"})`).
const ALLOWED_ALGS: [Algorithm; 2] = [Algorithm::RS256, Algorithm::ES256];

/// Cooldown between JWKS fetch ATTEMPTS: a flood of bogus-`kid` tokens (an
/// unauthenticated caller controls the header) costs the IdP at most one fetch per
/// interval instead of one per request.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Why a token failed verification — the taxonomy the caller maps to a status
/// (mirrors the `verify_session` 503-not-401 precedent: an IdP outage must not
/// masquerade as bad credentials).
#[derive(Debug, thiserror::Error)]
pub(crate) enum VerifyError {
    /// The token itself is demonstrably invalid: bad signature/alg/aud/iss/exp, or
    /// a `kid` absent from a fresh (or fresh-enough, see the cooldown) key set.
    /// Maps to Unauthorized (401).
    #[error("token rejected: {0}")]
    Rejected(#[source] anyhow::Error),
    /// No verdict was reachable: the JWKS fetch failed (network/HTTP status) and no
    /// cached key answers. Maps to Unavailable (503) — the caller's credentials may
    /// be perfectly fine.
    #[error("identity provider unavailable: {0}")]
    Infra(#[source] anyhow::Error),
}

fn rejected(e: impl Into<anyhow::Error>) -> VerifyError {
    VerifyError::Rejected(e.into())
}

/// The claims read back after signature verification. `aud`/`exp` are enforced by
/// `jsonwebtoken`'s validation; `iss`/`sub` are checked by [`OidcVerifier::verify`].
#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    iss: String,
    #[serde(default)]
    sub: String,
}

/// Verifies an OpenID-Connect ID token against a provider's JWKS: signature checked
/// against the fetched key set, alg ∈ {RS256, ES256}, `aud` == the configured
/// audience, `iss` has the expected prefix, `exp` required and in the future,
/// non-empty `sub` (for Epic, the account/product user id).
pub(crate) struct OidcVerifier {
    audience: String,
    issuer_prefix: String,
    jwks_url: String,
    http: reqwest::Client,
    /// The cached key set; `None` until the first SUCCESSFUL fetch fills it.
    keys: RwLock<Option<JwkSet>>,
    /// Singleflight + cooldown for JWKS refetches: the mutex serializes refreshers
    /// (concurrent cache misses queue and re-check the cache the winner filled);
    /// the `Instant` is the last fetch ATTEMPT (success or failure), so within
    /// [`MIN_REFRESH_INTERVAL`] no second fetch is issued.
    refresh: Mutex<Option<Instant>>,
}

impl OidcVerifier {
    /// Pure construction — no I/O (the JWKS is fetched lazily). `http` failures at
    /// client-build time are configuration errors surfaced at `init`.
    pub fn new(jwks_url: &str, issuer_prefix: &str, audience: &str) -> anyhow::Result<OidcVerifier> {
        Ok(OidcVerifier {
            audience: audience.to_string(),
            issuer_prefix: issuer_prefix.to_string(),
            jwks_url: jwks_url.to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            keys: RwLock::new(None),
            refresh: Mutex::new(None),
        })
    }

    /// The key for `kid` (or the first key when the token carries none), consulting
    /// the cache first and refetching the JWKS on a miss — so a provider key
    /// rotation heals without a restart. Refetches are SINGLEFLIGHT (one refresher
    /// at a time; queued misses re-check the winner's cache) and rate-bounded by
    /// [`MIN_REFRESH_INTERVAL`], because `kid` is attacker-controlled input on an
    /// unauthenticated path — without the bound every bogus token is one IdP fetch.
    async fn key_for(&self, kid: Option<&str>) -> Result<Jwk, VerifyError> {
        if let Some(set) = self.keys.read().await.as_ref() {
            if let Some(k) = find_key(set, kid) {
                return Ok(k.clone());
            }
        }
        // Singleflight: the mutex is held across the whole fetch, so concurrent
        // misses queue here and first re-check the cache the winner just filled.
        let mut refresh = self.refresh.lock().await;
        if let Some(set) = self.keys.read().await.as_ref() {
            if let Some(k) = find_key(set, kid) {
                return Ok(k.clone());
            }
        }
        // Cooldown: within MIN_REFRESH_INTERVAL of the last ATTEMPT, don't hit the
        // IdP again. An unknown kid while a recent successful fetch is cached is a
        // bad token (Rejected → 401); if no fetch has EVER succeeded there is no
        // verdict to give (Infra → 503).
        if let Some(last) = *refresh {
            if last.elapsed() < MIN_REFRESH_INTERVAL {
                return if self.keys.read().await.is_some() {
                    Err(VerifyError::Rejected(anyhow::anyhow!(
                        "no JWKS key for kid {kid:?} (refresh cooldown)"
                    )))
                } else {
                    Err(VerifyError::Infra(anyhow::anyhow!(
                        "JWKS never fetched and refresh is cooling down"
                    )))
                };
            }
        }
        let fetched: anyhow::Result<JwkSet> = async {
            Ok(self
                .http
                .get(&self.jwks_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?)
        }
        .await;
        // Stamp the ATTEMPT (success or failure) so a flood against a down IdP is
        // also bounded to one fetch per interval.
        *refresh = Some(Instant::now());
        let set = fetched.map_err(VerifyError::Infra)?;
        let found = find_key(&set, kid).cloned();
        *self.keys.write().await = Some(set);
        found.ok_or_else(|| {
            VerifyError::Rejected(anyhow::anyhow!("no JWKS key for kid {kid:?} after fresh fetch"))
        })
    }

    /// Returns the token subject if the token is authentic and valid. Failures are
    /// typed: a demonstrably bad token — bad signature, disallowed alg, wrong
    /// audience, expired, foreign issuer, missing subject, unknown kid after a
    /// fresh fetch — is [`VerifyError::Rejected`] (→ 401); a JWKS fetch failure
    /// with no cached verdict is [`VerifyError::Infra`] (→ 503).
    pub async fn verify(&self, token: &str) -> Result<String, VerifyError> {
        let header = decode_header(token).map_err(rejected)?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            return Err(rejected(anyhow::anyhow!("disallowed token alg {:?}", header.alg)));
        }
        let jwk = self.key_for(header.kid.as_deref()).await?;
        let key = DecodingKey::from_jwk(&jwk).map_err(rejected)?;

        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[&self.audience]);
        // `exp` presence + freshness (Go's WithExpirationRequired); `aud` presence is
        // implied by set_audience.
        validation.set_required_spec_claims(&["exp", "aud"]);
        let data = decode::<Claims>(token, &key, &validation).map_err(rejected)?;

        if !data.claims.iss.starts_with(&self.issuer_prefix) {
            return Err(rejected(anyhow::anyhow!("unexpected issuer {:?}", data.claims.iss)));
        }
        if data.claims.sub.is_empty() {
            return Err(rejected(anyhow::anyhow!("missing subject")));
        }
        Ok(data.claims.sub)
    }
}

/// `kid` match when the token names one; otherwise the first key (a single-key set).
fn find_key<'a>(set: &'a JwkSet, kid: Option<&str>) -> Option<&'a Jwk> {
    match kid {
        Some(kid) => set.keys.iter().find(|k| k.common.key_id.as_deref() == Some(kid)),
        None => set.keys.first(),
    }
}

/// The first 8 chars of an external subject — the placeholder display name
/// `epic:<shortID>` a first-sight login provisions (Go's `shortID`).
pub(crate) fn short_id(s: &str) -> &str {
    if s.len() > 8 {
        &s[..8]
    } else {
        s
    }
}

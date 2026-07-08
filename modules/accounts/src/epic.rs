//! The OIDC id_token verifier (port of Go's `modules/accounts/epic.go`). It is
//! configured, not hardcoded: Epic is the first user, Google (also OIDC) is the
//! known next one. The backend is a trusted VERIFIER — it never holds the user's
//! credentials, only checks the IdP's signed token (the EOS Connect model).
//!
//! Divergence from Go's shape (not semantics): Go's `keyfunc.NewDefault` fetched the
//! JWKS eagerly inside `Init`; Rust `init` must do no I/O (constraint #8), so the
//! JWKS is fetched LAZILY on first verify and cached, with one refetch when a
//! token's `kid` is absent from the cached set (the keyfunc refresh behaviour).

use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

/// The signature algorithms accepted — excludes `none` and every HMAC variant by
/// construction (Go's `jwt.WithValidMethods({"RS256","ES256"})`).
const ALLOWED_ALGS: [Algorithm; 2] = [Algorithm::RS256, Algorithm::ES256];

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
    /// The cached key set; `None` until the first verify fetches it.
    keys: RwLock<Option<JwkSet>>,
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
        })
    }

    /// The key for `kid` (or the first key when the token carries none), consulting
    /// the cache first and refetching the JWKS once on a miss — so a provider key
    /// rotation heals without a restart.
    async fn key_for(&self, kid: Option<&str>) -> anyhow::Result<Jwk> {
        if let Some(set) = self.keys.read().await.as_ref() {
            if let Some(k) = find_key(set, kid) {
                return Ok(k.clone());
            }
        }
        let set: JwkSet = self
            .http
            .get(&self.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let found = find_key(&set, kid).cloned();
        *self.keys.write().await = Some(set);
        found.ok_or_else(|| anyhow::anyhow!("no JWKS key for kid {kid:?}"))
    }

    /// Returns the token subject if the token is authentic and valid; any failure —
    /// bad signature, disallowed alg, wrong audience, expired, foreign issuer,
    /// missing subject — is an `Err` the caller maps to Unauthorized.
    pub async fn verify(&self, token: &str) -> anyhow::Result<String> {
        let header = decode_header(token)?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            anyhow::bail!("disallowed token alg {:?}", header.alg);
        }
        let jwk = self.key_for(header.kid.as_deref()).await?;
        let key = DecodingKey::from_jwk(&jwk)?;

        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[&self.audience]);
        // `exp` presence + freshness (Go's WithExpirationRequired); `aud` presence is
        // implied by set_audience.
        validation.set_required_spec_claims(&["exp", "aud"]);
        let data = decode::<Claims>(token, &key, &validation)?;

        if !data.claims.iss.starts_with(&self.issuer_prefix) {
            anyhow::bail!("unexpected issuer {:?}", data.claims.iss);
        }
        if data.claims.sub.is_empty() {
            anyhow::bail!("missing subject");
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

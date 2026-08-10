//! The OIDC id_token verifier (port of Go's `modules/accounts/epic.go`), shared by
//! every OIDC provider: nothing here is provider-specific, the differences are the
//! JWKS url, the issuer rule and the audiences handed to [`OidcVerifier::new`]. The
//! backend is a trusted VERIFIER — it never holds the user's credentials, only
//! checks the IdP's signed token (the EOS Connect model).
//!
//! Divergence from Go's shape (not semantics): Go's `keyfunc.NewDefault` fetched the
//! JWKS eagerly inside `Init`; Rust `init` must do no I/O (constraint #8), so the
//! JWKS is fetched LAZILY on first verify and cached with its fetch instant. A
//! cached kid is accepted only while the set is younger than [`JWKS_CACHE_TTL`]
//! (so a key the provider rotates out stops being accepted without a restart); a
//! stale set, or a `kid` absent from the cached set, triggers one refetch (the
//! keyfunc refresh behaviour), rate-bounded by [`MIN_REFRESH_INTERVAL`].

use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

use crate::providers::{check_absolute_url, VerifyError};

/// The signature algorithms accepted — excludes `none` and every HMAC variant by
/// construction (Go's `jwt.WithValidMethods({"RS256","ES256"})`).
const ALLOWED_ALGS: [Algorithm; 2] = [Algorithm::RS256, Algorithm::ES256];

/// Cooldown between JWKS fetch ATTEMPTS: a flood of bogus-`kid` tokens (an
/// unauthenticated caller controls the header) costs the IdP at most one fetch per
/// interval instead of one per request.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long a cached JWKS answers `kid` lookups before it is treated as stale and a
/// refetch is attempted. This bounds how long a key the provider has ROTATED OUT (e.g.
/// after a compromise) stays accepted: at most `JWKS_CACHE_TTL` past the rotation, since a
/// hit is honoured only while the set is younger than this (the full-set swap on
/// refetch is what actually drops the rotated kid).
const JWKS_CACHE_TTL: Duration = Duration::from_secs(600);

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

/// How a provider's `iss` claim is accepted. Both rules are lookalike-safe:
/// [`IssuerMatch::prefix`] matches only at a PATH boundary, so
/// `https://accounts.google.com.evil.test` fails a
/// `prefix("https://accounts.google.com")` guard exactly as it fails an exact one.
/// The variants differ in what they can EXPRESS: `prefix` covers a provider whose
/// issuers live under one URL, [`IssuerMatch::exact`] a provider with a fixed list of
/// spellings — including a scheme-less one, which the prefix rule's absolute-URL floor
/// cannot carry.
///
/// The inner match is PRIVATE so [`IssuerMatch::prefix`] / [`IssuerMatch::exact`] are
/// the only way to build one: each carries its variant's validation rule, and a caller
/// that could name the variant directly could skip that rule.
pub(crate) struct IssuerMatch(Match);

enum Match {
    Exact(Vec<String>),
    Prefix(String),
}

impl IssuerMatch {
    /// The prefix rule: an absolute URL with a host, matched at a path boundary by
    /// [`IssuerMatch::accepts`]. The absolute-URL floor is what stops a truncated value
    /// (`h`) from accepting every https issuer. Trailing slashes are trimmed HERE, so
    /// `https://host/v1/` and `https://host/v1` are one rule and neither spelling makes
    /// the boundary reject the provider's own bare issuer.
    pub(crate) fn prefix(key: &str, value: &str) -> anyhow::Result<IssuerMatch> {
        check_absolute_url(key, value)?;
        Ok(IssuerMatch(Match::Prefix(value.trim_end_matches('/').to_string())))
    }

    /// The exact rule: a non-empty list of non-empty spellings. An exact comparison
    /// cannot be widened by truncation, so the absolute-URL floor does NOT apply here —
    /// a bare host (`accounts.google.com`) is a real issuer value for a provider that
    /// emits both spellings. An empty list would match nothing at all.
    pub(crate) fn exact(key: &str, values: &[&str]) -> anyhow::Result<IssuerMatch> {
        if values.is_empty() {
            anyhow::bail!("invalid {key}: no issuer values — nothing would ever verify");
        }
        if values.iter().any(|v| v.is_empty()) {
            anyhow::bail!("invalid {key}: empty issuer value");
        }
        Ok(IssuerMatch(Match::Exact(
            values.iter().map(|v| v.to_string()).collect(),
        )))
    }

    fn accepts(&self, iss: &str) -> bool {
        match &self.0 {
            Match::Exact(values) => values.iter().any(|v| v == iss),
            Match::Prefix(prefix) => iss
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/')),
        }
    }
}

/// Verifies an OpenID-Connect ID token against a provider's JWKS: signature checked
/// against the fetched key set, alg ∈ {RS256, ES256}, `aud` ∈ the configured
/// audiences, `iss` accepted by the configured [`IssuerMatch`], `exp` required and in
/// the future, non-empty `sub` (the provider's own account identifier).
pub(crate) struct OidcVerifier {
    audiences: Vec<String>,
    issuer: IssuerMatch,
    jwks_url: String,
    http: reqwest::Client,
    /// The cached key set paired with the [`Instant`] it was fetched; `None` until
    /// the first SUCCESSFUL fetch fills it. The instant drives [`JWKS_CACHE_TTL`]
    /// expiry on the hit path.
    keys: RwLock<Option<(JwkSet, Instant)>>,
    /// Singleflight + cooldown for JWKS refetches: the mutex serializes refreshers
    /// (concurrent cache misses queue and re-check the cache the winner filled);
    /// the `Instant` is the last fetch ATTEMPT (success or failure), so within
    /// [`MIN_REFRESH_INTERVAL`] no second fetch is issued.
    refresh: Mutex<Option<Instant>>,
}

impl OidcVerifier {
    /// Pure construction — no I/O (the JWKS is fetched lazily). `http` failures at
    /// client-build time are configuration errors surfaced at `init`. An empty
    /// `audiences` is rejected here because `jsonwebtoken` matches `aud` against the
    /// configured set, so an empty set rejects every token: the provider would be
    /// enabled and unable to verify anything. That is a startup failure, not a
    /// login-time mystery.
    pub fn new(
        jwks_url: &str,
        issuer: IssuerMatch,
        audiences: Vec<String>,
    ) -> anyhow::Result<OidcVerifier> {
        if audiences.is_empty() {
            anyhow::bail!("OIDC verifier for {jwks_url}: no audiences configured");
        }
        Ok(OidcVerifier {
            audiences,
            issuer,
            jwks_url: jwks_url.to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
            keys: RwLock::new(None),
            refresh: Mutex::new(None),
        })
    }

    /// The key for `kid` (or the first key when the token carries none), consulting
    /// the cache first and refetching the JWKS when the cached set is stale
    /// ([`JWKS_CACHE_TTL`]) or missing the `kid` — so a provider key rotation both
    /// heals a new key and EXPIRES a rotated-out one without a restart. Refetches are
    /// SINGLEFLIGHT (one refresher at a time; queued misses re-check the winner's
    /// cache) and rate-bounded by [`MIN_REFRESH_INTERVAL`], because `kid` is
    /// attacker-controlled input on an unauthenticated path — without the bound every
    /// bogus token is one IdP fetch.
    async fn key_for(&self, kid: Option<&str>) -> Result<Jwk, VerifyError> {
        // Hit path: a cached kid is honoured only while the set is still fresh.
        if let Some(k) = self.fresh_hit(kid).await {
            return Ok(k);
        }
        // Singleflight: the mutex is held across the whole fetch, so concurrent
        // stale/misses queue here and first re-check the fresh cache the winner
        // just filled.
        let mut refresh = self.refresh.lock().await;
        if let Some(k) = self.fresh_hit(kid).await {
            return Ok(k);
        }
        // Cooldown: within MIN_REFRESH_INTERVAL of the last ATTEMPT, don't hit the
        // IdP again. Freshness degrades OPEN, unknown kids stay CLOSED: if the (now
        // stale) cache still answers this kid, serve it rather than reject a valid
        // login; an unknown kid while a fetch has ever succeeded is a bad token
        // (Rejected → 401); if no fetch has EVER succeeded there is no verdict to
        // give (Infra → 503).
        if let Some(last) = *refresh {
            if last.elapsed() < MIN_REFRESH_INTERVAL {
                return match self.keys.read().await.as_ref() {
                    Some((set, _)) => find_key(set, kid).cloned().ok_or_else(|| {
                        VerifyError::Rejected(anyhow::anyhow!(
                            "no JWKS key for kid {kid:?} (refresh cooldown)"
                        ))
                    }),
                    None => Err(VerifyError::Infra(anyhow::anyhow!(
                        "JWKS never fetched and refresh is cooling down"
                    ))),
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
        *self.keys.write().await = Some((set, Instant::now()));
        found.ok_or_else(|| {
            VerifyError::Rejected(anyhow::anyhow!("no JWKS key for kid {kid:?} after fresh fetch"))
        })
    }

    /// A cached key for `kid`, but ONLY if the cached set is younger than
    /// [`JWKS_CACHE_TTL`] — a stale set (or an absent kid) returns `None` so the
    /// caller falls through to the refresh path.
    async fn fresh_hit(&self, kid: Option<&str>) -> Option<Jwk> {
        let guard = self.keys.read().await;
        let (set, fetched_at) = guard.as_ref()?;
        if fetched_at.elapsed() >= JWKS_CACHE_TTL {
            return None;
        }
        find_key(set, kid).cloned()
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
        validation.set_audience(&self.audiences);
        // `exp` presence + freshness (Go's WithExpirationRequired); `aud` presence is
        // implied by set_audience.
        validation.set_required_spec_claims(&["exp", "aud"]);
        let data = decode::<Claims>(token, &key, &validation).map_err(rejected)?;

        if !self.issuer.accepts(&data.claims.iss) {
            return Err(rejected(anyhow::anyhow!("unexpected issuer {:?}", data.claims.iss)));
        }
        if data.claims.sub.is_empty() {
            return Err(rejected(anyhow::anyhow!("missing subject")));
        }
        Ok(data.claims.sub)
    }
}

#[cfg(test)]
impl OidcVerifier {
    /// Test-only: rewind the cached set's fetch instant past [`JWKS_CACHE_TTL`] so the
    /// next hit reads it as stale, exercising the TTL path without a 10-minute sleep.
    pub(crate) async fn expire_cache_for_test(&self) {
        if let Some((_, fetched_at)) = self.keys.write().await.as_mut() {
            *fetched_at = fetched_at
                .checked_sub(JWKS_CACHE_TTL + Duration::from_secs(1))
                .expect("Instant rewind (machine uptime exceeds JWKS_CACHE_TTL)");
        }
    }

    /// Test-only: clear the refresh cooldown so the next stale/miss is permitted to
    /// refetch, without waiting out [`MIN_REFRESH_INTERVAL`]. (Distinguishes the
    /// stale-refetch path from the stale-under-cooldown degrade-open path.)
    pub(crate) async fn reset_cooldown_for_test(&self) {
        *self.refresh.lock().await = None;
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
/// `<provider>:<shortID>` a first-sight login provisions (Go's `shortID`).
pub(crate) fn short_id(s: &str) -> &str {
    truncate(s, 8)
}

/// The crate's one string cut: `n` CHARACTERS, never bytes — a byte slice panics when
/// the cut lands inside a multibyte character.
pub(crate) fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

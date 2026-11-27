//! The bearer-token → player_id verifier the gateway consults at its single
//! auth-once seam. A consumer-defined interface (CLAUDE.md rule 4): the gateway
//! depends on the *capability* to verify a token, not on the `accounts` module.
//!
//! The real verifier is [`JwtVerifier`]: it checks an ES256 access token locally
//! against a snapshot of the `accounts.token_state` capability
//! (`accountsapi::TokenState`, a CONTRACT crate), which a pull loop refreshes. The
//! registry swap keeps it topology-blind: the accounts module provides the capability
//! locally, and an `accountsrpc` edge client provides it in every other split process.
//!
//! Fallback policy (NO silent dev fallback): when the capability is absent at init
//! the gateway FAILS STARTUP loudly — unless `GATEWAY_DEV_VERIFIER` is EXPLICITLY set
//! truthy, which enables [`DevSessionVerifier`] with its loud warning. A
//! mis-configured split gateway can no longer silently accept `dev-` tokens.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Once, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use lifecycle::Context;
use serde::Deserialize;

/// The single non-auth failure class of a [`SessionVerifier::verify`]: the verifier
/// has no current answer from accounts and so cannot say whether the token is valid. It is DISTINCT from `Ok(None)` (the token is definitively
/// missing/invalid): an outage must surface as 503/`Status::Unavailable` (retry may
/// succeed), never masquerade as an invalid session (which would log players out en
/// masse the moment accounts blips).
#[derive(Debug)]
pub struct VerifyUnavailable;

/// Verifies a bearer token and returns the caller's `player_id`, `Ok(None)` when the
/// token is missing/invalid, or `Err(VerifyUnavailable)` when the verifier itself is
/// unreachable (an accounts outage — a retryable dependency failure, NOT an invalid
/// session). The gateway calls this at exactly one place — the front handler, after
/// extracting the `Authorization: Bearer <token>` header — and threads the resolved id
/// through the operation as an `opsapi::Identity`. Nothing downstream re-verifies.
#[async_trait]
pub trait SessionVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<Option<String>, VerifyUnavailable>;
}

/// The `dev-` token prefix [`DevSessionVerifier`] accepts.
const DEV_PREFIX: &str = "dev-";

/// A dev-only [`SessionVerifier`] standing in for the real `accounts` token
/// verification. It accepts a token shaped **`dev-<player_id>`** and returns the
/// suffix verbatim as the player_id (e.g. `dev-alice` → `alice`,
/// `dev-4f9c…-uuid` → `4f9c…-uuid`); ANY other token is rejected. It emits a single
/// loud warning the first time it is consulted, so it is impossible to run this in
/// production without noticing dev auth is on.
pub struct DevSessionVerifier {
    warned: Once,
}

impl DevSessionVerifier {
    pub fn new() -> Self {
        DevSessionVerifier { warned: Once::new() }
    }
}

impl Default for DevSessionVerifier {
    fn default() -> Self {
        DevSessionVerifier::new()
    }
}

#[async_trait]
impl SessionVerifier for DevSessionVerifier {
    async fn verify(&self, token: &str) -> Result<Option<String>, VerifyUnavailable> {
        if token.len() > accountsapi::MAX_ACCESS_TOKEN_BYTES {
            return Ok(None);
        }
        self.warned.call_once(|| {
            tracing::warn!(
                "DEV SESSION AUTH IS ON: the gateway accepts any `Bearer dev-<player_id>` token \
                 as a verified player. This is a dev stand-in for `accounts` — NEVER run \
                 it in production."
            );
        });
        // `dev-` prefix with a non-empty suffix → that suffix is the player_id. The
        // dev verifier never has an outage — it is a pure prefix match.
        Ok(match token.strip_prefix(DEV_PREFIX) {
            Some(pid) if !pid.is_empty() => Some(pid.to_string()),
            _ => None,
        })
    }
}

/// How often the pull loop asks `accounts.token_state` for a fresh snapshot.
pub(crate) const PULL_INTERVAL: Duration = Duration::from_secs(5);
/// The bound on one `token_state` call.
pub(crate) const PULL_TIMEOUT: Duration = Duration::from_secs(4);
/// The oldest snapshot [`JwtVerifier`] verifies with, measured from the instant its pull
/// started; past it every bearer answers [`VerifyUnavailable`].
pub(crate) const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(60);

/// The clocks [`JwtVerifier`] reads: wall time for a token's `iat` and `exp`, and a
/// monotonic instant for the age of its snapshot.
pub(crate) trait Clocks: Send + Sync {
    /// Wall time in Unix seconds.
    fn unix_now(&self) -> i64;
    fn mono_now(&self) -> Instant;
}

struct SystemClocks;

impl Clocks for SystemClocks {
    fn unix_now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn mono_now(&self) -> Instant {
        Instant::now()
    }
}

/// The claims of an access token this verifier reads; `iss` and `aud` are checked by
/// the [`Validation`].
#[derive(Deserialize)]
struct AccessClaims {
    sub: String,
    sid: String,
    client_id: String,
    iat: i64,
    exp: i64,
}

/// A [`accountsapi::TokenStateSnapshot`] prepared for verification, stamped with the
/// instant before the pull that fetched it.
struct Loaded {
    pulled_at: Instant,
    validation: Validation,
    keys: HashMap<String, DecodingKey>,
    revoked_families: HashSet<String>,
    revoked_players: HashMap<String, i64>,
    revoked_clients: HashMap<String, i64>,
}

impl Loaded {
    fn new(snapshot: accountsapi::TokenStateSnapshot, pulled_at: Instant) -> Loaded {
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_issuer(&[snapshot.issuer.as_str()]);
        validation.set_audience(&[accountsapi::SWAROG_AUDIENCE]);
        // `exp` is checked against the injected wall clock; with both `validate_exp` and
        // `validate_nbf` off jsonwebtoken reads no clock (jsonwebtoken-9.3.1 validation.rs:271).
        validation.validate_exp = false;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let mut keys = HashMap::new();
        for jwk in snapshot.keys {
            if jwk.kty != "EC" || jwk.crv != "P-256" || jwk.alg != "ES256" {
                continue;
            }
            match DecodingKey::from_ec_components(&jwk.x, &jwk.y) {
                Ok(key) => {
                    keys.insert(jwk.kid, key);
                }
                Err(err) => {
                    tracing::warn!(%err, kid = %jwk.kid, "gateway: skipping an unreadable signing key");
                }
            }
        }
        Loaded {
            pulled_at,
            validation,
            keys,
            revoked_families: snapshot.revoked_families.into_iter().collect(),
            revoked_players: cut_offs(snapshot.revoked_players),
            revoked_clients: cut_offs(snapshot.revoked_clients),
        }
    }

    /// The token's `sub` when it is an ES256 `at+jwt` that passes the signature, `iss`, `aud`
    /// and `exp` checks and no revocation refuses it; `None` otherwise.
    fn verify(&self, token: &str, unix_now: i64) -> Option<String> {
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg != Algorithm::ES256
            || header.typ.as_deref() != Some(accountsapi::ACCESS_TOKEN_TYP)
        {
            return None;
        }
        let key = self.keys.get(header.kid.as_deref()?)?;
        let claims = jsonwebtoken::decode::<AccessClaims>(token, key, &self.validation)
            .ok()?
            .claims;
        let leeway = i64::try_from(accountsapi::ACCESS_EXP_LEEWAY.as_secs()).unwrap_or(i64::MAX);
        if claims.exp < unix_now.saturating_sub(leeway) || claims.sub.is_empty() {
            return None;
        }
        if self.revoked_families.contains(&claims.sid) {
            return None;
        }
        let refused = |cut_offs: &HashMap<String, i64>, id: &str| {
            cut_offs.get(id).is_some_and(|revoked_at| claims.iat <= *revoked_at)
        };
        if refused(&self.revoked_players, &claims.sub)
            || refused(&self.revoked_clients, &claims.client_id)
        {
            return None;
        }
        Some(claims.sub)
    }
}

/// Cut-offs by id; a repeated id keeps its latest cut-off.
fn cut_offs(revoked: Vec<accountsapi::Revoked>) -> HashMap<String, i64> {
    let mut map = HashMap::new();
    for r in revoked {
        let at = map.entry(r.id).or_insert(r.revoked_at_unix);
        *at = (*at).max(r.revoked_at_unix);
    }
    map
}

/// The real verifier: checks an ES256 access token locally against the latest
/// `accounts.token_state` snapshot. A bearer over [`accountsapi::MAX_ACCESS_TOKEN_BYTES`]
/// is `Ok(None)` before anything else; no snapshot, or one older than
/// [`SNAPSHOT_MAX_AGE`], is `Err(VerifyUnavailable)`.
pub struct JwtVerifier {
    source: Arc<dyn accountsapi::TokenState>,
    clocks: Arc<dyn Clocks>,
    state: RwLock<Option<Arc<Loaded>>>,
}

impl JwtVerifier {
    pub fn new(source: Arc<dyn accountsapi::TokenState>) -> JwtVerifier {
        JwtVerifier::with_clocks(source, Arc::new(SystemClocks))
    }

    pub(crate) fn with_clocks(
        source: Arc<dyn accountsapi::TokenState>,
        clocks: Arc<dyn Clocks>,
    ) -> JwtVerifier {
        JwtVerifier { source, clocks, state: RwLock::new(None) }
    }

    /// One pull: on success the snapshot replaces the previous one whole, stamped with
    /// the instant taken before the call; on failure the previous snapshot stays.
    pub(crate) async fn refresh_once(&self) {
        let started = self.clocks.mono_now();
        match tokio::time::timeout(PULL_TIMEOUT, self.source.token_state()).await {
            Ok(Ok(snapshot)) => {
                let loaded = Arc::new(Loaded::new(snapshot, started));
                *self.state.write().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
            }
            Ok(Err(err)) => {
                tracing::warn!(%err, "gateway: token_state pull failed (accounts unreachable?)");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = PULL_TIMEOUT.as_millis(),
                    "gateway: token_state pull timed out"
                );
            }
        }
    }

    /// Spawns the pull loop: one pull at once, then one every [`PULL_INTERVAL`].
    pub(crate) fn spawn_pull(self: &Arc<Self>) -> PullTask {
        let verifier = self.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PULL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                verifier.refresh_once().await;
            }
        });
        PullTask { task }
    }

    /// Whether a snapshot younger than [`SNAPSHOT_MAX_AGE`] is loaded: exactly when `verify`
    /// is not `Err(VerifyUnavailable)`.
    pub(crate) fn is_fresh(&self) -> bool {
        self.fresh().is_some()
    }

    /// The `/readyz` check `"token-state"`: red while there is no fresh snapshot, so a dead
    /// or failing pull loop is visible there and not only as 503s on requests.
    pub(crate) fn readiness_check(self: &Arc<Self>) -> httpmw::ReadyCheck {
        let verifier = self.clone();
        httpmw::ReadyCheck::new("token-state", move || {
            let verifier = verifier.clone();
            async move {
                if verifier.is_fresh() {
                    Ok(())
                } else {
                    Err(format!(
                        "no accounts.token_state snapshot pulled in the last {}s",
                        SNAPSHOT_MAX_AGE.as_secs()
                    ))
                }
            }
        })
    }

    fn fresh(&self) -> Option<Arc<Loaded>> {
        let loaded = self.state.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        let age = self.clocks.mono_now().saturating_duration_since(loaded.pulled_at);
        (age <= SNAPSHOT_MAX_AGE).then_some(loaded)
    }
}

#[async_trait]
impl SessionVerifier for JwtVerifier {
    async fn verify(&self, token: &str) -> Result<Option<String>, VerifyUnavailable> {
        if token.len() > accountsapi::MAX_ACCESS_TOKEN_BYTES {
            return Ok(None);
        }
        let loaded = self.fresh().ok_or(VerifyUnavailable)?;
        Ok(loaded.verify(token, self.clocks.unix_now()))
    }
}

/// The running pull loop of a [`JwtVerifier`], owned by the gateway from `start` to
/// `stop`.
pub(crate) struct PullTask {
    task: tokio::task::JoinHandle<()>,
}

impl PullTask {
    /// Aborts the loop and awaits the aborted handle; a pull in flight is a retry-safe
    /// read, so nothing needs a graceful end and the stop takes no grace.
    pub(crate) async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// What [`resolve_verifier`] chose: the verifier the front door uses and, when it is a
/// [`JwtVerifier`], that verifier again so the gateway runs its pull loop.
pub(crate) struct ResolvedVerifier {
    pub(crate) verifier: Arc<dyn SessionVerifier>,
    pub(crate) pull: Option<Arc<JwtVerifier>>,
}

/// Resolves the process's [`SessionVerifier`] at gateway `init` (phase 2 — every
/// provider's phase-1 `register`, module or stub, has already run):
///
///   1. the `accounts.token_state` capability, when present → [`JwtVerifier`];
///   2. else, `GATEWAY_DEV_VERIFIER` EXPLICITLY truthy (`1`/`true`/`on`) →
///      [`DevSessionVerifier`] with a loud warning;
///   3. else → **fail startup loudly** (review item 5: no silent dev fallback).
pub(crate) fn resolve_verifier(ctx: &Context) -> anyhow::Result<ResolvedVerifier> {
    resolve_verifier_with(ctx, dev_verifier_on(|key| std::env::var(key).ok()))
}

pub(crate) fn resolve_verifier_with(
    ctx: &Context,
    dev_verifier: bool,
) -> anyhow::Result<ResolvedVerifier> {
    if let Some(source) = ctx
        .registry()
        .try_require::<dyn accountsapi::TokenState>(&registry::key("accounts", "token_state"))
    {
        let jwt = Arc::new(JwtVerifier::new(source));
        return Ok(ResolvedVerifier { verifier: jwt.clone(), pull: Some(jwt) });
    }
    if dev_verifier {
        tracing::warn!(
            "gateway: no accounts.token_state capability in this process; GATEWAY_DEV_VERIFIER \
             is explicitly set, so falling back to DevSessionVerifier (dev-<player_id> tokens)"
        );
        return Ok(ResolvedVerifier { verifier: Arc::new(DevSessionVerifier::new()), pull: None });
    }
    anyhow::bail!(
        "gateway: no accounts.token_state capability is available in this process — add the \
         accounts module (monolith) or a remote::Stub::new(\"accounts\", ..., \
         accountsrpc::remote_factories()) (split), or set GATEWAY_DEV_VERIFIER=1 explicitly \
         for local dev-token auth"
    )
}

/// `true` only when `GATEWAY_DEV_VERIFIER` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive) in `var`, the environment reader; unset is `false`.
pub(crate) fn dev_verifier_on(var: impl Fn(&str) -> Option<String>) -> bool {
    matches!(
        var("GATEWAY_DEV_VERIFIER"),
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

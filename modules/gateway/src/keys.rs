//! The API-key → client-class policy verifier the gateway consults on every
//! op-dispatched request — the SECOND credential seam beside `verifier.rs`'s session
//! verifier, and deliberately the same shape: a consumer-defined trait (rule 4), a
//! REAL adapter over the `apikeys.keys` capability (`apikeysapi::Keys`, a CONTRACT
//! crate — never the impl), resolved from the registry at gateway `init`, and an
//! explicit-only dev escape hatch. A session bearer authorizes the *player*; a key
//! authorizes the *client class* — orthogonal checks, both enforced at the front.
//!
//! Unlike sessions, key lookups sit on EVERY request (both planes), so the real
//! verifier wraps the capability in a small TTL cache (5 s): `Ok(Some)` AND `Ok(None)`
//! are cached (bounding DB/edge chatter under bad-key spam), but an `Err` is NEVER
//! cached — an apikeys-svc blip must not poison a valid key as a 401 for a whole TTL
//! (the per-request `Err → deny` collapse still applies, logged `error!`). Revocation
//! and policy edits therefore propagate within ≤ TTL in both topologies.
//!
//! Fallback policy (NO silent dev fallback, mirroring `resolve_verifier`): when the
//! capability is absent at init the gateway FAILS STARTUP loudly — unless
//! `APIKEYS_DEV_ALLOW` is EXPLICITLY set truthy, which enables [`AllowAllKeyVerifier`]
//! with its loud warning.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use lifecycle::Context;
use opsapi::Status;

use apikeysapi::KeyRecord;

/// How long one key lookup result (present OR absent) is served from the cache before
/// the capability is consulted again — the propagation bound for a revoke/policy edit.
const KEY_CACHE_TTL: Duration = Duration::from_secs(5);

/// Cache size bound: on insert at this many entries the map is CLEARED (crude, O(1)
/// amortized) — an attacker spraying distinct garbage keys cannot grow memory without
/// bound, and a full clear costs at most one extra lookup per live key.
const KEY_CACHE_MAX_ENTRIES: usize = 10_000;

/// Resolves an API key string to its [`KeyRecord`], or `None` when the key is
/// missing from the store, revoked, or the lookup failed (the `Err → deny` collapse —
/// the trait's contract is "known key or not", exactly like `SessionVerifier`). The
/// gateway calls this at exactly one place per plane, after route/method match and
/// before session auth.
#[async_trait]
pub trait KeyVerifier: Send + Sync {
    async fn lookup(&self, key: &str) -> Option<KeyRecord>;
}

/// The three ways the front denies a request at the key check. One evaluation
/// ([`check_api_key`]) serves BOTH planes; only the response envelope differs, so the
/// message + status mapping lives here and cannot drift between HTTP and player-QUIC.
pub(crate) enum KeyDenial {
    /// No key was presented at all (no `X-Api-Key` header / no `api_key` field).
    Missing,
    /// A key was presented but is unknown or revoked (or the lookup failed).
    Invalid,
    /// The key is valid but its policy does not include the requested method.
    Forbidden,
}

impl KeyDenial {
    /// The plane-independent denial message (Decision 5's exact strings).
    pub(crate) fn message(&self) -> &'static str {
        match self {
            KeyDenial::Missing => "missing api key",
            KeyDenial::Invalid => "invalid api key",
            KeyDenial::Forbidden => "api key policy forbids this operation",
        }
    }

    /// The domain [`Status`] (→ HTTP code via `Status::http`): a missing/unknown key
    /// is Unauthorized (401 — no acceptable credential), a policy miss is Forbidden
    /// (403 — credential understood, operation refused).
    pub(crate) fn status(&self) -> Status {
        match self {
            KeyDenial::Missing | KeyDenial::Invalid => Status::Unauthorized,
            KeyDenial::Forbidden => Status::Forbidden,
        }
    }
}

/// The three-way key check both planes run after their route/method match: missing →
/// [`KeyDenial::Missing`], unknown/revoked → [`KeyDenial::Invalid`], policy miss →
/// [`KeyDenial::Forbidden`], else `Ok(())` and the request proceeds to session auth.
pub(crate) async fn check_api_key(
    verifier: &dyn KeyVerifier,
    key: Option<&str>,
    method: &str,
) -> Result<(), KeyDenial> {
    let Some(key) = key else {
        return Err(KeyDenial::Missing);
    };
    let Some(record) = verifier.lookup(key).await else {
        return Err(KeyDenial::Invalid);
    };
    if !policy_allows(&record.policy, method) {
        return Err(KeyDenial::Forbidden);
    }
    Ok(())
}

/// Evaluates a key policy against a wire method (Decision 4): the literal string
/// `full` allows everything; otherwise the policy is a comma-separated list of wire
/// method names, each compared exactly after trimming surrounding whitespace. An
/// empty policy allows nothing — and a NEW op is denied by every restricted key by
/// construction (absent from the list), which is the safe default.
pub(crate) fn policy_allows(policy: &str, method: &str) -> bool {
    policy == "full" || policy.split(',').any(|m| m.trim() == method)
}

/// A dev-only allow-all [`KeyVerifier`] for a process wired without the `apikeys`
/// capability (`APIKEYS_DEV_ALLOW` explicitly truthy). It resolves ANY presented key
/// string to a `full`-policy record ([`check_api_key`] still demands that *a* key be
/// presented — the missing-key 401 applies even in dev). It warns loudly exactly
/// once, so it is impossible to run in production without noticing.
pub struct AllowAllKeyVerifier {
    warned: Once,
}

impl AllowAllKeyVerifier {
    pub fn new() -> Self {
        AllowAllKeyVerifier { warned: Once::new() }
    }
}

impl Default for AllowAllKeyVerifier {
    fn default() -> Self {
        AllowAllKeyVerifier::new()
    }
}

#[async_trait]
impl KeyVerifier for AllowAllKeyVerifier {
    async fn lookup(&self, _key: &str) -> Option<KeyRecord> {
        self.warned.call_once(|| {
            tracing::warn!(
                "DEV API-KEY ALLOW-ALL IS ON: the gateway resolves ANY presented API key to a \
                 `full` policy. This is a dev stand-in for `apikeys` — NEVER run it in \
                 production."
            );
        });
        Some(KeyRecord { name: "dev-allow-all".to_string(), policy: "full".to_string() })
    }
}

/// The REAL verifier: adapts the `apikeys.keys` capability to [`KeyVerifier`] behind
/// the TTL cache described in the module doc. `Ok(Some)`/`Ok(None)` are cached for
/// [`KEY_CACHE_TTL`]; an `Err` (apikeys store/peer failure) is logged and collapses to
/// `None` for THIS request only — never cached, so one blip costs one denial, not a
/// TTL-long outage for a valid key.
pub struct RealKeyVerifier {
    keys: Arc<dyn apikeysapi::Keys>,
    ttl: Duration,
    cache: Mutex<HashMap<String, (Option<KeyRecord>, Instant)>>,
}

impl RealKeyVerifier {
    pub fn new(keys: Arc<dyn apikeysapi::Keys>) -> RealKeyVerifier {
        RealKeyVerifier::with_ttl(keys, KEY_CACHE_TTL)
    }

    /// A verifier with an explicit TTL — the test seam (`Duration::ZERO` makes every
    /// cached entry immediately stale, so expiry is testable without sleeping).
    pub fn with_ttl(keys: Arc<dyn apikeysapi::Keys>, ttl: Duration) -> RealKeyVerifier {
        RealKeyVerifier { keys, ttl, cache: Mutex::new(HashMap::new()) }
    }

    /// Serves `key` from the cache when its entry is younger than the TTL. The lock is
    /// never held across an await.
    fn cached(&self, key: &str) -> Option<Option<KeyRecord>> {
        let cache = self.cache.lock().unwrap();
        match cache.get(key) {
            Some((record, at)) if at.elapsed() < self.ttl => Some(record.clone()),
            _ => None,
        }
    }

    /// Caches one authoritative lookup result (present OR absent), clearing the whole
    /// map first when it has reached [`KEY_CACHE_MAX_ENTRIES`] (the bounded-memory
    /// rule — see the module doc).
    fn insert(&self, key: &str, record: Option<KeyRecord>) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= KEY_CACHE_MAX_ENTRIES {
            cache.clear();
        }
        cache.insert(key.to_string(), (record, Instant::now()));
    }
}

#[async_trait]
impl KeyVerifier for RealKeyVerifier {
    async fn lookup(&self, key: &str) -> Option<KeyRecord> {
        if let Some(record) = self.cached(key) {
            return record;
        }
        match self.keys.lookup_key(key.to_string()).await {
            Ok(record) => {
                self.insert(key, record.clone());
                record
            }
            Err(err) => {
                tracing::error!(%err, "gateway: api key lookup failed (apikeys unreachable?)");
                None
            }
        }
    }
}

/// Resolves the process's [`KeyVerifier`] at gateway `init` (phase 2 — every
/// provider's phase-1 `register`, module or stub, has already run):
///
///   1. the `apikeys.keys` capability, when present → [`RealKeyVerifier`] (TTL-cached);
///   2. else, `APIKEYS_DEV_ALLOW` EXPLICITLY truthy (`1`/`true`/`on`) →
///      [`AllowAllKeyVerifier`] with a loud warning;
///   3. else → **fail startup loudly** (no silent dev fallback — the `resolve_verifier`
///      posture).
pub(crate) fn resolve_key_verifier(ctx: &Context) -> anyhow::Result<Arc<dyn KeyVerifier>> {
    if let Some(keys) = ctx
        .registry()
        .try_require::<dyn apikeysapi::Keys>(&registry::key("apikeys", "keys"))
    {
        return Ok(Arc::new(RealKeyVerifier::new(keys)));
    }
    if dev_allow_explicitly_on() {
        tracing::warn!(
            "gateway: no apikeys.keys capability in this process; APIKEYS_DEV_ALLOW is \
             explicitly set, so falling back to AllowAllKeyVerifier (any presented key \
             resolves to a full policy)"
        );
        return Ok(Arc::new(AllowAllKeyVerifier::new()));
    }
    anyhow::bail!(
        "gateway: no apikeys.keys capability is available in this process — add the \
         apikeys module (monolith) or a remote::Stub::new(\"apikeys\", ..., \
         apikeysrpc::remote_factories()) (split), or set APIKEYS_DEV_ALLOW=1 explicitly \
         to accept any api key in local dev"
    )
}

/// `true` only when `APIKEYS_DEV_ALLOW` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` here — skipping the key check is a trust
/// decision, so it follows the gateway's explicit-only convention.
fn dev_allow_explicitly_on() -> bool {
    matches!(
        std::env::var("APIKEYS_DEV_ALLOW"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

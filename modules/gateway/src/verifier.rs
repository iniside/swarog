//! The bearer-token → player_id verifier the gateway consults at its single
//! auth-once seam. A consumer-defined interface (CLAUDE.md rule 4): the gateway
//! depends on the *capability* to verify a session, not on the `accounts` module.
//!
//! Since Step 6 the REAL verifier is [`SessionsVerifier`]: an adapter over the
//! `accounts.sessions` capability (`accountsapi::Sessions`, a CONTRACT crate —
//! sanctioned like inventory's use of `charactersapi`), resolved from the registry
//! at gateway `init`. The registry swap keeps it topology-blind: the accounts
//! module provides it locally in the monolith/accounts-svc, and an `accountsrpc`
//! edge client (a `remote::Stub` factory) provides it in every other split process.
//!
//! Fallback policy (NO silent dev fallback): when the capability is absent at init
//! the gateway FAILS STARTUP loudly — unless `ACCOUNTS_DEV_AUTH` is EXPLICITLY set
//! truthy, which enables [`DevSessionVerifier`] with its loud warning. A
//! mis-configured split gateway can no longer silently accept `dev-` tokens.

use std::sync::{Arc, Once};

use async_trait::async_trait;
use lifecycle::Context;

/// Verifies a bearer token and returns the caller's `player_id`, or `None` when the
/// token is missing/invalid. The gateway calls this at exactly one place — the front
/// handler, after extracting the `Authorization: Bearer <token>` header — and threads
/// the resolved id through the operation as an `opsapi::Identity`. Nothing downstream
/// re-verifies.
#[async_trait]
pub trait SessionVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Option<String>;
}

/// The `dev-` token prefix [`DevSessionVerifier`] accepts.
const DEV_PREFIX: &str = "dev-";

/// A dev-only [`SessionVerifier`] standing in for the real `accounts` sessions
/// service in M1. It accepts a token shaped **`dev-<player_id>`** and returns the
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
    async fn verify(&self, token: &str) -> Option<String> {
        self.warned.call_once(|| {
            tracing::warn!(
                "DEV SESSION AUTH IS ON: the gateway accepts any `Bearer dev-<player_id>` token \
                 as a verified player. This is a dev stand-in for `accounts` — NEVER run \
                 it in production."
            );
        });
        // `dev-` prefix with a non-empty suffix → that suffix is the player_id.
        match token.strip_prefix(DEV_PREFIX) {
            Some(pid) if !pid.is_empty() => Some(pid.to_string()),
            _ => None,
        }
    }
}

/// The REAL verifier (Step 6): adapts the `accounts.sessions` capability to the
/// gateway's consumer-defined [`SessionVerifier`]. `Ok(None)` (unknown/expired
/// token) and `Err` (accounts store/peer failure) both verify to `None` — the
/// trait's contract is "verified player or not"; the failure is logged so an
/// accounts outage is visible rather than silently indistinguishable from bad
/// credentials.
pub struct SessionsVerifier {
    sessions: Arc<dyn accountsapi::Sessions>,
}

impl SessionsVerifier {
    pub fn new(sessions: Arc<dyn accountsapi::Sessions>) -> SessionsVerifier {
        SessionsVerifier { sessions }
    }
}

#[async_trait]
impl SessionVerifier for SessionsVerifier {
    async fn verify(&self, token: &str) -> Option<String> {
        match self.sessions.verify_session(token.to_string()).await {
            Ok(pid) => pid.filter(|p| !p.is_empty()),
            Err(err) => {
                tracing::error!(%err, "gateway: session verification failed (accounts unreachable?)");
                None
            }
        }
    }
}

/// Resolves the process's [`SessionVerifier`] at gateway `init` (phase 2 — every
/// provider's phase-1 `register`, module or stub, has already run):
///
///   1. the `accounts.sessions` capability, when present → [`SessionsVerifier`];
///   2. else, `ACCOUNTS_DEV_AUTH` EXPLICITLY truthy (`1`/`true`/`on`) →
///      [`DevSessionVerifier`] with a loud warning;
///   3. else → **fail startup loudly** (review item 5: no silent dev fallback).
pub(crate) fn resolve_verifier(ctx: &Context) -> anyhow::Result<Arc<dyn SessionVerifier>> {
    if let Some(sessions) = ctx
        .registry()
        .try_require::<dyn accountsapi::Sessions>(&registry::key("accounts", "sessions"))
    {
        return Ok(Arc::new(SessionsVerifier::new(sessions)));
    }
    if dev_auth_explicitly_on() {
        tracing::warn!(
            "gateway: no accounts.sessions capability in this process; ACCOUNTS_DEV_AUTH is \
             explicitly set, so falling back to DevSessionVerifier (dev-<player_id> tokens)"
        );
        return Ok(Arc::new(DevSessionVerifier::new()));
    }
    anyhow::bail!(
        "gateway: no accounts.sessions capability is available in this process — add the \
         accounts module (monolith) or a remote::Stub::new(\"accounts\", ..., \
         accountsrpc::remote_factories()) (split), or set ACCOUNTS_DEV_AUTH=1 explicitly \
         for local dev-token auth"
    )
}

/// `true` only when `ACCOUNTS_DEV_AUTH` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` here — the accounts module's own default-ON
/// convenience does NOT extend to the gateway's auth fallback.
fn dev_auth_explicitly_on() -> bool {
    matches!(
        std::env::var("ACCOUNTS_DEV_AUTH"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

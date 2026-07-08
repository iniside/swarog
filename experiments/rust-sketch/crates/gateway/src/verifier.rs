//! The bearer-token → player_id verifier the gateway consults at its single
//! auth-once seam. A consumer-defined interface (CLAUDE.md rule 4): the gateway
//! depends on the *capability* to verify a session, not on the `accounts` module.
//!
//! M1 ships only [`DevSessionVerifier`], a loud stand-in for the real `accounts`
//! sessions service (deferred to Milestone 2). Milestone 2 swaps in a real verifier
//! (opaque DB-backed sessions / an edge-backed `accountsrpc.Client` for the split
//! front-door) that satisfies the SAME [`SessionVerifier`] trait — the gateway is
//! unchanged.

use std::sync::Once;

use async_trait::async_trait;

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
                 as a verified player. This is a Milestone-1 stand-in for `accounts` — NEVER run \
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

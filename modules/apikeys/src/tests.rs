//! Unit tests for the dev-seed shape + the `apikeys.keys` capability (the `Keys` trait
//! the gateway resolves) over the live store. The pure store CRUD lives in
//! `store_tests.rs`; here we assert the seed policy contract and that the Service maps
//! unknown/revoked keys to `Ok(None)`.

use super::*;
use crate::store::Store;
use crate::store_tests::{cleanup, test_pool, unique_name};
use apikeysapi::Keys as _;

// ---- Unit: the dev-seed policy contract (no DB) ----------------------------

/// `dev-client` carries the player-facing methods but NOT `match.report` (the
/// trusted-server op — the harness's real negative case); `dev-server` is `full`.
#[test]
fn dev_seed_policy_contract() {
    let client = DEV_SEED
        .iter()
        .find(|(name, ..)| *name == "dev-client")
        .expect("dev-client seeded");
    assert_eq!(client.1, "dev-key-client");
    assert!(client.2.split(',').any(|m| m == "accounts.login"));
    assert!(client.2.split(',').any(|m| m == "leaderboard.topScores"));
    assert!(
        !client.2.split(',').any(|m| m == "match.report"),
        "dev-client must NOT carry match.report (trusted-server op)"
    );

    let server = DEV_SEED
        .iter()
        .find(|(name, ..)| *name == "dev-server")
        .expect("dev-server seeded");
    assert_eq!((server.1, server.2), ("dev-key-server", "full"));
}

/// Every entry in the client policy is a `<prefix>.<method>` wire name (a single dot,
/// non-empty halves) — guards against a stray comma/space creeping into the const.
#[test]
fn dev_client_policy_entries_are_wire_methods() {
    for m in DEV_CLIENT_POLICY.split(',') {
        let (prefix, method) = m.split_once('.').unwrap_or_else(|| panic!("no dot in {m:?}"));
        assert!(!prefix.is_empty() && !method.is_empty(), "malformed method {m:?}");
        assert_eq!(m.trim(), m, "stray whitespace in {m:?}");
    }
}

// ---- Integration: the Keys capability over the live store ------------------

#[tokio::test]
async fn capability_lookup_known_unknown_revoked() {
    let Some(pool) = test_pool().await else { return };
    let svc = Service { store: Store { pool: pool.clone() } };
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "accounts.login").await.unwrap();

    // Known → Ok(Some(record)).
    let rec = svc.lookup_key(key.clone()).await.unwrap();
    assert_eq!(
        rec,
        Some(apikeysapi::KeyRecord { name: name.clone(), policy: "accounts.login".into() })
    );

    // Unknown → Ok(None).
    assert_eq!(svc.lookup_key(format!("{base}-nope")).await.unwrap(), None);

    // Revoked → Ok(None) (not an Err — a revoked key is a valid "no" answer).
    svc.store.revoke(&name).await.unwrap();
    assert_eq!(svc.lookup_key(key).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

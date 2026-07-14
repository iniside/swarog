//! Unit tests for the dev-seed contract (pure, over the consts) + the `apikeys.keys`
//! capability (the `Keys` trait the gateway resolves) over the live store — now hashed +
//! role-JOINed. The pure role/key store CRUD lives in `store_tests.rs`; the admin
//! configurator in `admin_tests.rs`.

use super::*;
use crate::store::Store;
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};
use apikeysapi::Keys as _;

// ---- Unit: the dev-seed contract (no DB) -----------------------------------

/// The `dev-client` ROLE carries the player-facing methods but NOT `match.report` (the
/// trusted-server op — the harness's real negative case); `dev-server` is `full`. The dev
/// KEYS map to those roles with the well-known plaintext secrets.
#[test]
fn dev_seed_role_and_key_contract() {
    let client_policy = DEV_SEED_ROLES
        .iter()
        .find(|(name, _)| *name == "dev-client")
        .expect("dev-client role seeded")
        .1;
    assert!(client_policy.split(',').any(|m| m == "accounts.login"));
    assert!(client_policy.split(',').any(|m| m == "leaderboard.topScores"));
    assert!(
        !client_policy.split(',').any(|m| m == "match.report"),
        "dev-client must NOT carry match.report (trusted-server op)"
    );

    let server_policy = DEV_SEED_ROLES
        .iter()
        .find(|(name, _)| *name == "dev-server")
        .expect("dev-server role seeded")
        .1;
    assert_eq!(server_policy, "full");

    // The keys reference those roles with the well-known plaintext secrets.
    assert_eq!(
        DEV_SEED_KEYS,
        &[
            ("dev-client", "dev-key-client", "dev-client"),
            ("dev-server", "dev-key-server", "dev-server"),
        ]
    );
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
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Service { store: Store { pool: pool.clone() } };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    svc.store.create_role(&role, "accounts.login").await.unwrap();
    let (secret, _prefix) = svc.store.create_key(&key, &role).await.unwrap();

    // Known → Ok(Some(record)) with the resolved ROLE policy.
    assert_eq!(
        svc.lookup_key(secret.clone()).await.unwrap(),
        Some(apikeysapi::KeyRecord { name: key.clone(), policy: "accounts.login".into() })
    );
    // Unknown → Ok(None).
    assert_eq!(svc.lookup_key(format!("{base}-nope")).await.unwrap(), None);
    // Revoked → Ok(None).
    let rev = svc.store.list_keys().await.unwrap().into_iter().find(|k| k.name == key).unwrap().revision;
    svc.store.revoke_key(&key, rev).await.unwrap();
    assert_eq!(svc.lookup_key(secret).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

/// The dev-seed MECHANISM (test-prefixed, never touching the shared harness rows): a KNOWN
/// plaintext secret resolves via its stored SHA-256 digest to the role's policy — the same
/// path `X-Api-Key: dev-key-server` takes (splitproof K3/K4 + smoke depend on it).
#[tokio::test]
async fn dev_seed_mechanism_known_secret_resolves_role_policy() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Service { store: Store { pool: pool.clone() } };
    let base = unique_name(&pool).await;
    let server_role = format!("{base}-server");
    let client_role = format!("{base}-client");
    let server_key = format!("{base}-server-key");
    let client_key = format!("{base}-client-key");
    let server_secret = format!("{base}-dev-key-server");
    let client_secret = format!("{base}-dev-key-client");

    svc.store.upsert_seed_role(&server_role, "full").await.unwrap();
    svc.store.upsert_seed_role(&client_role, DEV_CLIENT_POLICY).await.unwrap();
    svc.store.upsert_seed_key(&server_key, &server_secret, &server_role).await.unwrap();
    svc.store.upsert_seed_key(&client_key, &client_secret, &client_role).await.unwrap();

    // The server key resolves to `full`.
    assert_eq!(svc.lookup_key(server_secret).await.unwrap().unwrap().policy, "full");
    // The client key resolves and does NOT authorize match.report.
    let client = svc.lookup_key(client_secret).await.unwrap().unwrap();
    assert!(!client.policy.split(',').any(|m| m == "match.report"));

    cleanup(&pool, &base).await;
}

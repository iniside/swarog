//! Live-Postgres store tests (the local DB is the test DB): the normalized role+key
//! CRUD, hashed lookup + the role JOIN, CAS-by-revision (proving the STALE branch does
//! NOT write), the FK authority (delete-role-in-use, create-key-missing-role), and the
//! self-healing seed. Every fixture uses a `test-`-prefixed, per-test-unique name base
//! and deletes its own rows (keys before roles — FK order), so the shared local Postgres
//! never has the harness's `dev-client`/`dev-server` rows touched.

use super::*;
use crate::store::{Store, WriteError};
use sqlx::PgPool;
use std::time::Duration;

/// Fallback DSN when `DATABASE_URL` is unset (matches the other modules' tests).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn db_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    DB_TEST_LOCK.lock().await
}

/// Opens the local Postgres, migrates the apikeys schema, and returns `None` (printing a
/// skip line) when it's unreachable.
pub(crate) async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — apikeys DB tests skipped");
            return None;
        }
    };
    if let Err(err) = sqlx::raw_sql(SCHEMA_DDL).execute(&pool).await {
        eprintln!("SKIP: apikeys migrate failed: {err}");
        return None;
    }
    Some(pool)
}

/// A fresh, per-test-unique `test-…` name base.
pub(crate) async fn unique_name(pool: &PgPool) -> String {
    let (n,): (String,) =
        sqlx::query_as("SELECT 'test-' || replace(gen_random_uuid()::text, '-', '')")
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

/// Removes every row a test created under its unique base — keys BEFORE roles (FK order).
pub(crate) async fn cleanup(pool: &PgPool, base: &str) {
    let like = format!("{base}%");
    let _ = sqlx::query("DELETE FROM apikeys.keys WHERE name LIKE $1")
        .bind(&like)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM apikeys.roles WHERE name LIKE $1")
        .bind(&like)
        .execute(pool)
        .await;
}

/// The `revision` of a role, for asserting a CAS write did (or did NOT) happen.
async fn role_revision(pool: &PgPool, name: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT revision FROM apikeys.roles WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn role_policy(store: &Store, name: &str) -> Option<String> {
    store
        .list_roles()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .map(|r| r.policy)
}

async fn key_summary(store: &Store, name: &str) -> Option<crate::store::KeySummary> {
    store
        .list_keys()
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.name == name)
}

fn assert_conflict<T: std::fmt::Debug>(result: Result<T, WriteError>) {
    assert!(
        matches!(result, Err(WriteError::Conflict(_))),
        "expected WriteError::Conflict, got {result:?}"
    );
}

// ---- Lookup + the role JOIN ------------------------------------------------

#[tokio::test]
async fn lookup_known_unknown_revoked() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "full").await.unwrap();
    let (secret, _prefix) = store.create_key(&key, &role).await.unwrap();

    // Known → the record with the resolved ROLE policy.
    assert_eq!(
        store.lookup(&secret).await.unwrap(),
        Some(apikeysapi::KeyRecord { name: key.clone(), policy: "full".into() })
    );
    // Unknown → None.
    assert_eq!(store.lookup(&format!("{base}-nope")).await.unwrap(), None);

    // Revoked → None (row stays; lookup ignores it).
    let rev = key_summary(&store, &key).await.unwrap().revision;
    store.revoke_key(&key, rev).await.unwrap();
    assert_eq!(store.lookup(&secret).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

/// The JOIN is the effective-policy authority: editing the ROLE's policy changes what
/// `lookup` returns for a key referencing it, with no touch to the key row.
#[tokio::test]
async fn role_policy_edit_changes_effective_key_policy() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "accounts.login").await.unwrap();
    let (secret, _p) = store.create_key(&key, &role).await.unwrap();
    assert_eq!(store.lookup(&secret).await.unwrap().unwrap().policy, "accounts.login");

    // Edit the role policy under its current revision → lookup reflects it immediately.
    let rev = role_revision(&pool, &role).await.unwrap();
    store.set_role_policy(&role, rev, "full").await.unwrap();
    assert_eq!(store.lookup(&secret).await.unwrap().unwrap().policy, "full");
    // The role revision advanced by exactly one.
    assert_eq!(role_revision(&pool, &role).await.unwrap(), rev + 1);

    cleanup(&pool, &base).await;
}

// ---- CAS: the STALE branch must NOT write ----------------------------------

#[tokio::test]
async fn stale_revision_set_role_policy_conflicts_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    store.create_role(&role, "accounts.login").await.unwrap();
    let rev = role_revision(&pool, &role).await.unwrap();

    // A wrong (stale) expected revision → Conflict, and the row is unchanged.
    assert_conflict(store.set_role_policy(&role, rev + 999, "full").await);
    assert_eq!(role_policy(&store, &role).await.unwrap(), "accounts.login");
    assert_eq!(role_revision(&pool, &role).await.unwrap(), rev, "no write on a stale CAS");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn stale_revision_set_key_role_conflicts_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let r1 = format!("{base}-r1");
    let r2 = format!("{base}-r2");
    let key = format!("{base}-key");
    store.create_role(&r1, "full").await.unwrap();
    store.create_role(&r2, "accounts.login").await.unwrap();
    store.create_key(&key, &r1).await.unwrap();
    let before = key_summary(&store, &key).await.unwrap();

    assert_conflict(store.set_key_role(&key, before.revision + 999, &r2).await);
    let after = key_summary(&store, &key).await.unwrap();
    assert_eq!(after.role, r1, "role unchanged on a stale CAS");
    assert_eq!(after.revision, before.revision, "no write on a stale CAS");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn stale_revision_revoke_conflicts_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "full").await.unwrap();
    let (secret, _p) = store.create_key(&key, &role).await.unwrap();
    let before = key_summary(&store, &key).await.unwrap();

    assert_conflict(store.revoke_key(&key, before.revision + 999).await);
    assert!(!key_summary(&store, &key).await.unwrap().revoked, "not revoked on a stale CAS");
    assert!(store.lookup(&secret).await.unwrap().is_some(), "still resolves — no write");

    cleanup(&pool, &base).await;
}

// ---- FK authority ----------------------------------------------------------

#[tokio::test]
async fn delete_role_in_use_conflicts_and_keeps_the_role() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "full").await.unwrap();
    store.create_key(&key, &role).await.unwrap();
    let rev = role_revision(&pool, &role).await.unwrap();

    // The FK (not a pre-check) rejects deleting a role a key still references.
    assert_conflict(store.delete_role(&role, rev).await);
    assert!(role_policy(&store, &role).await.is_some(), "role kept — still in use");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn delete_role_unused_succeeds() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    store.create_role(&role, "full").await.unwrap();
    let rev = role_revision(&pool, &role).await.unwrap();

    store.delete_role(&role, rev).await.unwrap();
    assert!(role_policy(&store, &role).await.is_none(), "role gone");

    cleanup(&pool, &base).await;
}

/// A domain-missing target on create_key (nonexistent role) is a `Conflict` via the FK —
/// NOT a not-found (finding #2: NotFound would read as the edge's UnknownMethod).
#[tokio::test]
async fn create_key_missing_role_conflicts_not_notfound() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let key = format!("{base}-key");

    assert_conflict(store.create_key(&key, &format!("{base}-no-such-role")).await);
    assert!(key_summary(&store, &key).await.is_none(), "no key written");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn set_key_role_missing_role_conflicts() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "full").await.unwrap();
    store.create_key(&key, &role).await.unwrap();
    let rev = key_summary(&store, &key).await.unwrap().revision;

    assert_conflict(store.set_key_role(&key, rev, &format!("{base}-nope")).await);
    assert_eq!(key_summary(&store, &key).await.unwrap().role, role);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn create_role_duplicate_conflicts() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    store.create_role(&role, "full").await.unwrap();
    assert_conflict(store.create_role(&role, "accounts.login").await);
    assert_eq!(role_policy(&store, &role).await.unwrap(), "full", "first policy kept");

    cleanup(&pool, &base).await;
}

// ---- Secret secrecy: minted once, never in a read --------------------------

#[tokio::test]
async fn create_key_returns_secret_once_and_reads_never_hold_it() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    store.create_role(&role, "full").await.unwrap();
    let (secret, prefix) = store.create_key(&key, &role).await.unwrap();

    assert!(secret.starts_with("ak_"), "secret shape: {secret}");
    assert_eq!(prefix, secret.chars().take(12).collect::<String>(), "prefix is the first 12 chars");
    assert!(secret.len() <= apikeysapi::MAX_KEY_BYTES);

    // The stored column holds the DIGEST, never the plaintext.
    let stored_hash: String = sqlx::query_scalar("SELECT secret_hash FROM apikeys.keys WHERE name = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(stored_hash, secret, "secret is never stored in cleartext");
    assert_eq!(stored_hash, crate::store::secret_hash(&secret), "stored digest == sha256(secret)");

    // The list summary is structurally secret-free (only the prefix), and resolves.
    let row = key_summary(&store, &key).await.unwrap();
    assert_eq!(row.prefix, prefix);
    assert_eq!(store.lookup(&secret).await.unwrap().unwrap().name, key);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn invalid_policy_rejected() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");

    assert!(matches!(store.create_role(&role, "   ").await, Err(WriteError::Invalid(_))));
    assert!(matches!(store.create_role(&role, "a,,b").await, Err(WriteError::Invalid(_))));
    assert!(role_policy(&store, &role).await.is_none(), "no role written on invalid policy");

    cleanup(&pool, &base).await;
}

// ---- Seed mechanism (test-prefixed; never touches the shared dev rows) ------

#[tokio::test]
async fn seed_upsert_roles_and_keys_self_heal() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    let secret = format!("{base}-secret");

    store.upsert_seed_role(&role, "full").await.unwrap();
    store.upsert_seed_key(&key, &secret, &role).await.unwrap();
    assert_eq!(store.lookup(&secret).await.unwrap().unwrap().policy, "full");

    // A stray revoke + role drift on the shared dev DB, then re-seed self-heals.
    let rev = key_summary(&store, &key).await.unwrap().revision;
    store.revoke_key(&key, rev).await.unwrap();
    store.set_role_policy(&role, role_revision(&pool, &role).await.unwrap(), "accounts.login").await.unwrap();
    assert!(store.lookup(&secret).await.unwrap().is_none(), "revoked before re-seed");

    // Capture the CAS tokens right before the re-seed to prove they advance.
    let role_rev_before = role_revision(&pool, &role).await.unwrap();
    let key_rev_before = key_summary(&store, &key).await.unwrap().revision;

    store.upsert_seed_role(&role, "full").await.unwrap();
    store.upsert_seed_key(&key, &secret, &role).await.unwrap();
    assert_eq!(
        store.lookup(&secret).await.unwrap().unwrap().policy,
        "full",
        "re-seed clears revoked_at and restores the role policy"
    );
    // Idempotent — still exactly one key row.
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM apikeys.keys WHERE name = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // FOLD (Step 7): a re-seed BUMPS the CAS revision on both rows, so a form rendered
    // against the pre-seed revision now correctly conflicts instead of clobbering the
    // freshly-seeded state.
    assert_eq!(
        role_revision(&pool, &role).await.unwrap(),
        role_rev_before + 1,
        "re-seed advances the role CAS token"
    );
    assert_eq!(
        key_summary(&store, &key).await.unwrap().revision,
        key_rev_before + 1,
        "re-seed advances the key CAS token"
    );

    cleanup(&pool, &base).await;
}

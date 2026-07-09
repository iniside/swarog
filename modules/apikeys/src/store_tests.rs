//! Live-Postgres store tests (the local DB is the test DB): CRUD, lookup
//! known/unknown/revoked, policy edits, and the self-healing seed upsert. Every fixture
//! uses a `test-`-prefixed, per-test-unique key name and deletes its own rows, so the
//! shared local Postgres never has the harness's `dev-client`/`dev-server` rows touched.

use super::*;
use crate::store::Store;
use sqlx::PgPool;
use std::time::Duration;

/// Fallback DSN when `DATABASE_URL` is unset (matches the other modules' tests).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

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

/// A fresh, per-test-unique `test-…` name base (uuid hyphens stripped so it also serves
/// as a valid key string).
pub(crate) async fn unique_name(pool: &PgPool) -> String {
    let (n,): (String,) =
        sqlx::query_as("SELECT 'test-' || replace(gen_random_uuid()::text, '-', '')")
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

/// Removes every row a test created under its unique base (name AND the key column,
/// which we set equal to the base).
pub(crate) async fn cleanup(pool: &PgPool, base: &str) {
    let _ = sqlx::query("DELETE FROM apikeys.keys WHERE name LIKE $1 OR key LIKE $1")
        .bind(format!("{base}%"))
        .execute(pool)
        .await;
}

#[tokio::test]
async fn lookup_known_unknown_revoked() {
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    store.insert(&name, &key, "full").await.unwrap();

    // Known → the record.
    let rec = store.lookup(&key).await.unwrap();
    assert_eq!(
        rec,
        Some(apikeysapi::KeyRecord { name: name.clone(), policy: "full".into() })
    );

    // Unknown → None.
    assert_eq!(store.lookup(&format!("{base}-nope")).await.unwrap(), None);

    // Revoked → None (the row stays, but lookup ignores it).
    store.revoke(&name).await.unwrap();
    assert_eq!(store.lookup(&key).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn policy_crud_and_list() {
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    store.insert(&name, &key, "accounts.login").await.unwrap();

    // set_policy replaces the string; lookup reflects it.
    store.set_policy(&name, "full").await.unwrap();
    assert_eq!(store.lookup(&key).await.unwrap().unwrap().policy, "full");

    // list surfaces the row with its secret, policy and revoked flag.
    let rows = store.list().await.unwrap();
    let row = rows.iter().find(|r| r.name == name).expect("row present in list");
    assert_eq!(row.key, key);
    assert_eq!(row.policy, "full");
    assert!(!row.revoked);

    // revoke flips the flag in list.
    store.revoke(&name).await.unwrap();
    let rows = store.list().await.unwrap();
    let row = rows.iter().find(|r| r.name == name).unwrap();
    assert!(row.revoked);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn seed_upsert_is_idempotent() {
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let name = format!("{base}-seed");
    let key = format!("{base}-key");

    store.upsert_seed(&name, &key, "full").await.unwrap();
    store.upsert_seed(&name, &key, "full").await.unwrap();

    // Exactly one row, and it's the seeded one.
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM apikeys.keys WHERE name = $1")
        .bind(&name)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "seed upsert must not duplicate the row");
    assert_eq!(store.lookup(&key).await.unwrap().unwrap().policy, "full");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn seed_upsert_self_heals_revoke_and_policy() {
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let name = format!("{base}-seed");
    let key = format!("{base}-key");

    // Seed, then a stray revoke + policy drift on the shared dev DB.
    store.upsert_seed(&name, &key, "full").await.unwrap();
    store.revoke(&name).await.unwrap();
    store.set_policy(&name, "accounts.login").await.unwrap();
    assert_eq!(store.lookup(&key).await.unwrap(), None, "revoked before re-seed");

    // Re-running the seed un-revokes AND restores the policy (the migrate self-heal).
    store.upsert_seed(&name, &key, "full").await.unwrap();
    let rec = store.lookup(&key).await.unwrap();
    assert_eq!(
        rec,
        Some(apikeysapi::KeyRecord { name: name.clone(), policy: "full".into() }),
        "seed upsert must clear revoked_at and restore policy"
    );

    cleanup(&pool, &base).await;
}

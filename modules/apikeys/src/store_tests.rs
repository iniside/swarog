//! Live-Postgres store tests (the local DB is the test DB): the normalized role+key
//! CRUD, hashed lookup + the role JOIN, CAS-by-revision (proving the STALE branch does
//! NOT write), the FK authority (delete-role-in-use, create-key-missing-role), and the
//! self-healing seed. Every fixture uses a `test-`-prefixed, per-test-unique name base
//! and deletes its own rows (keys before roles — FK order), so the shared local Postgres
//! never has the harness's `dev-client`/`dev-server` rows touched.

use super::*;
use crate::store::{KeyStatus, Store, WriteError};
use sqlx::PgPool;

static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn db_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    DB_TEST_LOCK.lock().await
}

/// The apikeys schema on top of the shared pool.
pub(crate) async fn test_pool() -> Option<PgPool> {
    let pool = testdb::test_pool().await?;
    ddl(&pool).run().await.expect("migrate apikeys schema");
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
        .roles_with_counts()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .map(|r| r.policy)
}

async fn key_summary(store: &Store, name: &str) -> Option<crate::store::KeyRow> {
    store
        .keys_page(Some(name), None, None, None, 1000)
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

/// Asserts the operator-input verdict AND that its message names the offending field —
/// the whole point of checking the cap in Rust rather than leaving it to the column CHECK.
fn assert_invalid<T: std::fmt::Debug>(result: Result<T, WriteError>, names: &str) {
    match result {
        Err(WriteError::Invalid(msg)) => assert!(
            msg.contains(names),
            "expected the rejection to name {names:?}, got {msg:?}"
        ),
        other => panic!("expected WriteError::Invalid, got {other:?}"),
    }
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

/// An over-cap policy string is an `Invalid` (never Conflict/NotFound) and writes nothing
/// — bounding the string that rides every gateway key-lookup response + 5s cache. Covers
/// BOTH policy write paths: create_role (no row created) and set_role_policy (the
/// existing policy preserved).
#[tokio::test]
async fn over_cap_policy_rejected_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    // A single comma-free method name well past the 4 KiB cap (loose-valid but too big).
    let huge = "a".repeat(9000);

    // create_role rejects it and creates no row.
    assert!(matches!(store.create_role(&role, &huge).await, Err(WriteError::Invalid(_))));
    assert!(role_policy(&store, &role).await.is_none(), "no role written on over-cap policy");

    // set_role_policy rejects it and preserves the existing policy.
    store.create_role(&role, "full").await.unwrap();
    let rev = role_revision(&pool, &role).await.unwrap();
    assert!(matches!(store.set_role_policy(&role, rev, &huge).await, Err(WriteError::Invalid(_))));
    assert_eq!(role_policy(&store, &role).await.unwrap(), "full", "policy unchanged on over-cap edit");

    cleanup(&pool, &base).await;
}

/// An over-cap role/key NAME is an `Invalid` naming the field (never Conflict/Db) on
/// EVERY writer that binds one — as an inserted value, as an updated value, and as a
/// `WHERE` predicate — and nothing is written.
#[tokio::test]
async fn over_cap_name_rejected_on_every_writer_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    let huge = format!("{base}{}", "a".repeat(crate::store::MAX_NAME_BYTES + 1 - base.len()));
    assert_eq!(huge.len(), crate::store::MAX_NAME_BYTES + 1);

    store.create_role(&role, "full").await.unwrap();
    let role_rev = role_revision(&pool, &role).await.unwrap();
    store.create_key(&key, &role).await.unwrap();
    let key_rev = key_summary(&store, &key).await.unwrap().revision;

    // Inserted values.
    assert_invalid(store.create_role(&huge, "full").await, "role name");
    assert_invalid(store.create_key(&huge, &role).await, "key name");
    assert_invalid(store.create_key(&format!("{base}-k2"), &huge).await, "role name");
    // Updated values.
    assert_invalid(store.set_key_role(&key, key_rev, &huge).await, "role name");
    // `WHERE` predicates.
    assert_invalid(store.set_role_policy(&huge, role_rev, "full").await, "role name");
    assert_invalid(store.delete_role(&huge, role_rev).await, "role name");
    assert_invalid(store.revoke_key(&huge, key_rev).await, "key name");
    assert_invalid(store.set_key_role(&huge, key_rev, &role).await, "key name");

    assert!(role_policy(&store, &huge).await.is_none(), "no role written under an over-cap name");
    assert!(key_summary(&store, &huge).await.is_none(), "no key written under an over-cap name");
    let row = key_summary(&store, &key).await.unwrap();
    assert_eq!((row.role.as_str(), row.revision), (role.as_str(), key_rev), "the live key is untouched");
    assert_eq!(role_revision(&pool, &role).await.unwrap(), role_rev, "the live role is untouched");

    cleanup(&pool, &base).await;
}

/// A name of EXACTLY the cap is accepted end-to-end — the ceiling is inclusive, so the
/// Rust check and the `octet_length(...) <= 128` CHECK agree on the boundary value.
#[tokio::test]
async fn at_cap_name_is_accepted_end_to_end() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let pad = crate::store::MAX_NAME_BYTES - base.len();
    let role = format!("{base}{}", "r".repeat(pad));
    let key = format!("{base}{}", "k".repeat(pad));
    assert_eq!((role.len(), key.len()), (crate::store::MAX_NAME_BYTES, crate::store::MAX_NAME_BYTES));

    store.create_role(&role, "full").await.unwrap();
    let (secret, _prefix) = store.create_key(&key, &role).await.unwrap();
    assert_eq!(store.lookup(&secret).await.unwrap().unwrap().name, key);

    cleanup(&pool, &base).await;
}

/// The DB-level fail-safe under the Rust caps: a writer that does NOT call
/// `validate_name`/`validate_policy` (the dev-seed upserts, whose inputs are compile-time
/// consts) still cannot store an over-cap value, and the named 23514 the column CHECK
/// raises classifies as `Invalid` — the same verdict the Rust cap gives — never as store
/// trouble. An UNMAPPED 23514 stays `Db`: that arm is deliberate.
#[tokio::test]
async fn column_checks_backstop_the_caps_and_map_to_the_same_verdict() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let huge_name = format!("{base}{}", "a".repeat(crate::store::MAX_NAME_BYTES + 1 - base.len()));
    let huge_policy = "a".repeat(crate::store::MAX_POLICY_BYTES + 1);

    store.upsert_seed_role(&base, "full").await.unwrap();

    let err = store
        .upsert_seed_role(&huge_name, "full")
        .await
        .expect_err("roles_name_len_check must reject an over-cap name");
    let db = err.as_database_error().expect("a database error");
    assert_eq!(db.code().as_deref(), Some("23514"));
    assert_eq!(db.constraint(), Some("roles_name_len_check"));
    assert_invalid(Err::<(), _>(WriteError::from_db(err, "unused")), "role name");

    let err = store
        .upsert_seed_role(&base, &huge_policy)
        .await
        .expect_err("roles_policy_len_check must reject an over-cap policy");
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("roles_policy_len_check"));
    assert_invalid(Err::<(), _>(WriteError::from_db(err, "unused")), "role policy");

    let err = store
        .upsert_seed_key(&huge_name, "s", &base)
        .await
        .expect_err("keys_name_len_check must reject an over-cap name");
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("keys_name_len_check"));
    assert_invalid(Err::<(), _>(WriteError::from_db(err, "unused")), "key name");

    // A 23514 from a constraint the cap table does not name stays store trouble.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "CREATE TEMP TABLE apikeys_unmapped_probe \
           (v int CONSTRAINT probe_unmapped_check CHECK (v > 0))",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let err = sqlx::query("INSERT INTO apikeys_unmapped_probe VALUES (0)")
        .execute(&mut *conn)
        .await
        .expect_err("the probe CHECK must reject 0");
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("probe_unmapped_check"));
    assert!(
        matches!(WriteError::from_db(err, "unused"), WriteError::Db(_)),
        "an unmapped 23514 must stay store trouble"
    );
    sqlx::raw_sql("DROP TABLE apikeys_unmapped_probe").execute(&mut *conn).await.unwrap();

    assert!(role_policy(&store, &huge_name).await.is_none());
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

/// Every key of a test, walked page by page through the keyset cursor.
async fn walk_pages(store: &Store, needle: &str, page: i64) -> Vec<crate::store::KeyRow> {
    let mut all = Vec::new();
    let mut after: Option<(i64, String)> = None;
    loop {
        let rows = store
            .keys_page(Some(needle), None, None, after.as_ref().map(|(us, n)| (*us, n.as_str())), page)
            .await
            .unwrap();
        let done = (rows.len() as i64) < page;
        after = rows.last().map(|r| (r.created_us, r.name.clone()));
        all.extend(rows);
        if done {
            return all;
        }
    }
}

#[tokio::test]
async fn keys_page_keyset_walks_every_key_once_newest_first() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    store.create_role(&role, "full").await.unwrap();
    for i in 0..27 {
        store.create_key(&format!("{base}-k{i:02}"), &role).await.unwrap();
    }
    // k16 shares k17's created_at, so k17 ends page 1 and k16 starts page 2 only via the name half of the cursor.
    sqlx::query("UPDATE apikeys.keys SET created_at = (SELECT created_at FROM apikeys.keys WHERE name = $1) WHERE name = $2")
        .bind(format!("{base}-k17"))
        .bind(format!("{base}-k16"))
        .execute(&pool)
        .await
        .unwrap();

    let walked = walk_pages(&store, &base, 10).await;
    let mut expected: Vec<(i64, String)> = sqlx::query_as(
        "SELECT (EXTRACT(EPOCH FROM created_at) * 1000000)::int8, name FROM apikeys.keys WHERE name LIKE $1",
    )
    .bind(format!("{base}%"))
    .fetch_all(&pool)
    .await
    .unwrap();
    expected.sort_by(|a, b| b.cmp(a));
    let got: Vec<(i64, String)> = walked.iter().map(|r| (r.created_us, r.name.clone())).collect();
    assert_eq!(got, expected, "every key exactly once, (created_at, name) DESC");
    assert_eq!(got.len(), 27);
    let first = &walked[0];
    assert_eq!(first.role, role);
    assert!(!first.revoked);
    assert!(first.revision >= 0);
    let c = first.created.as_bytes();
    assert!(c.len() == 10 && c[4] == b'-' && c[7] == b'-', "YYYY-MM-DD: {}", first.created);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn keys_page_filters_by_status_role_and_literal_needle() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let role_a = format!("{base}-ra");
    let role_b = format!("{base}-rb");
    store.create_role(&role_a, "full").await.unwrap();
    store.create_role(&role_b, "full").await.unwrap();
    let live = format!("{base}-live%_x");
    let gone = format!("{base}-gone");
    let other = format!("{base}-other");
    let (_, live_prefix) = store.create_key(&live, &role_a).await.unwrap();
    store.create_key(&gone, &role_a).await.unwrap();
    store.create_key(&other, &role_b).await.unwrap();
    let rev = key_summary(&store, &gone).await.unwrap().revision;
    store.revoke_key(&gone, rev).await.unwrap();

    let names = |rows: Vec<crate::store::KeyRow>| {
        let mut n: Vec<String> = rows.into_iter().map(|r| r.name).collect();
        n.sort();
        n
    };
    let page = |status, role: Option<String>, needle: String| {
        let store = &store;
        async move { store.keys_page(Some(&needle), status, role.as_deref(), None, 100).await.unwrap() }
    };
    let mut all = vec![gone.clone(), live.clone(), other.clone()];
    all.sort();
    assert_eq!(names(page(None, None, base.clone()).await), all);
    assert_eq!(names(page(Some(KeyStatus::Revoked), None, base.clone()).await), vec![gone.clone()]);
    assert_eq!(
        names(page(Some(KeyStatus::Active), None, base.clone()).await),
        vec![live.clone(), other.clone()]
    );
    assert_eq!(names(page(None, Some(role_b.clone()), base.clone()).await), vec![other.clone()]);
    assert_eq!(
        names(page(Some(KeyStatus::Active), Some(role_a.clone()), base.clone()).await),
        vec![live.clone()]
    );
    // % and _ are literal: a LIKE would match every key here.
    assert_eq!(names(page(None, None, format!("{base}-LIVE%_")).await), vec![live.clone()]);
    assert!(page(None, None, format!("{base}-%")).await.is_empty(), "% is not a wildcard");
    assert!(page(None, None, format!("{base}_")).await.is_empty(), "_ is not a wildcard");
    // The prefix matches too, case-insensitively.
    let by_prefix = store.keys_page(Some(&live_prefix.to_uppercase()), None, None, None, 100).await.unwrap();
    assert!(by_prefix.iter().any(|r| r.name == live && r.prefix == live_prefix));

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn role_counts_include_revoked_keys_and_key_counts_split_active() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let store = Store { pool: pool.clone() };
    let base = unique_name(&pool).await;
    let used = format!("{base}-used");
    let empty = format!("{base}-empty");
    store.create_role(&used, "full").await.unwrap();
    store.create_role(&empty, "full").await.unwrap();
    let (total0, active0) = store.key_counts().await.unwrap();
    let k1 = format!("{base}-k1");
    store.create_key(&k1, &used).await.unwrap();
    store.create_key(&format!("{base}-k2"), &used).await.unwrap();
    let rev = key_summary(&store, &k1).await.unwrap().revision;
    store.revoke_key(&k1, rev).await.unwrap();

    let roles: Vec<_> = store
        .roles_with_counts()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.name.starts_with(&base))
        .collect();
    let got: Vec<(&str, i64)> = roles.iter().map(|r| (r.name.as_str(), r.keys)).collect();
    assert_eq!(got, vec![(empty.as_str(), 0), (used.as_str(), 2)], "name-ordered, revoked counted");
    assert_eq!(roles[1].revision, role_revision(&pool, &used).await.unwrap());
    assert_eq!(roles[1].policy, "full");
    assert_eq!(store.key_counts().await.unwrap(), (total0 + 2, active0 + 1));

    cleanup(&pool, &base).await;
}

// ---------------------------------------------------------------------------
// The cap table vs the DDL — pure, no Postgres.
// ---------------------------------------------------------------------------

/// The one whitespace-normalized `CONSTRAINT <name> CHECK (...)` clause `SCHEMA_DDL`
/// declares, or a panic naming the missing constraint.
fn ddl_clause(constraint: &str) -> String {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let needle = format!("CONSTRAINT {constraint} ");
    let start = flat.find(&needle).unwrap_or_else(|| {
        panic!(
            "SCHEMA_DDL declares no `CONSTRAINT {constraint}` — store::COLUMN_CAPS names a \
             constraint the schema never creates, so nothing backstops that column and a \
             23514 could never map back to it"
        )
    });
    let rest = &flat[start..];
    let end = ["),", ");"]
        .iter()
        .filter_map(|terminator| rest.find(terminator))
        .min()
        .map(|index| index + 1)
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// Each byte ceiling is stated in TWO languages — `store::COLUMN_CAPS` in Rust and an
/// `octet_length(...) <= N` CHECK in `SCHEMA_DDL`. Raise the const alone and a legitimate
/// value passes Rust, dies on the column CHECK, and `WriteError::from_db` maps the 23514
/// back through the SAME table, reporting the NEW number for the OLD limit — a lying
/// message. Pure and always-runs: the only other thing pinning the pair is a live-Postgres
/// test, which is simply absent when the DB is.
#[test]
fn every_column_cap_matches_its_check_constraint_in_the_ddl() {
    for cap in crate::store::COLUMN_CAPS {
        let clause = ddl_clause(cap.constraint);
        assert!(
            clause.ends_with(&format!("<= {})", cap.max_bytes)),
            "{}: Rust caps the {} at {} bytes, but SCHEMA_DDL says `{clause}`",
            cap.constraint,
            cap.what,
            cap.max_bytes
        );
        assert!(
            clause.contains("octet_length("),
            "{}: the CHECK must count OCTETS (the Rust twin is str::len), got `{clause}`",
            cap.constraint
        );
    }
}

/// The reverse leg: a `*_len_check` in the DDL that no `ColumnCap` names would fire as an
/// UNMAPPED 23514, which `WriteError::from_db` deliberately keeps as `Db` — operator input
/// reported as store trouble.
#[test]
fn every_len_check_in_the_ddl_is_mapped_by_a_column_cap() {
    let flat = SCHEMA_DDL.split_whitespace().collect::<Vec<_>>().join(" ");
    let declared: Vec<&str> = flat
        .match_indices("CONSTRAINT ")
        .map(|(index, marker)| {
            flat[index + marker.len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
        })
        .filter(|name| name.ends_with("_len_check"))
        .collect();
    assert!(!declared.is_empty(), "SCHEMA_DDL declares no length CHECK");
    for name in declared {
        assert!(
            crate::store::COLUMN_CAPS
                .iter()
                .any(|cap| cap.constraint == name),
            "SCHEMA_DDL declares `CONSTRAINT {name}` that store::COLUMN_CAPS does not name — \
             its 23514 stays an unmapped Db error instead of the operator-input verdict"
        );
    }
}

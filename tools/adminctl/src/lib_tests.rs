//! Live-Postgres tests for the admin-user verbs (house `test_pool()` pattern — SKIP
//! cleanly when Postgres is unreachable). They run against the REAL `admin` schema (the
//! DDL is idempotent, so `ensure_schema` covers the fresh-DB installer precondition
//! without dropping the shared schema other tests share); every test uses a per-run
//! unique username and deletes its own rows, so concurrent runs never collide.

use super::*;
use std::time::Duration;

async fn test_pool() -> Option<PgPool> {
    let dsn = dsn();
    match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => Some(p),
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — adminctl tests skipped");
            None
        }
    }
}

/// A per-call unique username (nanos + pid derived) so concurrent test binaries never
/// contend on the same `admin.users` PK.
fn unique_username(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{}-{}", std::process::id(), nanos)
}

/// Reads back the stored `pass_hash` for a username (test-only introspection).
async fn stored_hash(pool: &PgPool, username: &str) -> Option<String> {
    sqlx::query_scalar("SELECT pass_hash FROM admin.users WHERE username = $1")
        .bind(username)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn create_user_stores_verifiable_hash() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let user = unique_username("adminctl-create");

    let inserted = create_user(&pool, &user, "s3cret-pw").await.unwrap();
    assert!(inserted, "first create must report a new row");

    let hash = stored_hash(&pool, &user).await.expect("user row exists");
    // The stored hash is the module's argon2id PHC string, verifiable by the module's
    // own verify_password — the installer and login path share one implementation.
    assert!(admin::verify_password(&hash, "s3cret-pw"), "correct password verifies");
    assert!(!admin::verify_password(&hash, "wrong-pw"), "wrong password rejected");

    delete_user(&pool, &user).await.unwrap();
}

#[tokio::test]
async fn create_user_upsert_resets_password() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let user = unique_username("adminctl-reset");

    assert!(create_user(&pool, &user, "first-pw").await.unwrap());
    let first = stored_hash(&pool, &user).await.unwrap();

    // Re-running create-user for an existing user is a password RESET, not an insert.
    let inserted = create_user(&pool, &user, "second-pw").await.unwrap();
    assert!(!inserted, "second create must report a reset, not a new row");

    let second = stored_hash(&pool, &user).await.unwrap();
    assert_ne!(first, second, "reset stores a fresh hash");
    assert!(admin::verify_password(&second, "second-pw"), "new password verifies");
    assert!(!admin::verify_password(&second, "first-pw"), "old password no longer works");

    delete_user(&pool, &user).await.unwrap();
}

#[tokio::test]
async fn delete_removes_user() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let user = unique_username("adminctl-delete");

    create_user(&pool, &user, "pw").await.unwrap();
    assert!(stored_hash(&pool, &user).await.is_some(), "user exists before delete");

    assert!(delete_user(&pool, &user).await.unwrap(), "delete reports removal");
    assert!(stored_hash(&pool, &user).await.is_none(), "user gone after delete");

    // A second delete is a clean no-op that reports false (not an error).
    assert!(!delete_user(&pool, &user).await.unwrap(), "re-delete reports no such user");
}

#[tokio::test]
async fn ensure_schema_is_idempotent_for_fresh_db() {
    let Some(pool) = test_pool().await else {
        return;
    };
    // The installer precondition: create-user on a DB where nothing booted the module.
    // `ensure_schema` runs the same idempotent DDL the module's migrate() runs, so a
    // repeat call (and the schema already existing) is a clean no-op.
    ensure_schema(&pool).await.unwrap();
    ensure_schema(&pool).await.unwrap();

    // And create-user succeeds straight after, proving the table is usable.
    let user = unique_username("adminctl-fresh");
    assert!(create_user(&pool, &user, "pw").await.unwrap());
    delete_user(&pool, &user).await.unwrap();
}

#[tokio::test]
async fn create_user_rejects_empty_password() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let user = unique_username("adminctl-empty");
    let err = create_user(&pool, &user, "").await.unwrap_err();
    assert!(err.to_string().contains("password"), "empty password is rejected: {err}");
    // No row should have been created.
    assert!(stored_hash(&pool, &user).await.is_none(), "no user row for a rejected create");
}

/// The zombie-account regression: `create_user` must normalize (trim) the raw argv
/// username before binding, so the stored row matches what the login handler's own
/// trim+cap would look up — never a padded row the login path can never reach.
#[tokio::test]
async fn create_user_trims_padded_username() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let bare = unique_username("adminctl-pad");
    let padded = format!("  {bare}  ");

    let inserted = create_user(&pool, &padded, "pw").await.unwrap();
    assert!(inserted, "first create must report a new row");

    // Stored under the TRIMMED name, not the raw padded argv.
    assert!(stored_hash(&pool, &bare).await.is_some(), "row stored under the trimmed username");
    assert!(stored_hash(&pool, &padded).await.is_none(), "no row stored under the padded username");

    delete_user(&pool, &bare).await.unwrap();
}

/// `create_user` rejects a username over the 128-byte cap — same authority the
/// login handler enforces, applied to the TRIMMED value.
#[tokio::test]
async fn create_user_rejects_over_cap_username() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let too_long = "a".repeat(200);
    let err = create_user(&pool, &too_long, "pw").await.unwrap_err();
    assert!(err.to_string().contains("128-byte cap"), "over-cap username is rejected: {err}");
    assert!(stored_hash(&pool, &too_long).await.is_none(), "no user row for a rejected create");
}

/// `delete_user` round-trips with `create_user`'s normalization: deleting the SAME
/// padded input that created the row must find and remove it (both bind the
/// trimmed name through the same authority).
#[tokio::test]
async fn delete_user_round_trips_padded_username() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let bare = unique_username("adminctl-pad-del");
    let padded = format!("  {bare}  ");

    create_user(&pool, &bare, "pw").await.unwrap();
    assert!(stored_hash(&pool, &bare).await.is_some(), "user exists before delete");

    assert!(delete_user(&pool, &padded).await.unwrap(), "padded-input delete finds the trimmed row");
    assert!(stored_hash(&pool, &bare).await.is_none(), "user gone after delete");
}

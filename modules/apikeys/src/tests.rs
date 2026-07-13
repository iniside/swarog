//! Unit tests for the dev-seed shape + the `apikeys.keys` capability (the `Keys` trait
//! the gateway resolves) over the live store. The pure store CRUD lives in
//! `store_tests.rs`; here we assert the seed policy contract and that the Service maps
//! unknown/revoked keys to `Ok(None)`.

use super::*;
use crate::store::Store;
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};
use apikeysapi::Keys as _;
use std::sync::Arc;

fn params_from_form(form: &adminapi::Form) -> adminapi::Params {
    form.fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .chain(
            form.hidden
                .iter()
                .map(|field| (field.name.clone(), field.value.clone())),
        )
        .collect()
}

async fn rendered_form(svc: &Arc<Service>) -> adminapi::Form {
    crate::admin::admin_content_full(svc)
        .await
        .unwrap()
        .form
        .expect("local API key admin form")
}

async fn key_row(store: &Store, name: &str) -> crate::store::KeyRow {
    store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("missing API key row {name}"))
}

fn assert_conflict(result: Result<(), adminapi::SubmitError>) {
    assert!(
        matches!(result, Err(adminapi::SubmitError::Conflict)),
        "expected stale-form conflict, got {result:?}"
    );
}

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
    let _guard = db_test_lock().await;
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

#[tokio::test]
async fn admin_cas_fresh_edit_add_and_revoke_uses_rendered_snapshot() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let first = format!("{base}-first");
    let second = format!("{base}-second");
    let added = format!("{base}-added");
    let late = format!("{base}-late");
    svc.store
        .insert(&first, &format!("{base}-first-key"), "accounts.login")
        .await
        .unwrap();
    svc.store
        .insert(&second, &format!("{base}-second-key"), "full")
        .await
        .unwrap();

    let mut form = rendered_form(&svc).await;
    assert_eq!(form.hidden.len(), 1);
    assert_eq!(form.hidden[0].name, crate::admin::EXPECTED_STATE_FIELD);
    let snapshot: Vec<serde_json::Value> = serde_json::from_str(&form.hidden[0].value).unwrap();
    let rendered_names: Vec<&str> = snapshot
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .filter(|name| name.starts_with(&base))
        .collect();
    assert_eq!(rendered_names, vec![first.as_str(), second.as_str()]);
    assert!(snapshot.iter().all(|row| row.get("key").is_none()));

    let mut values = params_from_form(&form);
    values.insert(first.clone(), "match.report".into());
    values.insert("_revoke_name".into(), first.clone());
    values.insert("_new_name".into(), added.clone());
    values.insert("_new_key".into(), format!("{base}-added-key"));
    values.insert("_new_policy".into(), "leaderboard.topScores".into());

    // A row outside the GET snapshot is allowed and must neither conflict nor be
    // overwritten by this batch.
    svc.store
        .insert(&late, &format!("{base}-late-key"), "full")
        .await
        .unwrap();

    let submit = form.submit.take().expect("local submit closure");
    submit(values).await.unwrap();

    let first_row = key_row(&svc.store, &first).await;
    assert_eq!(first_row.policy, "match.report");
    assert!(first_row.revoked, "edit + revoke must land in one update");
    let added_row = key_row(&svc.store, &added).await;
    assert_eq!(added_row.policy, "leaderboard.topScores");
    assert!(!added_row.revoked);
    assert_eq!(key_row(&svc.store, &late).await.policy, "full");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn admin_cas_changed_row_rolls_back_other_edits_and_insert() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let stale = format!("{base}-stale");
    let edited = format!("{base}-edited");
    let added = format!("{base}-added");
    svc.store
        .insert(&stale, &format!("{base}-stale-key"), "accounts.login")
        .await
        .unwrap();
    svc.store
        .insert(&edited, &format!("{base}-edited-key"), "full")
        .await
        .unwrap();

    let mut form = rendered_form(&svc).await;
    let mut values = params_from_form(&form);
    values.insert(edited.clone(), "match.report".into());
    values.insert("_new_name".into(), added.clone());
    values.insert("_new_key".into(), format!("{base}-added-key"));
    values.insert("_new_policy".into(), "full".into());
    svc.store.set_policy(&stale, "leaderboard.topScores").await.unwrap();

    let submit = form.submit.take().unwrap();
    assert_conflict(submit(values).await);
    assert_eq!(key_row(&svc.store, &stale).await.policy, "leaderboard.topScores");
    assert_eq!(key_row(&svc.store, &edited).await.policy, "full");
    assert!(svc.store.list().await.unwrap().iter().all(|row| row.name != added));

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn admin_cas_deleted_row_rolls_back_other_changes() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let deleted = format!("{base}-deleted");
    let edited = format!("{base}-edited");
    svc.store
        .insert(&deleted, &format!("{base}-deleted-key"), "full")
        .await
        .unwrap();
    svc.store
        .insert(&edited, &format!("{base}-edited-key"), "accounts.login")
        .await
        .unwrap();

    let mut form = rendered_form(&svc).await;
    let mut values = params_from_form(&form);
    values.insert(edited.clone(), "match.report".into());
    sqlx::query("DELETE FROM apikeys.keys WHERE name = $1")
        .bind(&deleted)
        .execute(&pool)
        .await
        .unwrap();

    let submit = form.submit.take().unwrap();
    assert_conflict(submit(values).await);
    assert_eq!(key_row(&svc.store, &edited).await.policy, "accounts.login");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn admin_cas_revoked_row_rolls_back_other_changes() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let revoked = format!("{base}-revoked");
    let edited = format!("{base}-edited");
    svc.store
        .insert(&revoked, &format!("{base}-revoked-key"), "full")
        .await
        .unwrap();
    svc.store
        .insert(&edited, &format!("{base}-edited-key"), "accounts.login")
        .await
        .unwrap();

    let mut form = rendered_form(&svc).await;
    let mut values = params_from_form(&form);
    values.insert(edited.clone(), "match.report".into());
    svc.store.revoke(&revoked).await.unwrap();

    let submit = form.submit.take().unwrap();
    assert_conflict(submit(values).await);
    assert!(key_row(&svc.store, &revoked).await.revoked);
    assert_eq!(key_row(&svc.store, &edited).await.policy, "accounts.login");

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn admin_cas_insert_collision_rolls_back_prior_update() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let edited = format!("{base}-edited");
    let existing = format!("{base}-existing");
    let collided_key = format!("{base}-collision-key");
    let added = format!("{base}-added");
    svc.store
        .insert(&edited, &format!("{base}-edited-key"), "accounts.login")
        .await
        .unwrap();
    svc.store.insert(&existing, &collided_key, "full").await.unwrap();

    let mut form = rendered_form(&svc).await;
    let mut values = params_from_form(&form);
    values.insert(edited.clone(), "match.report".into());
    values.insert("_new_name".into(), added.clone());
    values.insert("_new_key".into(), collided_key);
    values.insert("_new_policy".into(), "full".into());

    let submit = form.submit.take().unwrap();
    assert_conflict(submit(values).await);
    assert_eq!(key_row(&svc.store, &edited).await.policy, "accounts.login");
    assert!(svc.store.list().await.unwrap().iter().all(|row| row.name != added));

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn admin_cas_missing_or_malformed_snapshot_is_conflict_before_db() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend")
        .unwrap();
    let svc = Service { store: Store { pool } };

    assert_conflict(crate::admin::apply_edit(&svc, adminapi::Params::new()).await);
    let mut malformed = adminapi::Params::new();
    malformed.insert(crate::admin::EXPECTED_STATE_FIELD.into(), "not-json".into());
    assert_conflict(crate::admin::apply_edit(&svc, malformed).await);
}

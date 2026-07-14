//! Live-Postgres tests for the "API Keys" admin page: the render (KPIs, table, fields)
//! over seeded rows, and the submit round-trip (policy edit, add-new, revoke, invalid
//! policy rejected). Every fixture uses a `test-`-prefixed unique key name and deletes
//! its own rows, so the shared local Postgres never has the harness's dev rows touched.
//!
//! The KPI totals count the WHOLE (shared) table, so the assertions check internal
//! consistency (total == table rows, active <= total) and that the seeded rows render as
//! expected — never an absolute global count.

use super::*;
use crate::admin::{admin_content_full, apply_edit};
use crate::store::Store;
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};
use std::sync::Arc;

/// Finds a table row by its Name cell (column 0).
fn find_row<'a>(table: &'a adminapi::Table, name: &str) -> Option<&'a Vec<adminapi::Cell>> {
    table.rows.iter().find(|r| r[0].text == name)
}

async fn rendered_params(svc: &Arc<Service>) -> adminapi::Params {
    let form = admin_content_full(svc)
        .await
        .unwrap()
        .form
        .expect("local API key admin form");
    form.fields
        .into_iter()
        .map(|field| (field.name, field.value))
        .chain(
            form.hidden
                .into_iter()
                .map(|field| (field.name, field.value)),
        )
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn render_shows_rows_kpis_and_fields() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let active_name = format!("{base}-active");
    let active_key = format!("{base}-active-key");
    let revoked_name = format!("{base}-revoked");
    let revoked_key = format!("{base}-revoked-key");
    svc.store.insert(&active_name, &active_key, "accounts.login").await.unwrap();
    svc.store.insert(&revoked_name, &revoked_key, "full").await.unwrap();
    svc.store.revoke(&revoked_name).await.unwrap();

    let content = admin_content_full(&svc).await.unwrap();

    // KPIs: internally consistent with the rendered table (shared DB ⇒ no absolute count).
    let table = content.table.as_ref().expect("table present");
    assert_eq!(content.kpis[0].label, "Keys");
    assert_eq!(content.kpis[1].label, "Active");
    let total: usize = content.kpis[0].value.parse().unwrap();
    let active: usize = content.kpis[1].value.parse().unwrap();
    assert_eq!(total, table.rows.len(), "Keys KPI must equal the table row count");
    assert!(active <= total, "Active must not exceed Keys");
    assert_eq!(
        active,
        table.rows.iter().filter(|r| r[4].text == "active").count(),
        "Active KPI must match the green-badge rows"
    );

    // Table columns + the two seeded rows (active green, revoked red; key rendered mono).
    assert_eq!(
        table.columns,
        vec!["Name", "Key", "Policy", "Created", "Status"]
    );
    let arow = find_row(table, &active_name).expect("active row present");
    assert_eq!(arow[1].text, active_key);
    assert!(arow[1].mono, "key cell is monospaced");
    assert_eq!(arow[2].text, "accounts.login");
    assert_eq!((arow[4].text.as_str(), arow[4].badge.as_str()), ("active", "green"));
    let rrow = find_row(table, &revoked_name).expect("revoked row present");
    assert_eq!((rrow[4].text.as_str(), rrow[4].badge.as_str()), ("revoked", "red"));

    // Form: one policy field per key (name == key name, value == policy) + reserved fields.
    let form = content.form.as_ref().expect("editable form present");
    let field = form.fields.iter().find(|f| f.name == active_name).expect("policy field present");
    assert_eq!(field.value, "accounts.login");
    for reserved in ["_new_name", "_new_key", "_new_policy", "_revoke_name"] {
        assert!(
            form.fields.iter().any(|f| f.name == reserved),
            "reserved field {reserved} present"
        );
    }

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_policy_edit_add_and_revoke() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "accounts.login").await.unwrap();

    // Policy edit: posting a changed value re-writes the policy.
    let mut edit = rendered_params(&svc).await;
    edit.insert(name.clone(), "full".into());
    apply_edit(&svc, edit).await.unwrap();
    assert_eq!(svc.store.lookup(&key).await.unwrap().unwrap().policy, "full");

    // Add-new: the full triple inserts a new key.
    let new_name = format!("{base}-new");
    let new_key = format!("{base}-new-key");
    let mut add = rendered_params(&svc).await;
    add.insert("_new_name".into(), new_name.clone());
    add.insert("_new_key".into(), new_key.clone());
    add.insert("_new_policy".into(), "characters.create".into());
    apply_edit(&svc, add).await.unwrap();
    assert_eq!(
        svc.store.lookup(&new_key).await.unwrap(),
        Some(apikeysapi::KeyRecord { name: new_name.clone(), policy: "characters.create".into() })
    );

    // Revoke: naming a key in `_revoke_name` revokes it (lookup then misses).
    let mut rev = rendered_params(&svc).await;
    rev.insert("_revoke_name".into(), new_name.clone());
    apply_edit(&svc, rev).await.unwrap();
    assert_eq!(svc.store.lookup(&new_key).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

// The edge admin fan-out (the split `admin.adminData` path — apikeys' admin face
// registered on the internal edge and reachable over QUIC) is asserted end-to-end,
// cross-process, by splitproof [AD3b] (`GET /admin/api-keys` through gateway →
// admin-svc → apikeys-svc, two hops, 200 + `dev-client` rendered). The former
// in-crate wire round-trip (`edge_serves_admin_data`) was removed here to keep
// apikeys off the foreign `adminrpc` glue crate: apikeys cannot generate admin's
// `admin.adminData` Client itself, so dialing its own admin face in-crate meant a
// dev-dependency on another domain's `<name>rpc`, which the fortress rule forbids.

/// Atomicity of a MIXED submit: one call carrying a valid policy edit, an INVALID policy
/// for another key, AND a valid add-row triple. Phase-1 validation fails on the invalid
/// policy before ANY write, so the whole form is rejected and the store is left exactly
/// as it was — no partial commit of the valid edit or the add-row.
#[tokio::test(flavor = "multi_thread")]
async fn submit_mixed_valid_and_invalid_is_atomic() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let x_name = format!("{base}-x");
    let x_key = format!("{base}-x-key");
    let y_name = format!("{base}-y");
    let y_key = format!("{base}-y-key");
    svc.store.insert(&x_name, &x_key, "accounts.login").await.unwrap();
    svc.store.insert(&y_name, &y_key, "characters.create").await.unwrap();

    // One submit: VALID policy change for X, INVALID (blank) policy for Y, VALID add-row.
    let new_name = format!("{base}-new");
    let new_key = format!("{base}-new-key");
    let mut edit = rendered_params(&svc).await;
    edit.insert(x_name.clone(), "full".into());
    edit.insert(y_name.clone(), "   ".into());
    edit.insert("_new_name".into(), new_name.clone());
    edit.insert("_new_key".into(), new_key.clone());
    edit.insert("_new_policy".into(), "leaderboard.topScores".into());

    let err = apply_edit(&svc, edit).await.unwrap_err();
    assert!(err.to_string().contains("invalid policy"), "got: {err}");

    // Nothing committed: X and Y policies unchanged, no new row inserted.
    assert_eq!(svc.store.lookup(&x_key).await.unwrap().unwrap().policy, "accounts.login");
    assert_eq!(svc.store.lookup(&y_key).await.unwrap().unwrap().policy, "characters.create");
    assert_eq!(svc.store.lookup(&new_key).await.unwrap(), None, "add-row must not have committed");

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_rejects_invalid_policy_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "accounts.login").await.unwrap();

    // A blank policy is invalid; apply_edit returns an error and leaves the policy intact.
    let mut edit = rendered_params(&svc).await;
    edit.insert(name.clone(), "   ".into());
    let err = apply_edit(&svc, edit).await.unwrap_err();
    assert!(err.to_string().contains("invalid policy"), "got: {err}");
    assert_eq!(svc.store.lookup(&key).await.unwrap().unwrap().policy, "accounts.login");

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_rejects_underscore_prefixed_new_name_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    // A `_`-prefixed name would collide with the form's own `_new_*`/`_revoke_name`
    // control fields on the next render — apply_edit must reject it before any write.
    let new_name = format!("_{base}-new");
    let new_key = format!("{base}-new-key");

    let mut add = rendered_params(&svc).await;
    add.insert("_new_name".into(), new_name.clone());
    add.insert("_new_key".into(), new_key.clone());
    add.insert("_new_policy".into(), "full".into());
    let err = apply_edit(&svc, add).await.unwrap_err();
    assert!(err.to_string().contains("must not start with '_'"), "got: {err}");
    assert_eq!(svc.store.lookup(&new_key).await.unwrap(), None, "insert must not have committed");

    cleanup(&pool, &base).await;
}

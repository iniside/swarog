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
use crate::store_tests::{cleanup, test_pool, unique_name};
use std::sync::Arc;

/// Finds a table row by its Name cell (column 0).
fn find_row<'a>(table: &'a adminapi::Table, name: &str) -> Option<&'a Vec<adminapi::Cell>> {
    table.rows.iter().find(|r| r[0].text == name)
}

#[tokio::test(flavor = "multi_thread")]
async fn render_shows_rows_kpis_and_fields() {
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
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "accounts.login").await.unwrap();

    // Policy edit: posting a changed value re-writes the policy.
    let mut edit = adminapi::Params::new();
    edit.insert(name.clone(), "full".into());
    apply_edit(&svc, edit).await.unwrap();
    assert_eq!(svc.store.lookup(&key).await.unwrap().unwrap().policy, "full");

    // Add-new: the full triple inserts a new key.
    let new_name = format!("{base}-new");
    let new_key = format!("{base}-new-key");
    let mut add = adminapi::Params::new();
    add.insert("_new_name".into(), new_name.clone());
    add.insert("_new_key".into(), new_key.clone());
    add.insert("_new_policy".into(), "characters.create".into());
    apply_edit(&svc, add).await.unwrap();
    assert_eq!(
        svc.store.lookup(&new_key).await.unwrap(),
        Some(apikeysapi::KeyRecord { name: new_name.clone(), policy: "characters.create".into() })
    );

    // Revoke: naming a key in `_revoke_name` revokes it (lookup then misses).
    let mut rev = adminapi::Params::new();
    rev.insert("_revoke_name".into(), new_name.clone());
    apply_edit(&svc, rev).await.unwrap();
    assert_eq!(svc.store.lookup(&new_key).await.unwrap(), None);

    cleanup(&pool, &base).await;
}

/// The edge admin fan-out end-to-end (the split `admin.adminData` path): register BOTH
/// the keys face and the admin face on one edge server exactly as `init` does, then dial
/// with the generated admin Client and assert the page comes back. Guards the Step 6
/// regression where the admin face silently went unregistered.
#[tokio::test(flavor = "multi_thread")]
async fn edge_serves_admin_data() {
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "full").await.unwrap();

    let ca = edge::DevCA::generate().unwrap();
    let mut server = edge::Server::new();
    apikeysrpc::keys_rpc::register_server(&mut server, svc.clone());
    apikeysrpc::register_admin(&mut server, svc.clone());
    let running = server.listen("127.0.0.1:0".parse().unwrap(), &ca).unwrap();
    let addr = running.local_addr();

    let client = edge::Client::dial(addr, &ca).await.unwrap();
    let admin_client = adminrpc::admin_data_rpc::Client::new(std::sync::Arc::new(client));
    let data = adminapi::AdminData::admin_data(&admin_client).await.unwrap();
    assert_eq!(data.id, "apikeys");
    assert_eq!(data.section, "Platform");
    assert!(data.content.form.is_none(), "remote content is read-only");
    assert!(
        data.content.table.unwrap().rows.iter().any(|r| r[0].text == name),
        "seeded key renders in the remote admin table"
    );

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_rejects_invalid_policy_without_writing() {
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let name = format!("{base}-a");
    let key = format!("{base}-key");
    svc.store.insert(&name, &key, "accounts.login").await.unwrap();

    // A blank policy is invalid; apply_edit returns an error and leaves the policy intact.
    let mut edit = adminapi::Params::new();
    edit.insert(name.clone(), "   ".into());
    let err = apply_edit(&svc, edit).await.unwrap_err();
    assert!(err.to_string().contains("invalid policy"), "got: {err}");
    assert_eq!(svc.store.lookup(&key).await.unwrap().unwrap().policy, "accounts.login");

    cleanup(&pool, &base).await;
}

//! Live-Postgres tests for the "API Keys" configurator: the typed render (keys table with
//! a display prefix — never the secret, the `_action` Select, role/key Selects), the
//! submit DISPATCH (each action, explicit rejection of partial/empty input), CAS conflict
//! on a stale form, the SHOW-ONCE secret, and the remote seams — including finding #2
//! (a domain-missing target surfaces as `Conflict`, never `NotFound`).
//!
//! Every fixture uses a `test-`-prefixed unique base and deletes its own rows, so the
//! shared local Postgres never has the harness's dev rows touched.

use super::*;
use crate::admin::{admin_content_local, apply_submit};
use crate::store::Store;
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};
use adminapi::{AdminData as _, AdminSubmit as _};
use sqlx::PgPool;
use std::sync::Arc;

async fn content(svc: &Arc<Service>) -> adminapi::Content {
    admin_content_local(svc).await.unwrap()
}

/// The full posted-form param map: every visible field + hidden CAS-evidence field, as a
/// browser would echo it (the caller overrides the action + the fields it drives).
async fn form_params(svc: &Arc<Service>) -> adminapi::Params {
    let form = content(svc).await.form.expect("local form");
    form.fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .chain(form.hidden.iter().map(|h| (h.name.clone(), h.value.clone())))
        .collect()
}

async fn role_rev(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar("SELECT revision FROM apikeys.roles WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_role(svc: &Arc<Service>, name: &str, policy: &str) {
    svc.store.create_role(name, policy).await.unwrap();
}

// ---- Render ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn render_shows_keys_table_prefix_and_typed_action_select() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");
    seed_role(&svc, &role, "full").await;
    let (secret, prefix) = svc.store.create_key(&key, &role).await.unwrap();

    let c = content(&svc).await;
    let table = c.table.as_ref().expect("table");
    assert_eq!(table.columns, vec!["Name", "Prefix", "Role", "Created", "Status"]);
    let row = table.rows.iter().find(|r| r[0].text == key).expect("key row");
    assert_eq!(row[1].text, prefix, "prefix column shows the prefix");
    assert!(row[1].mono, "prefix is monospaced");
    assert_ne!(row[1].text, secret, "the table NEVER shows the full secret");
    assert_eq!(row[2].text, role);
    assert_eq!((row[4].text.as_str(), row[4].badge.as_str()), ("active", "green"));

    // The typed action selector + role/key Selects carry the current rows as options.
    let form = c.form.as_ref().expect("form");
    let action = form.fields.iter().find(|f| f.name == "_action").expect("_action field");
    assert_eq!(action.kind, adminapi::FieldKind::Select);
    assert!(action.options.iter().any(|o| o.value == "create_key"));
    let key_role = form.fields.iter().find(|f| f.name == "key_role").expect("key_role select");
    assert_eq!(key_role.kind, adminapi::FieldKind::Select);
    assert!(key_role.options.iter().any(|o| o.value == role));
    // CAS evidence is present for the row.
    assert!(form.hidden.iter().any(|h| h.name == format!("_expected_key_rev_{key}")));

    cleanup(&pool, &base).await;
}

// ---- Submit dispatch + show-once -------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn submit_create_role_then_create_key_reveals_secret_once() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    let key = format!("{base}-key");

    // create_role → empty reveal.
    let mut p = form_params(&svc).await;
    p.insert("_action".into(), "create_role".into());
    p.insert("role_name".into(), role.clone());
    p.insert("role_policy".into(), "full".into());
    assert!(apply_submit(&svc, p).await.unwrap().reveal.is_empty());

    // create_key → the secret, exactly once.
    let mut p = form_params(&svc).await;
    p.insert("_action".into(), "create_key".into());
    p.insert("key_name".into(), key.clone());
    p.insert("key_role".into(), role.clone());
    let out = apply_submit(&svc, p).await.unwrap();
    assert_eq!(out.reveal.len(), 1);
    assert_eq!(out.reveal[0].label, "secret");
    let secret = out.reveal[0].value.clone();
    assert!(secret.starts_with("ak_"), "secret shape: {secret}");

    // The secret resolves to the role policy — and no READ path ever holds it.
    assert_eq!(svc.store.lookup(&secret).await.unwrap().unwrap().policy, "full");
    assert!(
        svc.store.list_keys().await.unwrap().iter().all(|k| k.prefix != secret),
        "list_keys never carries the full secret"
    );
    let rerender = content(&svc).await.form.unwrap();
    assert!(
        rerender.fields.iter().all(|f| !f.value.contains(&secret))
            && rerender.hidden.iter().all(|h| !h.value.contains(&secret)),
        "a re-render never echoes the show-once secret"
    );

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_partial_create_key_rejected_without_writing() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    seed_role(&svc, &role, "full").await;

    // Action chosen, role chosen, but the key NAME left blank — explicit rejection, not a
    // silent no-op (the bug the old flat-form design had).
    let mut p = form_params(&svc).await;
    p.insert("_action".into(), "create_key".into());
    p.insert("key_role".into(), role.clone());
    let err = apply_submit(&svc, p).await.unwrap_err();
    assert!(matches!(err, adminapi::SubmitError::Other(_)), "got {err:?}");
    assert!(
        svc.store.list_keys().await.unwrap().iter().all(|k| !k.name.starts_with(&base)),
        "no key written on partial input"
    );

    cleanup(&pool, &base).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_empty_action_rejected() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });

    let mut p = adminapi::Params::new();
    p.insert("_action".into(), String::new());
    assert!(matches!(apply_submit(&svc, p).await.unwrap_err(), adminapi::SubmitError::Other(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_stale_role_revision_conflicts_and_preserves_out_of_band_write() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;
    let role = format!("{base}-role");
    seed_role(&svc, &role, "accounts.login").await;

    // Render captures `_expected_role_rev_<role>` = 1.
    let mut p = form_params(&svc).await;
    p.insert("_action".into(), "set_role_policy".into());
    p.insert("role_target".into(), role.clone());
    p.insert("role_policy".into(), "characters.create".into());

    // Out-of-band edit bumps the revision to 2 and sets policy "full".
    let rev1 = role_rev(&pool, &role).await;
    svc.store.set_role_policy(&role, rev1, "full").await.unwrap();

    // The stale submit conflicts and does NOT overwrite the out-of-band value.
    let err = apply_submit(&svc, p).await.unwrap_err();
    assert!(matches!(err, adminapi::SubmitError::Conflict), "got {err:?}");
    assert_eq!(
        svc.store.list_roles().await.unwrap().into_iter().find(|r| r.name == role).unwrap().policy,
        "full",
        "out-of-band write preserved; stale submit wrote nothing"
    );

    cleanup(&pool, &base).await;
}

// ---- Remote seams ----------------------------------------------------------

/// Finding #2: a domain-missing target through the REMOTE `admin_submit` surfaces as
/// `Conflict` (409), NEVER `NotFound` (which the edge would make indistinguishable from
/// UnknownMethod → the admin masking the real error as read-only 405).
#[tokio::test(flavor = "multi_thread")]
async fn admin_submit_create_key_missing_role_is_conflict_not_notfound() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });
    let base = unique_name(&pool).await;

    let mut p = adminapi::Params::new();
    p.insert("_action".into(), "create_key".into());
    p.insert("key_name".into(), format!("{base}-key"));
    p.insert("key_role".into(), format!("{base}-no-such-role"));
    let err = svc.admin_submit("apikeys".into(), p).await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Conflict, "domain conflict, not {:?}", err.status);
    assert_ne!(err.status, opsapi::Status::NotFound);

    cleanup(&pool, &base).await;
}

/// The remote READ (`admin_data`) returns the SAME typed structure with NO submit closure
/// (the wire can't carry it) — the admin drives writes via `admin_submit`.
#[tokio::test(flavor = "multi_thread")]
async fn admin_data_remote_has_typed_fields_and_no_submit_closure() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let svc = Arc::new(Service { store: Store { pool: pool.clone() } });

    let data = svc.admin_data().await.unwrap();
    assert_eq!(data.id, "apikeys");
    let form = data.content.form.expect("remote form structure present");
    assert!(form.submit.is_none(), "remote form carries no submit closure");
    assert!(
        form.fields.iter().any(|f| f.name == "_action" && f.kind == adminapi::FieldKind::Select),
        "typed action selector marshals to the remote render"
    );
}

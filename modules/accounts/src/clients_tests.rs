//! The dev seed's gate and insert, the client id shape and the grant order.

use std::ffi::OsString;

use accountsapi::{grant_types, DEV_GAME_CLIENT_ID, MAX_CLIENT_ID_BYTES, SWAROG_AUDIENCE};
use lifecycle::{Context, Module as _};
use testdb::test_pool;

use super::{canonical_grants, dev_seed_from, new_client_id, seed_dev_client};
use crate::tests::{dev_kek, wired, GHOST_PLAYER};
use crate::Accounts;

#[test]
fn dev_seed_is_on_only_for_an_explicitly_truthy_value() {
    assert!(!dev_seed_from(None), "unset is off");
    for off in ["", "0", "false", "off", "yes"] {
        assert!(!dev_seed_from(Some(OsString::from(off))), "{off:?} is off");
    }
    for on in ["1", "true", "TRUE", "on", "On"] {
        assert!(dev_seed_from(Some(OsString::from(on))), "{on:?} is on");
    }
    assert!(Accounts::new().dev_seed.get().is_none(), "migrate before init seeds nothing");
}

#[test]
fn a_client_id_is_c_and_22_base64url_characters() {
    for _ in 0..32 {
        let id = new_client_id();
        assert_eq!(id.len(), 24, "{id}");
        assert!(id.len() <= MAX_CLIENT_ID_BYTES);
        assert!(id.starts_with("c_"), "{id}");
        assert!(id[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{id}");
    }
    assert_ne!(new_client_id(), new_client_id());
}

#[test]
fn grants_keep_the_contract_order_whatever_the_pick_order() {
    let picked = vec![grant_types::REFRESH_TOKEN.to_string(), grant_types::PASSWORD.to_string()];
    assert_eq!(canonical_grants(&picked), [grant_types::PASSWORD, grant_types::REFRESH_TOKEN]);
    assert!(canonical_grants(&[]).is_empty());
}

type DevClient = (String, Vec<String>, String);

async fn dev_client(conn: &mut sqlx::PgConnection) -> Vec<DevClient> {
    sqlx::query_as("SELECT display_name, grant_types, audience FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .fetch_all(conn)
        .await
        .unwrap()
}

/// In one rolled-back transaction, so a dev fleet's or a parallel test's dev client is never
/// touched.
#[tokio::test]
async fn the_dev_seed_inserts_the_dev_client_once_and_keeps_an_edited_row() {
    let Some(pool) = test_pool().await else { return };
    wired(&pool).await;
    let all: Vec<String> = grant_types::ALL.map(String::from).to_vec();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(dev_client(&mut tx).await.is_empty());

    seed_dev_client(&mut *tx).await.unwrap();
    let seeded = ("Dev game".to_string(), all.clone(), SWAROG_AUDIENCE.to_string());
    assert_eq!(dev_client(&mut tx).await, [seeded]);

    sqlx::query("UPDATE accounts.oidc_clients SET display_name = 'edited' WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .execute(&mut *tx)
        .await
        .unwrap();
    seed_dev_client(&mut *tx).await.unwrap();
    let kept = ("edited".to_string(), all, SWAROG_AUDIENCE.to_string());
    assert_eq!(dev_client(&mut tx).await, [kept], "a repeat seed inserts nothing and keeps the edit");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn migrate_with_the_flag_on_leaves_the_dev_client_in_place() {
    let Some(pool) = test_pool().await else { return };
    wired(&pool).await;
    let ctx = Context::with_db(pool.clone());
    let accounts = Accounts::with_signing_kek(dev_kek());
    accounts.dev_seed.set(true).unwrap();
    accounts.register(&ctx).unwrap();
    {
        let _no_ghost = GHOST_PLAYER.read().await;
        accounts.migrate(&ctx).await.unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(dev_client(&mut conn).await.len(), 1);
}

//! The dev seed's gate and insert, the client id shape and the grant order.

use std::ffi::OsString;

use accountsapi::{grant_types, DEV_GAME_CLIENT_ID, MAX_CLIENT_ID_BYTES, SWAROG_AUDIENCE};
use lifecycle::Module as _;
use testdb::test_pool;

use super::{
    canonical_grants, dev_seed_from, migrate_dev_seed, new_client_id, seed_dev_client, DEV_SEED_ENV,
};
use crate::tests::{dev_kek, wired, GHOST_PLAYER};
use crate::Accounts;

/// Held by every test here that touches the `dev-game` row, after `wired` (which takes
/// `GHOST_PLAYER` for read): the migrate tests delete and reseed it committed, which a
/// rolled-back test would otherwise see mid-transaction.
static DEV_CLIENT_ROW: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn dev_seed_is_on_only_for_an_explicitly_truthy_value() {
    assert!(!dev_seed_from(None), "unset is off");
    for off in ["", "0", "false", "off", "yes"] {
        assert!(!dev_seed_from(Some(OsString::from(off))), "{off:?} is off");
    }
    for on in ["1", "true", "TRUE", "on", "On"] {
        assert!(dev_seed_from(Some(OsString::from(on))), "{on:?} is on");
    }
}

#[test]
fn dev_seed_on_is_what_init_read_and_off_when_init_never_ran() {
    assert!(!Accounts::new().dev_seed_on(), "no init is off");
    let off = Accounts::new();
    off.dev_seed.set(false).unwrap();
    assert!(!off.dev_seed_on(), "init read off");
    let on = Accounts::new();
    on.dev_seed.set(true).unwrap();
    assert!(on.dev_seed_on(), "init read on");
}

/// `init` reads `ACCOUNTS_DEV_SEED` into the switch `migrate` passes; no test process sets it,
/// so a real `register` + `init` on a fresh `wired` context leaves the switch off.
#[tokio::test]
async fn init_with_the_env_unset_reads_the_dev_seed_off() {
    let Some(pool) = test_pool().await else { return };
    assert!(std::env::var_os(DEV_SEED_ENV).is_none(), "the test process must not set {DEV_SEED_ENV}");
    let (ctx, _svc) = wired(&pool).await;
    let accounts = Accounts::with_signing_kek(dev_kek());
    accounts.register(&ctx).unwrap();
    accounts.init(&ctx).unwrap();
    assert!(!accounts.dev_seed_on(), "init with the env unset reads the switch off");
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
    let _row = DEV_CLIENT_ROW.lock().await;
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

/// The switch `migrate` passes `ACCOUNTS_DEV_SEED` to: off (the default) inserts nothing, on
/// inserts the dev client. In one rolled-back transaction like the test above.
#[tokio::test]
async fn the_dev_seed_switch_inserts_the_dev_client_only_when_on() {
    let Some(pool) = test_pool().await else { return };
    wired(&pool).await;
    let _row = DEV_CLIENT_ROW.lock().await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .execute(&mut *tx)
        .await
        .unwrap();

    migrate_dev_seed(false, &mut *tx).await.unwrap();
    assert!(dev_client(&mut tx).await.is_empty(), "the switch off seeds nothing");

    migrate_dev_seed(true, &mut *tx).await.unwrap();
    assert_eq!(dev_client(&mut tx).await.len(), 1, "the switch on seeds the dev client");
    tx.rollback().await.unwrap();
}

/// The real `migrate` of an `Accounts` whose `init` read the switch on reseeds a deleted dev
/// client, which pins the value at the shipping call site. The delete is committed because
/// `migrate` runs on its own connection; the row stays afterwards, as on every dev fleet.
#[tokio::test]
async fn migrate_with_the_switch_on_reseeds_a_deleted_dev_client() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, _svc) = wired(&pool).await;
    let _no_ghost = GHOST_PLAYER.write().await;
    let _row = DEV_CLIENT_ROW.lock().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(dev_client(&mut conn).await.is_empty());

    let accounts = Accounts::with_signing_kek(dev_kek());
    accounts.dev_seed.set(true).unwrap();
    accounts.migrate(&ctx).await.unwrap();
    assert_eq!(dev_client(&mut conn).await.len(), 1, "migrate with the switch on seeds the dev client");
}

/// The real `migrate` of an `Accounts` whose `init` never ran passes the switch off and leaves
/// a deleted dev client deleted. The delete is committed like the test above's; the row is
/// reseeded before the assertion, so a failure never leaves the shared database without it.
#[tokio::test]
async fn migrate_with_the_switch_off_leaves_a_deleted_dev_client_deleted() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, _svc) = wired(&pool).await;
    let _no_ghost = GHOST_PLAYER.write().await;
    let _row = DEV_CLIENT_ROW.lock().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("DELETE FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(DEV_GAME_CLIENT_ID)
        .execute(&mut *conn)
        .await
        .unwrap();

    let migrated = Accounts::with_signing_kek(dev_kek()).migrate(&ctx).await;
    let after = dev_client(&mut conn).await;
    seed_dev_client(&mut *conn).await.unwrap();
    migrated.unwrap();
    assert!(after.is_empty(), "migrate with the switch off seeds nothing");
}

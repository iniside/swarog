//! The client a login names (`Service::issue_session_tx`, `Service::refresh_allowed_tx`)
//! and the token family every mint keeps (`accounts.token_families`), which the presence
//! readers count.

use super::*;

use accountsapi::grant_types::{PASSWORD, REFRESH_TOKEN};

fn assert_invalid_client(err: Error, what: &str) {
    assert_eq!(err.status, opsapi::Status::Unauthorized, "{what}: {err:?}");
    assert_eq!(err.msg, "invalid client", "{what}");
}

async fn family_of(pool: &PgPool, refresh_token: &str) -> String {
    sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
        .bind(refresh_token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_rows(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.token_families WHERE id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn token_used(pool: &PgPool, refresh_token: &str) -> bool {
    sqlx::query_scalar("SELECT used_at IS NOT NULL FROM accounts.refresh_tokens WHERE token = $1")
        .bind(refresh_token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn online_until(svc: &Service, player_id: &str) -> String {
    let rows = svc.store.players_by_id(&[player_id.to_string()]).await.unwrap();
    rows.into_iter().next().unwrap().online_until
}

#[tokio::test]
async fn an_unknown_client_is_refused_on_every_login_op() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let unknown = format!("t-unknown-{}", suffix());
    let email = format!("nocl-{}@test.local", suffix().to_lowercase());

    let register = svc.register(email.clone(), "pw".into(), "NoCl".into(), unknown.clone()).await;
    let guest = svc.create_guest(unknown.clone()).await;
    let reg = svc.register(email.clone(), "pw".into(), "NoCl".into(), test_client()).await.unwrap();
    let login = svc.login(email, "pw".into(), unknown).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_invalid_client(register.unwrap_err(), "register");
    assert_invalid_client(guest.unwrap_err(), "create_guest");
    assert_invalid_client(login.unwrap_err(), "login");
}

#[tokio::test]
async fn an_oversized_client_id_is_refused_like_an_unknown_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("bigcl-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email.clone(), "pw".into(), "BigCl".into(), test_client()).await.unwrap();
    let oversized = "c".repeat(accountsapi::MAX_CLIENT_ID_BYTES + 1);

    let login = svc.login(email, "pw".into(), oversized.clone()).await;
    let guest = svc.create_guest(oversized).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_invalid_client(login.unwrap_err(), "login");
    assert_invalid_client(guest.unwrap_err(), "create_guest");
}

#[tokio::test]
async fn a_client_without_the_password_grant_cannot_register_or_log_in() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[REFRESH_TOKEN]).await;
    let email = format!("nogrant-{}@test.local", suffix().to_lowercase());

    let refused_register = svc.register(email.clone(), "pw".into(), "NoGrant".into(), client.clone()).await;
    let after_refusal = svc.login(email.clone(), "pw".into(), test_client()).await;
    let reg = svc.register(email.clone(), "pw".into(), "NoGrant".into(), test_client()).await.unwrap();
    let refused_login = svc.login(email, "pw".into(), client.clone()).await;
    cleanup_player(&pool, &reg.player_id).await;
    delete_client(&pool, &client).await;

    assert_invalid_client(refused_register.unwrap_err(), "register");
    assert_eq!(
        after_refusal.unwrap_err().status,
        opsapi::Status::Unauthorized,
        "a refused registration creates no account"
    );
    assert_invalid_client(refused_login.unwrap_err(), "login");
}

#[tokio::test]
async fn refresh_is_refused_once_the_client_drops_refresh_token_and_keeps_the_token() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &[PASSWORD, REFRESH_TOKEN]).await;
    let email = format!("dropref-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "DropRef".into(), client.clone()).await.unwrap();

    set_client_grants(&pool, &client, &[PASSWORD]).await;
    let refused = svc.refresh(reg.refresh_token.clone()).await;
    let used_after_refusal = token_used(&pool, &reg.refresh_token).await;
    set_client_grants(&pool, &client, &[PASSWORD, REFRESH_TOKEN]).await;
    let restored = svc.refresh(reg.refresh_token.clone()).await.map(|_| ());
    cleanup_player(&pool, &reg.player_id).await;
    delete_client(&pool, &client).await;

    assert_invalid_client(refused.unwrap_err(), "refresh");
    assert!(!used_after_refusal, "a refused refresh leaves its token unconsumed");
    restored.unwrap();
}

#[tokio::test]
async fn a_revoked_familys_row_is_gone_and_no_longer_counts_as_online() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("revfam-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "RevFam".into(), test_client()).await.unwrap();
    let family = family_of(&pool, &reg.refresh_token).await;
    let online_before = online_until(&svc, &reg.player_id).await;
    let rows_before = family_rows(&pool, &family).await;

    let mut tx = pool.begin().await.unwrap();
    svc.store.kill_family_tx(&mut tx, &family).await.unwrap();
    tx.commit().await.unwrap();
    let rows_after = family_rows(&pool, &family).await;
    let online_after = online_until(&svc, &reg.player_id).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_eq!(rows_before, 1, "every mint keeps its family row");
    assert!(!online_before.is_empty(), "a live family counts as online");
    assert_eq!(rows_after, 0, "a revoked family's row is deleted");
    assert_eq!(online_after, "", "a revoked family no longer counts as online");
}

#[tokio::test]
async fn a_refresh_moves_the_familys_access_expiry() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("touch-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email, "pw".into(), "Touch".into(), test_client()).await.unwrap();
    let family = family_of(&pool, &reg.refresh_token).await;
    sqlx::query(
        "UPDATE accounts.token_families SET access_expires_at = now() - interval '1 minute' \
          WHERE id = $1::uuid",
    )
    .bind(&family)
    .execute(&pool)
    .await
    .unwrap();
    let online_lapsed = online_until(&svc, &reg.player_id).await;

    let refreshed = svc.refresh(reg.refresh_token.clone()).await.map(|_| ());
    let online_refreshed = online_until(&svc, &reg.player_id).await;
    cleanup_player(&pool, &reg.player_id).await;

    assert_eq!(online_lapsed, "", "a family whose access lapsed is not online");
    refreshed.unwrap();
    assert!(!online_refreshed.is_empty(), "the refresh's access token extends the family");
}

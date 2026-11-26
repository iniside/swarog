//! Identity last used: every session-creating path stamps the identity it minted through
//! (`Service::issue_session_tx`), and a refresh, which rotates inside a family, does not.

use super::*;

/// Seconds since `provider`'s identity of `player_id` was last used; `None` when never.
async fn idle_secs(pool: &PgPool, player_id: &str, provider: &str) -> Option<f64> {
    sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM (now() - last_used_at))::float8 \
           FROM accounts.identities WHERE player_id = $1::uuid AND provider = $2",
    )
    .bind(player_id)
    .bind(provider)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Moves every stamped identity of `player_id` one day into the past.
async fn backdate(pool: &PgPool, player_id: &str) {
    sqlx::query(
        "UPDATE accounts.identities SET last_used_at = last_used_at - interval '1 day' \
          WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .execute(pool)
    .await
    .unwrap();
}

fn fresh(idle: Option<f64>) -> bool {
    idle.is_some_and(|s| s < 3_600.0)
}

fn day_old(idle: Option<f64>) -> bool {
    idle.is_some_and(|s| s >= 86_400.0)
}

#[tokio::test]
async fn register_and_login_stamp_and_refresh_does_not() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("used-{}@test.local", suffix().to_lowercase());
    let reg = svc.register(email.clone(), "pw".into(), "Used".into(), test_client()).await.unwrap();
    let id = reg.player_id.clone();
    let registered = idle_secs(&pool, &id, "password").await;

    backdate(&pool, &id).await;
    let refreshed = svc.refresh(reg.refresh_token.clone()).await.map(|_| ());
    let after_refresh = idle_secs(&pool, &id, "password").await;

    let login = svc.login(email.clone(), "pw".into(), test_client()).await.map(|_| ());
    let after_login = idle_secs(&pool, &id, "password").await;
    cleanup_player(&pool, &id).await;

    assert!(fresh(registered), "register stamps its identity: {registered:?}");
    refreshed.unwrap();
    assert!(day_old(after_refresh), "a refresh never stamps: {after_refresh:?}");
    login.unwrap();
    assert!(fresh(after_login), "a password login stamps: {after_login:?}");
}

#[tokio::test]
async fn a_mixed_case_password_login_stamps_the_one_identity() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("used-{}@test.local", suffix().to_lowercase());
    let id = svc.register(email.clone(), "pw".into(), "Used".into(), test_client()).await.unwrap().player_id;
    backdate(&pool, &id).await;

    let login = svc.login(email.to_ascii_uppercase(), "pw".into(), test_client()).await.map(|_| ());
    let after_login = idle_secs(&pool, &id, "password").await;
    cleanup_player(&pool, &id).await;

    login.unwrap();
    assert!(fresh(after_login), "an upper-case login stamps the one identity: {after_login:?}");
}

#[tokio::test]
async fn external_login_stamps_on_first_and_later_sight() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let sub = format!("used-{}", suffix());
    let (first, _) = svc.external_login("epic", &sub, "epic:used", None, TEST_CLIENT_ID).await.unwrap();
    let id = first.player_id.clone();
    let provisioned = idle_secs(&pool, &id, "epic").await;

    backdate(&pool, &id).await;
    let again = svc.external_login("epic", &sub, "epic:used", None, TEST_CLIENT_ID).await.map(|_| ());
    let after_again = idle_secs(&pool, &id, "epic").await;
    cleanup_player(&pool, &id).await;

    assert!(fresh(provisioned), "first sight stamps: {provisioned:?}");
    again.unwrap();
    assert!(fresh(after_again), "a later sight stamps again: {after_again:?}");
}

#[tokio::test]
async fn a_guest_session_stamps_the_guest_identity() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let guest = svc.create_guest(test_client()).await.unwrap();
    let idle = idle_secs(&pool, &guest.player_id, "guest").await;
    cleanup_player(&pool, &guest.player_id).await;

    assert!(fresh(idle), "a guest session stamps its identity: {idle:?}");
}

#[tokio::test]
async fn a_linked_identity_stays_unused_until_a_session_goes_through_it() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("link-{}@test.local", suffix().to_lowercase());
    let id = svc.register(email.clone(), "pw".into(), "Link".into(), test_client()).await.unwrap().player_id;
    let sub = format!("used-link-{}", suffix());
    let linked = svc.link_identity(&id, "epic", &sub).await.map(|_| ());
    let after_link = idle_secs(&pool, &id, "epic").await;

    let login = svc.login(email, "pw".into(), test_client()).await.map(|_| ());
    let after_dev_login = idle_secs(&pool, &id, "epic").await;
    cleanup_player(&pool, &id).await;

    linked.unwrap();
    assert_eq!(after_link, None, "linking mints no session, so it stamps nothing");
    login.unwrap();
    assert_eq!(after_dev_login, None, "a login through dev never stamps the epic identity");
}

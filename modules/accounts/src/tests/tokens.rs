//! Access tokens: the signer, `verify_access`, the boot size check, the
//! `token_revocations` writers and the `TokenState` snapshot.

use super::*;
use crate::jwt::Tokens;
use accountsapi::TokenState as _;

async fn random_uuid(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT gen_random_uuid()::text").fetch_one(pool).await.unwrap()
}

/// How many `token_revocations` rows name `id` in `column` (`family_id`, `player_id` or
/// `client_id`).
async fn revocations(pool: &PgPool, column: &str, id: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*)::int8 FROM accounts.token_revocations WHERE {column}::text = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn player_count(pool: &PgPool, player_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*)::int8 FROM accounts.players WHERE id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Registers a password player for `tag` and answers its id and refresh token.
async fn registered(svc: &Service, tag: &str) -> (String, String) {
    let sess = svc
        .register(format!("{tag}-{}@test.local", suffix()), "pw".into(), tag.into(), test_client())
        .await
        .unwrap();
    (sess.player_id, sess.refresh_token)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_token_round_trips_and_one_past_exp_and_leeway_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let tokens = &svc.tokens;
    let (sub, sid) = (random_uuid(&pool).await, random_uuid(&pool).await);
    let now = Tokens::unix_now();

    let fresh = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now);
    let token = tokens.sign(&fresh).await.unwrap();
    assert_eq!(tokens.verify_access(&token).await.unwrap(), Some(fresh.clone()));

    let stale = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now - 3600);
    let expired = tokens.sign(&stale).await.unwrap();
    assert_eq!(tokens.verify_access(&expired).await.unwrap(), None, "a token an hour old verified");

    let snapshot = tokens.snapshot().await.unwrap();
    let leeway = accountsapi::ACCESS_EXP_LEEWAY.as_secs() as i64;
    assert_eq!(Tokens::verify_against(&snapshot, &token, fresh.exp + leeway), Some(fresh.clone()));
    assert_eq!(Tokens::verify_against(&snapshot, &token, fresh.exp + leeway + 1), None);

    let mut tampered = token.clone();
    let last = if tampered.ends_with('A') { "B" } else { "A" };
    tampered.replace_range(tampered.len() - 1.., last);
    assert_eq!(Tokens::verify_against(&snapshot, &tampered, now), None, "a bad signature verified");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_boot_size_check_passes_the_default_issuer_and_refuses_a_long_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    svc.tokens.check_token_size().await.unwrap();

    let long = Tokens::new(
        pool.clone(),
        "accounts",
        format!("https://{}.example/accounts/oidc", "a".repeat(600)),
        Arc::new(OnceLock::from(dev_kek())),
    );
    let err = long.check_token_size().await.unwrap_err().to_string();
    assert!(err.contains("ACCOUNTS_OIDC_ISSUER"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_replayed_outside_grace_writes_one_family_revocation() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, refresh) = registered(&svc, "RevFamily").await;
    let family: String =
        sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
            .bind(&refresh)
            .fetch_one(&pool)
            .await
            .unwrap();

    svc.refresh(refresh.clone()).await.unwrap();
    sqlx::query("UPDATE accounts.refresh_tokens SET used_at = now() + make_interval(secs => $2) WHERE token = $1")
        .bind(&refresh)
        .bind(-60.0_f64)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(revocations(&pool, "family_id", &family).await, 0);

    let err = svc.refresh(refresh).await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unauthorized);
    assert_eq!(revocations(&pool, "family_id", &family).await, 1, "the replay must revoke its family once");
    assert_eq!(revocations(&pool, "player_id", &pid).await, 0, "a replay revokes the family, not the player");

    cleanup_player(&pool, &pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_password_writes_one_player_revocation_that_outlives_the_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevReset").await;

    let reset = svc.reset_password(&pid).await.unwrap();
    assert!(matches!(reset, PasswordReset::Reset { .. }), "the player has a password to reset");
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1);

    cleanup_player(&pool, &pid).await;
    assert_eq!(player_count(&pool, &pid).await, 0);
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1, "the player's cascade took its revocation");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_account_writes_one_player_revocation_that_outlives_the_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevDelete").await;

    let ticket = svc.begin_delete(Identity::player(&pid)).await.unwrap().ticket;
    svc.delete_account(Identity::player(&pid), ticket).await.unwrap();
    assert_eq!(player_count(&pool, &pid).await, 0, "delete_account must delete the player");
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1, "the delete must leave one player revocation");

    cleanup_player(&pool, &pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_all_writes_one_player_revocation_that_outlives_the_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevAll").await;
    let mut second = svc.store.pool.begin().await.unwrap();
    svc.store.insert_family_tx(&mut second, &pid, TEST_CLIENT_ID).await.unwrap();
    second.commit().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let revoked = svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(revoked, Some(2), "both families are revoked");
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1, "two families must still be one player row");

    cleanup_player(&pool, &pid).await;
    assert_eq!(player_count(&pool, &pid).await, 0);
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1, "the player's cascade took its revocation");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_delete_writes_one_client_revocation() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &accountsapi::grant_types::ALL[..]).await;

    assert!(svc.store.delete_client(&client).await.unwrap());
    let left: i64 = sqlx::query_scalar("SELECT count(*)::int8 FROM accounts.oidc_clients WHERE client_id = $1")
        .bind(&client)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert_eq!(revocations(&pool, "client_id", &client).await, 1);
}

/// A revoke that waits on a mint's player lock stamps `revoked_at` after that mint
/// commits, though its own transaction began first: every token the mint issued has
/// `iat <= revoked_at`. A stamp of the revoke's `now()` would be earlier than the mint's.
#[tokio::test(flavor = "multi_thread")]
async fn a_player_revocation_waiting_on_a_mint_is_stamped_after_the_mint() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevRace").await;

    let mut revoker = pool.begin().await.unwrap();
    let revoker_now: String = sqlx::query_scalar("SELECT now()::text").fetch_one(&mut *revoker).await.unwrap();
    let mut minter = pool.begin().await.unwrap();
    let mint_now: String = sqlx::query_scalar("SELECT now()::text").fetch_one(&mut *minter).await.unwrap();
    let minter_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *minter).await.unwrap();
    svc.store.insert_family_tx(&mut minter, &pid, TEST_CLIENT_ID).await.unwrap();

    let revoke = tokio::spawn({
        let (svc, pid) = (svc.clone(), pid.clone());
        async move {
            let killed = svc.store.kill_player_families_tx(&mut revoker, &pid).await;
            (revoker, killed)
        }
    });
    assert!(blocked_query(&pool, minter_pid).await.is_some(), "the revoke never waited on the mint");
    minter.commit().await.unwrap();
    let (revoker, killed) = revoke.await.unwrap();
    assert_eq!(killed.unwrap(), Some(2), "the registration family and the minted one");
    revoker.commit().await.unwrap();

    let (revoker_first, after_mint): (bool, bool) = sqlx::query_as(
        "SELECT $3::timestamptz < $2::timestamptz, revoked_at >= $2::timestamptz            FROM accounts.token_revocations WHERE player_id = $1::uuid",
    )
    .bind(&pid)
    .bind(&mint_now)
    .bind(&revoker_now)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(revoker_first, "the revoke's transaction must start before the mint's");
    assert!(after_mint, "revoked_at is before the mint's iat");

    cleanup_player(&pool, &pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn token_state_serves_the_keys_and_the_live_revocations() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (family, player, gone) = (random_uuid(&pool).await, random_uuid(&pool).await, random_uuid(&pool).await);
    let client = format!("t-{}", suffix());
    let base = Tokens::unix_now() - 100;
    sqlx::query(
        "INSERT INTO accounts.token_revocations (family_id, player_id, client_id, revoked_at, expires_at)          VALUES ($1::uuid, NULL, NULL, now(), now() + interval '1 hour'),                 (NULL, $2::uuid, NULL, to_timestamp($4::float8 - 50), now() + interval '1 hour'),                 (NULL, $2::uuid, NULL, to_timestamp($4::float8 + 0.25), now() + interval '1 hour'),                 (NULL, NULL, $3, to_timestamp($4::float8), now() + interval '1 hour'),                 (NULL, $5::uuid, NULL, now() - interval '2 hours', now() - interval '1 second')",
    )
    .bind(&family)
    .bind(&player)
    .bind(&client)
    .bind(base as f64)
    .bind(&gone)
    .execute(&pool)
    .await
    .unwrap();

    let state = svc.token_state().await.unwrap();
    assert_eq!(state.issuer, accountsapi::DEFAULT_OIDC_ISSUER);
    let active: String = sqlx::query_scalar("SELECT kid FROM accounts.signing_keys WHERE status = 'active'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(state.keys.iter().any(|k| k.kid == active), "the active key {active} is missing");
    assert!(state.revoked_families.contains(&family));
    let cut_off = |list: &[accountsapi::Revoked], id: &str| {
        list.iter().filter(|r| r.id == id).map(|r| r.revoked_at_unix).collect::<Vec<_>>()
    };
    assert_eq!(cut_off(&state.revoked_players, &player), [base + 1], "two rows fold into the later, rounded up");
    assert_eq!(cut_off(&state.revoked_clients, &client), [base]);
    assert!(cut_off(&state.revoked_players, &gone).is_empty(), "an expired revocation was served");

    let tokens = &svc.tokens;
    let sid = random_uuid(&pool).await;
    for (sub, client_id, cut) in [(&player, TEST_CLIENT_ID, base + 1), (&sid, client.as_str(), base)] {
        let at_cut = tokens.claims(sub, accountsapi::SWAROG_AUDIENCE, client_id, &sid, cut);
        let refused = tokens.sign(&at_cut).await.unwrap();
        assert_eq!(Tokens::verify_against(&state, &refused, cut), None, "iat == revoked_at verified");
        let after = tokens.claims(sub, accountsapi::SWAROG_AUDIENCE, client_id, &sid, cut + 1);
        let accepted = tokens.sign(&after).await.unwrap();
        assert_eq!(Tokens::verify_against(&state, &accepted, cut + 1), Some(after), "iat > revoked_at refused");
    }
    let in_family = tokens.claims(&sid, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &family, base + 10);
    let revoked_family = tokens.sign(&in_family).await.unwrap();
    assert_eq!(Tokens::verify_against(&state, &revoked_family, base + 10), None, "a revoked family verified");

    sqlx::query(
        "DELETE FROM accounts.token_revocations           WHERE family_id = $1::uuid OR player_id IN ($2::uuid, $4::uuid) OR client_id = $3",
    )
    .bind(&family)
    .bind(&player)
    .bind(&client)
    .bind(&gone)
    .execute(&pool)
    .await
    .unwrap();
}

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
    let mut conn = pool.acquire().await.unwrap();

    let fresh = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now);
    let token = tokens.sign(&mut conn, &fresh).await.unwrap();
    assert_eq!(tokens.verify_access(&token).await.unwrap(), Some(fresh.clone()));

    let stale = tokens.claims(&sub, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &sid, now - 3600);
    let expired = tokens.sign(&mut conn, &stale).await.unwrap();
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
    let mut conn = pool.acquire().await.unwrap();
    for (sub, client_id, cut) in [(&player, TEST_CLIENT_ID, base + 1), (&sid, client.as_str(), base)] {
        let at_cut = tokens.claims(sub, accountsapi::SWAROG_AUDIENCE, client_id, &sid, cut);
        let refused = tokens.sign(&mut conn, &at_cut).await.unwrap();
        assert_eq!(Tokens::verify_against(&state, &refused, cut), None, "iat == revoked_at verified");
        let after = tokens.claims(sub, accountsapi::SWAROG_AUDIENCE, client_id, &sid, cut + 1);
        let accepted = tokens.sign(&mut conn, &after).await.unwrap();
        assert_eq!(Tokens::verify_against(&state, &accepted, cut + 1), Some(after), "iat > revoked_at refused");
    }
    let in_family = tokens.claims(&sid, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &family, base + 10);
    let revoked_family = tokens.sign(&mut conn, &in_family).await.unwrap();
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

/// A pool of one connection, so a mint that asked the pool for a second connection while
/// holding its transaction's would wait out `acquire_timeout` and fail. The register reads
/// the empty key cache and the refresh one stale past the cache TTL; both read it on their
/// own transaction.
#[tokio::test(flavor = "multi_thread")]
async fn a_mint_and_a_refresh_read_a_stale_signing_key_on_their_own_connection() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    let Some(pool) = test_pool().await else { return };
    let (ctx, _svc) = wired(&pool).await;
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let one = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dsn)
        .await
        .unwrap();
    let base = Instant::now();
    let elapsed = Arc::new(AtomicU64::new(0));
    let clock: crate::signing_keys::Clock = {
        let elapsed = elapsed.clone();
        Arc::new(move || base + Duration::from_secs(elapsed.load(Ordering::SeqCst)))
    };
    let svc = Service {
        tokens: Tokens::with_clock(
            one.clone(),
            "accounts",
            accountsapi::DEFAULT_OIDC_ISSUER.to_string(),
            Arc::new(OnceLock::from(dev_kek())),
            clock,
        ),
        store: Store { pool: one },
        bus: ctx.bus().clone(),
        open_registration: true,
        providers: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    };

    let sess = svc
        .register(format!("one-conn-{}@test.local", suffix()), "pw".into(), "oneconn".into(), test_client())
        .await
        .unwrap();
    elapsed.store(crate::jwt::ACTIVE_KEY_CACHE_TTL.as_secs() + 1, Ordering::SeqCst);
    let next = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    assert!(svc.tokens.verify_access(&next.token).await.unwrap().is_some(), "the refreshed token did not verify");
}

/// Writes a live cut-off for player `player` or client `client` stamped `ahead` seconds past the
/// database clock (a revoke stamped while the app clock lags it), and answers it rounded up
/// to the second, as every verifier compares it.
async fn cut_off(pool: &PgPool, player: Option<&str>, client: Option<&str>, ahead: f64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO accounts.token_revocations (player_id, client_id, revoked_at, expires_at) \
         VALUES ($1::uuid, $2, now() + make_interval(secs => $3), now() + interval '1 hour') \
         RETURNING ceil(extract(epoch FROM revoked_at))::int8",
    )
    .bind(player)
    .bind(client)
    .bind(ahead)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Asserts that a login and a refresh after a cut-off `cut` mint tokens that verify, and that
/// tokens of `sub` and `client_id` issued at or before it stay refused.
async fn assert_minted_past(svc: &Service, cut: i64, sub: &str, client_id: &str, email: &str) {
    let login = svc.login(email.to_string(), "pw".into(), client_id.to_string()).await.unwrap();
    let refreshed = svc.refresh(login.refresh_token.clone()).await.unwrap();
    for (what, token) in [("login", &login.token), ("refresh", &refreshed.token)] {
        let claims = svc.tokens.verify_access(token).await.unwrap();
        let claims = claims.unwrap_or_else(|| panic!("the {what} after the cut-off minted a refused token"));
        assert!(claims.iat > cut, "the {what} token's iat is not past the cut-off");
        assert_eq!(claims.exp, claims.iat + crate::jwt::ACCESS_TTL_SECS);
    }
    let access_expiry: i64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM f.access_expires_at)::int8 FROM accounts.token_families f \
           JOIN accounts.refresh_tokens t ON t.family_id = f.id WHERE t.token = $1",
    )
    .bind(&refreshed.refresh_token)
    .fetch_one(&svc.store.pool)
    .await
    .unwrap();
    let refreshed_claims = svc.tokens.verify_access(&refreshed.token).await.unwrap().unwrap();
    assert_eq!(access_expiry, refreshed_claims.exp, "the family's access expiry is not its token's exp");

    let mut conn = svc.store.pool.acquire().await.unwrap();
    for iat in [cut - 100, cut] {
        let sid = random_uuid(&svc.store.pool).await;
        let claims = svc.tokens.claims(sub, accountsapi::SWAROG_AUDIENCE, client_id, &sid, iat);
        let before = svc.tokens.sign(&mut conn, &claims).await.unwrap();
        assert_eq!(svc.tokens.verify_access(&before).await.unwrap(), None, "a token issued at {iat} verified");
    }
}

/// A login and a refresh right after a player revocation stamped ahead of the app clock
/// mint a token that verifies: its `iat` is raised past the rounded-up cut-off, while a
/// token issued at or before the cut-off stays refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_mint_after_a_player_cut_off_verifies_and_one_before_it_stays_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("cut-player-{}@test.local", suffix());
    let pid = svc.register(email.clone(), "pw".into(), "CutPlayer".into(), test_client()).await.unwrap().player_id;
    let cut = cut_off(&pool, Some(&pid), None, 5.0).await;
    assert_minted_past(&svc, cut, &pid, TEST_CLIENT_ID, &email).await;
    cleanup_player(&pool, &pid).await;
}

/// The same for a client cut-off: a login and a refresh through that client mint a token
/// that verifies, and a token of the client issued at or before the cut-off stays refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_mint_after_a_client_cut_off_verifies_and_one_before_it_stays_refused() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let client = insert_client(&pool, &accountsapi::grant_types::ALL[..]).await;
    let email = format!("cut-client-{}@test.local", suffix());
    let pid = svc.register(email.clone(), "pw".into(), "CutClient".into(), client.clone()).await.unwrap().player_id;
    let cut = cut_off(&pool, None, Some(&client), 5.0).await;
    assert_minted_past(&svc, cut, &pid, &client, &email).await;
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client).await;
}

/// A second player revocation in the same second as the first still refuses a token minted
/// between them: that token's `iat` is one second past the first cut-off, so the second is
/// stamped at least one second past the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_player_revocation_refuses_a_token_minted_past_the_first() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("cut-twice-{}@test.local", suffix());
    let pid = svc.register(email.clone(), "pw".into(), "CutTwice".into(), test_client()).await.unwrap().player_id;
    cut_off(&pool, Some(&pid), None, 5.0).await;
    let between = svc.login(email, "pw".into(), test_client()).await.unwrap();
    assert!(svc.tokens.verify_access(&between.token).await.unwrap().is_some());

    let mut tx = pool.begin().await.unwrap();
    assert!(svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap().is_some());
    tx.commit().await.unwrap();
    assert_eq!(
        svc.tokens.verify_access(&between.token).await.unwrap(),
        None,
        "a token minted past the first cut-off survived the second"
    );
    cleanup_player(&pool, &pid).await;
}

/// Leaves player `player_id` one family whose refresh token has expired and whose access
/// token expired `ago` ago.
async fn expire_tokens(pool: &PgPool, player_id: &str, ago: &str) {
    sqlx::query("UPDATE accounts.refresh_tokens SET expires_at = now() - interval '1 second' WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts.token_families SET access_expires_at = now() - $2::interval WHERE player_id = $1::uuid")
        .bind(player_id)
        .bind(ago)
        .execute(pool)
        .await
        .unwrap();
}

/// A family whose access token expired inside `ACCESS_EXP_LEEWAY` still verifies, so the
/// prune keeps it and Revoke all counts it and writes the player row; past the leeway it
/// counts nothing.
#[tokio::test(flavor = "multi_thread")]
async fn revoke_all_counts_a_family_whose_token_is_inside_the_exp_leeway() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevLeeway").await;
    expire_tokens(&pool, &pid, "10 seconds").await;

    let mut prune = pool.begin().await.unwrap();
    svc.store.prune_expired_token_rows(&mut prune).await.unwrap();
    prune.commit().await.unwrap();
    let families: i64 = sqlx::query_scalar("SELECT count(*)::int8 FROM accounts.token_families WHERE player_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(families, 1, "the prune deleted a family whose access token still verifies");

    let mut tx = pool.begin().await.unwrap();
    let revoked = svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(revoked, Some(1), "Revoke all skipped a family inside the leeway");
    assert_eq!(revocations(&pool, "player_id", &pid).await, 1);
    cleanup_player(&pool, &pid).await;

    let (past, _) = registered(&svc, "RevPastLeeway").await;
    expire_tokens(&pool, &past, "1 minute").await;
    let mut tx = pool.begin().await.unwrap();
    let revoked = svc.store.revoke_player_families_tx(&mut tx, &past).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(revoked, Some(0), "a family past the leeway still counted");
    cleanup_player(&pool, &past).await;
}

/// A login whose `iat` a cut-off of one kind raised past the clock is refused by a later
/// revocation of the other kind stamped by the clock: a client delete after a player cut-off,
/// and a player revocation after a client cut-off. Each revocation is stamped at or past the
/// newest `iat` of the families it covers.
#[tokio::test(flavor = "multi_thread")]
async fn a_revocation_refuses_a_mint_raised_past_the_other_kinds_cut_off() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let client = insert_client(&pool, &accountsapi::grant_types::ALL[..]).await;
    let email = format!("cross-client-{}@test.local", suffix());
    let pid = svc.register(email.clone(), "pw".into(), "CrossClient".into(), client.clone()).await.unwrap().player_id;
    let cut = cut_off(&pool, Some(&pid), None, 600.0).await;
    let raised = svc.login(email, "pw".into(), client.clone()).await.unwrap();
    let claims = svc.tokens.verify_access(&raised.token).await.unwrap().expect("the raised login did not verify");
    assert_eq!(claims.iat, cut + 1, "the player cut-off did not raise the login's iat");
    assert!(svc.store.delete_client(&client).await.unwrap());
    assert_eq!(
        svc.tokens.verify_access(&raised.token).await.unwrap(),
        None,
        "a client delete let through a token its player cut-off raised past the clock"
    );
    cleanup_player(&pool, &pid).await;

    let client = insert_client(&pool, &accountsapi::grant_types::ALL[..]).await;
    let email = format!("cross-player-{}@test.local", suffix());
    let pid = svc.register(email.clone(), "pw".into(), "CrossPlayer".into(), client.clone()).await.unwrap().player_id;
    let cut = cut_off(&pool, None, Some(&client), 600.0).await;
    let raised = svc.login(email, "pw".into(), client.clone()).await.unwrap();
    let claims = svc.tokens.verify_access(&raised.token).await.unwrap().expect("the raised login did not verify");
    assert_eq!(claims.iat, cut + 1, "the client cut-off did not raise the login's iat");
    let mut tx = pool.begin().await.unwrap();
    assert!(svc.store.kill_player_families_tx(&mut tx, &pid).await.unwrap().is_some());
    tx.commit().await.unwrap();
    assert_eq!(
        svc.tokens.verify_access(&raised.token).await.unwrap(),
        None,
        "a player revocation let through a token its client cut-off raised past the clock"
    );
    cleanup_player(&pool, &pid).await;
    delete_client(&pool, &client).await;
}

/// A family whose access token is in its last accepted second, `exp` plus
/// `ACCESS_EXP_LEEWAY` with the clock floored as every verifier floors it, still verifies:
/// the prune keeps it and Revoke all counts it. One transaction pins `now()` for the stamp,
/// the prune and the revoke, so no real clock is raced.
#[tokio::test(flavor = "multi_thread")]
async fn revoke_all_counts_a_family_in_the_last_second_of_the_exp_leeway() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (pid, _) = registered(&svc, "RevBoundary").await;
    let leeway = accountsapi::ACCESS_EXP_LEEWAY.as_secs() as i64;

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE accounts.refresh_tokens SET expires_at = now() - interval '1 second' WHERE player_id = $1::uuid")
        .bind(&pid)
        .execute(&mut *tx)
        .await
        .unwrap();
    let (family, exp, now): (String, i64, i64) = sqlx::query_as(
        "UPDATE accounts.token_families \
            SET access_expires_at = date_trunc('second', now()) - $2::int8 * interval '1 second' \
          WHERE player_id = $1::uuid \
          RETURNING id::text, extract(epoch FROM access_expires_at)::int8, \
                    floor(extract(epoch FROM now()))::int8",
    )
    .bind(&pid)
    .bind(leeway)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(now, exp + leeway);
    let claims = svc.tokens.claims(&pid, accountsapi::SWAROG_AUDIENCE, TEST_CLIENT_ID, &family, exp - crate::jwt::ACCESS_TTL_SECS);
    let token = svc.tokens.sign(&mut tx, &claims).await.unwrap();
    let snapshot = svc.tokens.snapshot().await.unwrap();
    assert!(Tokens::verify_against(&snapshot, &token, now).is_some(), "the boundary second's token did not verify");

    svc.store.prune_expired_token_rows(&mut tx).await.unwrap();
    let families: i64 = sqlx::query_scalar("SELECT count(*)::int8 FROM accounts.token_families WHERE player_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(families, 1, "the prune deleted a family in the last second its token verifies");
    let revoked = svc.store.revoke_player_families_tx(&mut tx, &pid).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(revoked, Some(1), "Revoke all skipped a family in the last second of the leeway");
    cleanup_player(&pool, &pid).await;
}

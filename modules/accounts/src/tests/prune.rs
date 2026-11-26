//! The durable session-prune reaction: accounts reacts to
//! `scheduler.fired{name:"accounts-sessions-prune"}` and deletes expired sessions on the
//! delivery tx. The [`PruneHandler`] is driven directly against a real sqlx tx (the same
//! shape the asyncevents plane's consume runs the handler in), so these exercise the
//! DELETE + name-filter without the transport internals. A child of `tests` so it reuses
//! that module's live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`) and its
//! once-per-binary schema serialization.

use super::*;

/// The token family `refresh_token` belongs to.
async fn family_of(pool: &PgPool, refresh_token: &str) -> String {
    sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
        .bind(refresh_token)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Inserts a session row for `player_id` in family `family_id` with an explicit
/// `expires_at` relative to now (positive = future/live, negative = past/expired),
/// returning its token.
async fn insert_session(pool: &PgPool, player_id: &str, family_id: &str, offset_days: i32) -> String {
    let token = store::new_token();
    sqlx::query(
        "INSERT INTO accounts.sessions (token, player_id, family_id, expires_at) \
         VALUES ($1, $2::uuid, $3::uuid, now() + make_interval(days => $4))",
    )
    .bind(&token)
    .bind(player_id)
    .bind(family_id)
    .bind(offset_days)
    .execute(pool)
    .await
    .expect("insert session");
    token
}

/// Whether a session with `token` still exists.
async fn session_exists(pool: &PgPool, token: &str) -> bool {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*)::int8 FROM accounts.sessions WHERE token = $1")
            .bind(token)
            .fetch_one(pool)
            .await
            .unwrap();
    n == 1
}

/// Drives the prune handler with a `scheduler.fired{name}` payload inside a committed tx —
/// the same shape the asyncevents plane's consume runs the handler in.
async fn deliver_prune(pool: &PgPool, handler: &PruneHandler, name: &str) {
    let payload = serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let delivery = Delivery {
        event_id: "accounts:test",
        tx: bus::AnyTx::new(&mut *tx),
    };
    handler.call(delivery, payload).await.unwrap();
    tx.commit().await.unwrap();
}

/// The prune reaction deletes ONLY expired sessions, and ONLY for the
/// `accounts-sessions-prune` schedule name; a live session survives, and a foreign
/// schedule name is a committed no-op.
#[tokio::test(flavor = "multi_thread")]
async fn prune_deletes_only_expired_sessions_for_prune_name() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    // A real player (register mints one) supplies the sessions FK target.
    let email = format!("prune-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Prune".into(), test_client()).await.unwrap();
    let pid = sess.player_id.clone();
    let family = family_of(&pool, &sess.refresh_token).await;

    let expired = insert_session(&pool, &pid, &family, -1).await; // expires_at 1 day in the past
    let live = insert_session(&pool, &pid, &family, 30).await; // still valid

    let handler = PruneHandler { svc: svc.clone() };

    // A non-prune schedule name must NOT prune (proves the name filter) — still commits.
    deliver_prune(&pool, &handler, "some-other-job").await;
    assert!(
        session_exists(&pool, &expired).await,
        "foreign schedule name pruned an expired session"
    );

    // accounts-sessions-prune: the expired session goes, the live one stays.
    deliver_prune(&pool, &handler, PRUNE_SCHEDULE_NAME).await;
    assert!(
        !session_exists(&pool, &expired).await,
        "expired session survived prune"
    );
    assert!(
        session_exists(&pool, &live).await,
        "live session was pruned"
    );

    cleanup_player(&pool, &pid).await;
}

/// The delete-ticket leg of the same prune reaction: an EXPIRED delete ticket is removed
/// by `accounts-sessions-prune`; a LIVE one is not. Pre-fix the prune handler only touched
/// sessions/refresh tokens, so an expired ticket row would accumulate forever.
#[tokio::test(flavor = "multi_thread")]
async fn prune_deletes_expired_delete_ticket_keeps_live_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let expired_sess = svc
        .register(format!("prune-ticket-exp-{}@test.local", suffix()), "pw".into(), "PruneTickExp".into(), test_client())
        .await
        .unwrap();
    let live_sess = svc
        .register(format!("prune-ticket-live-{}@test.local", suffix()), "pw".into(), "PruneTickLive".into(), test_client())
        .await
        .unwrap();
    let expired_pid = expired_sess.player_id.clone();
    let live_pid = live_sess.player_id.clone();

    let expired_ticket = svc.begin_delete(Identity::player(&expired_pid)).await.unwrap().ticket;
    let live_ticket = svc.begin_delete(Identity::player(&live_pid)).await.unwrap().ticket;

    sqlx::query(
        "UPDATE accounts.delete_tickets SET expires_at = now() - interval '1 minute' \
          WHERE player_id = $1::uuid",
    )
    .bind(&expired_pid)
    .execute(&pool)
    .await
    .unwrap();

    let handler = PruneHandler { svc: svc.clone() };
    deliver_prune(&pool, &handler, PRUNE_SCHEDULE_NAME).await;

    let (expired_n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM accounts.delete_tickets WHERE player_id = $1::uuid AND ticket = $2",
    )
    .bind(&expired_pid)
    .bind(&expired_ticket)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expired_n, 0, "an expired delete ticket must be pruned");

    let (live_n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM accounts.delete_tickets WHERE player_id = $1::uuid AND ticket = $2",
    )
    .bind(&live_pid)
    .bind(&live_ticket)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live_n, 1, "a live delete ticket must survive the prune");

    cleanup_player(&pool, &expired_pid).await;
    cleanup_player(&pool, &live_pid).await;
}

/// A foreign schedule name is a committed no-op AT THE HANDLER BOUNDARY (returns Ok, the
/// tick is marked processed, nothing deleted) even with an expired session present — the
/// filter is the whole point of subscribing raw to `scheduler.fired`.
#[tokio::test(flavor = "multi_thread")]
async fn foreign_schedule_name_is_a_committed_noop() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let email = format!("noop-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Noop".into(), test_client()).await.unwrap();
    let pid = sess.player_id.clone();
    let family = family_of(&pool, &sess.refresh_token).await;
    let expired = insert_session(&pool, &pid, &family, -1).await;

    let handler = PruneHandler { svc: svc.clone() };
    // Succeeds (no panic/err) AND leaves the expired row intact.
    deliver_prune(&pool, &handler, "audit-prune").await;
    assert!(
        session_exists(&pool, &expired).await,
        "a foreign schedule name must not prune sessions"
    );

    cleanup_player(&pool, &pid).await;
}

/// The browser-session leg of the prune reaction: an expired browser session is already
/// absent to the lookup, and `accounts-sessions-prune` deletes its row; a live one stays.
#[tokio::test(flavor = "multi_thread")]
async fn prune_deletes_expired_browser_session_keeps_live_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("prune-browser-{}@test.local", suffix());
    let pid = svc.register(email, "pw".into(), "PruneBrowser".into(), test_client()).await.unwrap().player_id;
    let expired = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    let live = svc.store.create_browser_session(&pid).await.unwrap().unwrap();
    sqlx::query(
        "UPDATE accounts.browser_sessions SET expires_at = now() - interval '1 minute' \
          WHERE csrf_token = $1",
    )
    .bind(&expired.csrf_token)
    .execute(&pool)
    .await
    .unwrap();

    let expired_read = svc.store.browser_session(&expired.token).await.unwrap();
    let handler = PruneHandler { svc: svc.clone() };
    deliver_prune(&pool, &handler, PRUNE_SCHEDULE_NAME).await;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT csrf_token FROM accounts.browser_sessions WHERE player_id = $1::uuid",
    )
    .bind(&pid)
    .fetch_all(&pool)
    .await
    .unwrap();
    let live_read = svc.store.browser_session(&live.token).await.unwrap();
    cleanup_player(&pool, &pid).await;

    assert!(expired_read.is_none(), "an expired browser session is not returned");
    assert_eq!(rows, vec![live.csrf_token.clone()], "the prune removes only the expired row");
    assert_eq!(live_read.map(|s| s.player_id), Some(pid), "the live session still resolves");
}

/// The token-family leg of the prune reaction: a family whose access lapsed and whose
/// refresh tokens all expired goes; one still holding a live refresh token stays, and so
/// does one whose access token is still live though its refresh tokens expired.
#[tokio::test(flavor = "multi_thread")]
async fn prune_deletes_a_family_only_once_nothing_in_it_grants_access() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("prune-fam-{}@test.local", suffix());
    let dead = svc.register(email.clone(), "pw".into(), "PruneFam".into(), test_client()).await.unwrap();
    let refreshable = svc.login(email.clone(), "pw".into(), test_client()).await.unwrap();
    let live_access = svc.login(email, "pw".into(), test_client()).await.unwrap();
    let pid = dead.player_id.clone();
    let dead_family = family_of(&pool, &dead.refresh_token).await;
    let refreshable_family = family_of(&pool, &refreshable.refresh_token).await;
    let live_access_family = family_of(&pool, &live_access.refresh_token).await;
    sqlx::query(
        "UPDATE accounts.token_families SET access_expires_at = now() - interval '1 minute' \
          WHERE id = ANY($1::text[]::uuid[])",
    )
    .bind(&[dead_family.as_str(), refreshable_family.as_str()][..])
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE accounts.refresh_tokens SET expires_at = now() - interval '1 minute' \
          WHERE family_id = ANY($1::text[]::uuid[])",
    )
    .bind(&[dead_family.as_str(), live_access_family.as_str()][..])
    .execute(&pool)
    .await
    .unwrap();

    let handler = PruneHandler { svc: svc.clone() };
    deliver_prune(&pool, &handler, PRUNE_SCHEDULE_NAME).await;
    let mut left: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM accounts.token_families WHERE player_id = $1::uuid",
    )
    .bind(&pid)
    .fetch_all(&pool)
    .await
    .unwrap();
    cleanup_player(&pool, &pid).await;

    left.sort();
    let mut kept = vec![refreshable_family, live_access_family];
    kept.sort();
    assert_eq!(left, kept, "only the family with nothing live is pruned");
}

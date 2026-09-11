//! The durable session-prune reaction: accounts reacts to
//! `scheduler.fired{name:"accounts-sessions-prune"}` and deletes expired sessions on the
//! delivery tx. The [`PruneHandler`] is driven directly against a real sqlx tx (the same
//! shape the asyncevents plane's consume runs the handler in), so these exercise the
//! DELETE + name-filter without the transport internals. A child of `tests` so it reuses
//! that module's live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`) and its
//! once-per-binary schema serialization.

use super::*;

/// Inserts a session row for `player_id` with an explicit `expires_at` relative to now
/// (positive = future/live, negative = past/expired), returning its token.
async fn insert_session(pool: &PgPool, player_id: &str, offset_days: i32) -> String {
    let token = store::new_token();
    sqlx::query(
        "INSERT INTO accounts.sessions (token, player_id, expires_at) \
         VALUES ($1, $2::uuid, now() + make_interval(days => $3))",
    )
    .bind(&token)
    .bind(player_id)
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
    let sess = svc.register(email, "pw".into(), "Prune".into()).await.unwrap();
    let pid = sess.player_id.clone();

    let expired = insert_session(&pool, &pid, -1).await; // expires_at 1 day in the past
    let live = insert_session(&pool, &pid, 30).await; // still valid

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
        .register(format!("prune-ticket-exp-{}@test.local", suffix()), "pw".into(), "PruneTickExp".into())
        .await
        .unwrap();
    let live_sess = svc
        .register(format!("prune-ticket-live-{}@test.local", suffix()), "pw".into(), "PruneTickLive".into())
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
    let sess = svc.register(email, "pw".into(), "Noop".into()).await.unwrap();
    let pid = sess.player_id.clone();
    let expired = insert_session(&pool, &pid, -1).await;

    let handler = PruneHandler { svc: svc.clone() };
    // Succeeds (no panic/err) AND leaves the expired row intact.
    deliver_prune(&pool, &handler, "audit-prune").await;
    assert!(
        session_exists(&pool, &expired).await,
        "a foreign schedule name must not prune sessions"
    );

    cleanup_player(&pool, &pid).await;
}

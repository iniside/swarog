//! Refresh-token rotation and reuse detection: `Auth::refresh` and its single
//! authority, `store::classify_presentation`. A child of `tests` so it reuses that
//! module's live-DB harness (`test_pool`/`wired`/`suffix`/`cleanup_player`) and its
//! once-per-binary schema serialization. Live-DB tests SKIP cleanly when Postgres is
//! unreachable.

use super::*;

/// Raw facts about one refresh row, read directly with hand-written SQL rather than
/// through `Store::refresh_row_tx` — the function under test must never also be the
/// oracle checking it.
struct Row {
    family_id: String,
    replaced_by: Option<String>,
    used: bool,
    expires_at_epoch: f64,
}

async fn refresh_row(pool: &PgPool, token: &str) -> Option<Row> {
    let row: Option<(String, Option<String>, bool, f64)> = sqlx::query_as(
        "SELECT family_id::text, replaced_by, used_at IS NOT NULL, \
                EXTRACT(EPOCH FROM expires_at)::float8 \
           FROM accounts.refresh_tokens WHERE token = $1",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .unwrap();
    row.map(|(family_id, replaced_by, used, expires_at_epoch)| Row {
        family_id,
        replaced_by,
        used,
        expires_at_epoch,
    })
}

async fn refresh_row_count(pool: &PgPool, token: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.refresh_tokens WHERE token = $1")
        .bind(token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn session_row_count(pool: &PgPool, token: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.sessions WHERE token = $1")
        .bind(token)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_refresh_count(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.refresh_tokens WHERE family_id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn family_session_count(pool: &PgPool, family_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts.sessions WHERE family_id = $1::uuid")
        .bind(family_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Rewrites `used_at` relative to now — the boundary control for the grace window.
/// Negative = in the past (outside grace once beyond -30s); small negative = still
/// inside it. Never a sleep.
async fn set_used_at_offset(pool: &PgPool, token: &str, offset_seconds: f64) {
    sqlx::query(
        "UPDATE accounts.refresh_tokens SET used_at = now() + make_interval(secs => $2) \
         WHERE token = $1",
    )
    .bind(token)
    .bind(offset_seconds)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_expires_at_offset(pool: &PgPool, token: &str, offset_seconds: f64) {
    sqlx::query(
        "UPDATE accounts.refresh_tokens SET expires_at = now() + make_interval(secs => $2) \
         WHERE token = $1",
    )
    .bind(token)
    .bind(offset_seconds)
    .execute(pool)
    .await
    .unwrap();
}

/// Happy rotation: `store::classify_presentation`'s `Rotate` branch. The consumed row
/// records its successor and the new access token verifies.
#[tokio::test]
async fn rotation_kills_the_old_token_and_the_successor_works() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rot-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Rot".into()).await.unwrap();
    let pid = sess.player_id.clone();

    let next = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    assert_eq!(next.player_id, pid);
    assert_ne!(next.token, sess.token, "rotation must mint a fresh access token");
    assert_ne!(
        next.refresh_token, sess.refresh_token,
        "rotation must mint a fresh refresh token"
    );

    let old = refresh_row(&pool, &sess.refresh_token)
        .await
        .expect("the consumed row is retained as reuse-detection evidence");
    assert!(old.used, "old refresh token must be consumed");
    assert_eq!(old.replaced_by.as_deref(), Some(next.refresh_token.as_str()));

    assert_eq!(
        svc.verify_session(next.token.clone()).await.unwrap(),
        Some(pid.clone()),
        "the new access token must verify"
    );

    cleanup_player(&pool, &pid).await;
}

/// The most important test in the step: `store::RefreshVerdict::Revoke` scopes the
/// kill to the FAMILY, never the player. Two families minted for one player, a
/// consumed token in family A replayed outside the grace window — family A is wiped
/// on BOTH tables, family B (the player's other login) verifies on both untouched.
/// A naive `WHERE player_id = $1` kill would pass every assertion except the
/// family-B ones — see the perturbation note in the task report.
#[tokio::test]
async fn replay_outside_grace_kills_only_its_own_family() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("fam-{}@test.local", suffix());
    let sess_a = svc.register(email.clone(), "pw".into(), "Fam".into()).await.unwrap();
    let pid = sess_a.player_id.clone();
    let sess_b = svc.login(email, "pw".into()).await.unwrap();
    assert_ne!(
        sess_a.refresh_token, sess_b.refresh_token,
        "login must mint a SECOND family, not extend the first"
    );

    let family_a = refresh_row(&pool, &sess_a.refresh_token).await.unwrap().family_id;
    let family_b = refresh_row(&pool, &sess_b.refresh_token).await.unwrap().family_id;
    assert_ne!(family_a, family_b);

    // Consume family A's original token, then simulate its replay arriving too late.
    let rotated_a = svc.refresh(sess_a.refresh_token.clone()).await.unwrap();
    set_used_at_offset(&pool, &sess_a.refresh_token, -60.0).await;

    let err = svc.refresh(sess_a.refresh_token.clone()).await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unauthorized);

    // Family A: every refresh row (parent + the successor it minted) and every
    // access session descended from it are gone.
    assert_eq!(family_refresh_count(&pool, &family_a).await, 0, "family A refresh rows must be gone");
    assert_eq!(family_session_count(&pool, &family_a).await, 0, "family A access sessions must be gone");
    assert_eq!(refresh_row_count(&pool, &rotated_a.refresh_token).await, 0);
    assert_eq!(session_row_count(&pool, &rotated_a.token).await, 0);

    // Family B: the player's OTHER login, untouched — verifies on BOTH tables.
    assert_eq!(family_refresh_count(&pool, &family_b).await, 1, "sibling family's refresh row must survive");
    assert_eq!(family_session_count(&pool, &family_b).await, 1, "sibling family's access session must survive");
    assert_eq!(
        svc.verify_session(sess_b.token.clone()).await.unwrap(),
        Some(pid.clone()),
        "sibling family's access session must still verify"
    );
    assert!(
        refresh_row(&pool, &sess_b.refresh_token).await.is_some(),
        "sibling family's refresh row must still resolve"
    );

    cleanup_player(&pool, &pid).await;
}

/// A consumed token replayed INSIDE the grace window hands back the recorded
/// successor (`RefreshVerdict::GraceReplay`) and revokes nothing — the lost-response
/// case, not theft.
#[tokio::test]
async fn replay_inside_grace_returns_the_recorded_successor_and_kills_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("grace-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Grace".into()).await.unwrap();
    let pid = sess.player_id.clone();

    let rotated = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    set_used_at_offset(&pool, &sess.refresh_token, -5.0).await; // comfortably inside the 30s grace

    let replay = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    assert_eq!(
        replay.refresh_token, rotated.refresh_token,
        "grace replay must hand back the recorded successor, not mint a new one"
    );
    assert_ne!(replay.token, rotated.token, "grace replay still mints a fresh access session");
    assert_eq!(replay.player_id, pid);

    let family = refresh_row(&pool, &sess.refresh_token).await.unwrap().family_id;
    // parent (consumed) + the one successor rotation minted — no second refresh row.
    assert_eq!(family_refresh_count(&pool, &family).await, 2, "grace replay must not mint a second refresh row");
    // register + rotate + grace-replay: three access sessions, nothing revoked.
    assert_eq!(family_session_count(&pool, &family).await, 3, "grace replay must not revoke anything");

    let successor_row = refresh_row(&pool, &rotated.refresh_token).await.unwrap();
    assert!(!successor_row.used, "the grace replay must not itself consume the successor");

    cleanup_player(&pool, &pid).await;
}

/// An expired refresh token is `RefreshVerdict::Deny` via the `row.expired` branch.
#[tokio::test]
async fn expired_refresh_token_is_unauthorized() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("exp-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Exp".into()).await.unwrap();
    let pid = sess.player_id.clone();

    set_expires_at_offset(&pool, &sess.refresh_token, -60.0).await;

    let err = svc.refresh(sess.refresh_token.clone()).await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unauthorized);

    cleanup_player(&pool, &pid).await;
}

/// Unknown and expired tokens answer with the SAME `Unauthorized` — no oracle that
/// would tell a holder which kind of invalid token it has.
#[tokio::test]
async fn unknown_refresh_token_is_byte_identical_to_expired() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("unk-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Unk".into()).await.unwrap();
    let pid = sess.player_id.clone();

    set_expires_at_offset(&pool, &sess.refresh_token, -60.0).await;
    let expired_err = svc.refresh(sess.refresh_token.clone()).await.unwrap_err();
    let unknown_err = svc.refresh(store::new_token()).await.unwrap_err();

    assert_eq!(expired_err.status, unknown_err.status);
    assert_eq!(
        expired_err.msg, unknown_err.msg,
        "expired and unknown refresh tokens must be indistinguishable"
    );
    assert_eq!(unknown_err.status, opsapi::Status::Unauthorized);

    cleanup_player(&pool, &pid).await;
}

/// Two dials presenting the SAME refresh token concurrently: the row-level UPDATE
/// predicate (`used_at IS NULL`) is the whole concurrency story — exactly one wins
/// the rotation, the loser serializes behind it on the row lock and falls into the
/// grace branch (its own `now()` is its own earlier transaction start, well inside
/// the 30s window). The contention is CAUSED, never hoped for: the test opens its own
/// transaction and takes `FOR UPDATE` on the refresh row first, so both dials park in
/// that row's lock queue before either can touch it; releasing the barrier hands the
/// row to one dial and leaves the other waiting on that dial's transaction. Both
/// return 200 with the SAME refresh_token and DIFFERENT access tokens; counting `Ok`s
/// would prove nothing, so this asserts the persisted row state.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_refreshes_produce_exactly_one_rotation() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("race-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Race".into()).await.unwrap();
    let pid = sess.player_id.clone();
    let family = refresh_row(&pool, &sess.refresh_token).await.unwrap().family_id;

    // The barrier: a test-owned transaction holding the very row both dials will
    // UPDATE. Its backend pid is what the queue is measured against below.
    let mut barrier = pool.begin().await.unwrap();
    let barrier_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    let held: String = sqlx::query_scalar(
        "SELECT token FROM accounts.refresh_tokens WHERE token = $1 FOR UPDATE",
    )
    .bind(&sess.refresh_token)
    .fetch_one(&mut *barrier)
    .await
    .unwrap();
    assert_eq!(held, sess.refresh_token, "the barrier must hold the row under test");

    let svc_a = svc.clone();
    let svc_b = svc.clone();
    let t1 = sess.refresh_token.clone();
    let t2 = sess.refresh_token.clone();
    let handle_a = tokio::spawn(async move { svc_a.refresh(t1).await });
    let handle_b = tokio::spawn(async move { svc_b.refresh(t2).await });

    let mut queued = false;
    for _ in 0..1000 {
        // Two waiters on one row queue in two different shapes: the first blocks on
        // the barrier's `transactionid`, the second on the `tuple` lock the first now
        // holds. Both are counted by tracing the block chain two levels back to the
        // barrier's pid.
        let waiting: i64 = sqlx::query_scalar(
            "WITH direct AS ( \
                 SELECT pid FROM pg_stat_activity \
                  WHERE datname = current_database() \
                    AND wait_event_type = 'Lock' \
                    AND $1::int = ANY(pg_blocking_pids(pid)) \
             ) \
             SELECT count(*) FROM pg_stat_activity a \
              WHERE a.datname = current_database() \
                AND a.wait_event_type = 'Lock' \
                AND ($1::int = ANY(pg_blocking_pids(a.pid)) \
                     OR EXISTS (SELECT 1 FROM direct d WHERE d.pid = ANY(pg_blocking_pids(a.pid))))",
        )
        .bind(barrier_pid)
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting >= 2 {
            queued = true;
            break;
        }
        // Neither dial can complete while the barrier holds the row, so this only
        // trips when the barrier itself failed to take the lock.
        if handle_a.is_finished() && handle_b.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Releasing the barrier grants the row to exactly one dial; the other is still in
    // the queue and now waits on THAT dial's transaction.
    barrier.rollback().await.unwrap();

    let r_a = handle_a.await.unwrap().unwrap();
    let r_b = handle_b.await.unwrap().unwrap();

    assert!(
        queued,
        "the barrier never parked BOTH dials in the row's lock queue — the fixture failed to \
         serialize them, so the assertions below would not be about a contended row"
    );
    assert_eq!(r_a.refresh_token, r_b.refresh_token, "both callers must converge on one successor");
    assert_ne!(r_a.token, r_b.token, "two distinct access sessions must be minted");

    let rotated_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts.refresh_tokens \
          WHERE family_id = $1::uuid AND replaced_by IS NOT NULL",
    )
    .bind(&family)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rotated_rows, 1, "exactly one row may record a rotation");
    assert_eq!(family_refresh_count(&pool, &family).await, 2, "parent + exactly one successor");

    cleanup_player(&pool, &pid).await;
}

/// A rotation that never commits (crash mid-transaction, simulated by an explicit
/// `tx.rollback()` on `Store::rotate_refresh_tx` driven directly) must leave the
/// presented token unconsumed — the one-transaction invariant `Auth::refresh` relies
/// on. Re-proven through the production path: a genuine rotation still succeeds
/// afterward and does not yield the doomed, rolled-back successor.
#[tokio::test]
async fn rolled_back_rotation_leaves_the_original_token_usable() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("rb-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Rb".into()).await.unwrap();
    let pid = sess.player_id.clone();

    let mut tx = svc.store.pool.begin().await.unwrap();
    let doomed_successor = store::new_token();
    let rotated = svc
        .store
        .rotate_refresh_tx(&mut tx, &sess.refresh_token, &doomed_successor)
        .await
        .unwrap();
    assert!(rotated.is_some(), "the doomed rotation must have matched the still-live row");
    tx.rollback().await.unwrap();

    let row = refresh_row(&pool, &sess.refresh_token).await.unwrap();
    assert!(!row.used, "a rolled-back rotation must leave the token unconsumed");

    let next = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    assert_ne!(
        next.refresh_token, doomed_successor,
        "the rolled-back successor must never have been committed"
    );

    cleanup_player(&pool, &pid).await;
}

/// A family's expiry is a hard cap: the successor inherits the parent's `expires_at`
/// verbatim rather than a fresh 30 days, so a rotation late in a family's life
/// cannot slide the family's expiry forward.
#[tokio::test]
async fn successor_inherits_the_parents_expiry_never_a_fresh_thirty_days() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("cap-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "Cap".into()).await.unwrap();
    let pid = sess.player_id.clone();

    // Simulate a family late in its life: 5 minutes from expiry, not 30 days.
    set_expires_at_offset(&pool, &sess.refresh_token, 300.0).await;
    let parent_expiry = refresh_row(&pool, &sess.refresh_token).await.unwrap().expires_at_epoch;

    let next = svc.refresh(sess.refresh_token.clone()).await.unwrap();
    let successor_expiry = refresh_row(&pool, &next.refresh_token).await.unwrap().expires_at_epoch;

    assert!(
        (successor_expiry - parent_expiry).abs() < 1.0,
        "successor must inherit the parent's expiry exactly: parent={parent_expiry} successor={successor_expiry}"
    );
    assert!(
        successor_expiry < parent_expiry + 3600.0,
        "successor must not be minted a fresh 30-day expiry: parent={parent_expiry} successor={successor_expiry}"
    );

    cleanup_player(&pool, &pid).await;
}

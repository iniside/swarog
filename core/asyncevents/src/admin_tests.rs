//! Live-Postgres tests for the operator verbs (house `test_pool()` pattern). The
//! `skip`/`retry` logic lives in the lib exactly so it is testable here without the
//! CLI shell. Events are appended via the plane's own writer; the plane is migrated
//! per test (idempotent).

use super::*;
use std::time::Duration;

async fn test_pool() -> Option<PgPool> {
    let pool = testdb::test_pool().await?;
    let _migrate = crate::store::store_tests::PLANE_META_MIGRATE_GUARD.read().await;
    crate::Plane::new(pool.clone(), testdb::dsn())
        .unwrap()
        .migrate()
        .await
        .expect("migrate the asyncevents plane");
    Some(pool)
}

fn skipped(result: SkipResult) -> SkipOutcome {
    match result {
        SkipResult::Skipped(outcome) => *outcome,
        other => panic!("expected a skip, got {other:?}"),
    }
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}.{}-{}", std::process::id(), nanos)
}

/// Appends `n` events on `topic` and returns their positions in log order.
async fn append_positions(pool: &PgPool, topic: &str, n: usize) -> Vec<(i64, String, i64)> {
    let contract = bus::EventContract {
        // `append` takes `&EventContract`, whose topic is `&'static str`; leaking a
        // per-test topic string is fine in a test binary.
        topic: Box::leak(topic.to_string().into_boxed_str()),
        version: 1,
        history: bus::HistoryPolicy::MinRetention { days: 7 },
    };
    let mut tx = pool.begin().await.unwrap();
    for i in 0..n {
        let payload = format!(r#"{{"n":{i}}}"#);
        crate::store::append(&mut tx, &contract, payload.as_bytes())
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    sqlx::query_as(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column. Positional read, so the alias name
        // is irrelevant to decoding.
        "SELECT generation, producer_xid::text AS producer_xid_text, tie_breaker FROM asyncevents.events \
         WHERE topic = $1 ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(topic)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// A per-call unique `tie_breaker` (nanos + pid derived) so concurrent tests seeding
/// synthetic events at the same `(generation, producer_xid)` never collide on the
/// events PK. Positive by construction (sign bit masked off).
fn unique_tie() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    ((nanos ^ ((std::process::id() as u64) << 40)) as i64) & i64::MAX
}

/// Inserts one synthetic event directly at `(generation=0, producer_xid, tie_breaker)`.
/// `OVERRIDING SYSTEM VALUE` is required because `tie_breaker` is `GENERATED ALWAYS`;
/// pinning `generation = 0` (< the seeded `plane_meta.generation` of 1) makes the row
/// frontier-eligible deterministically, so no `wait_eligible` poll is needed.
async fn insert_synthetic_event(pool: &PgPool, topic: &str, producer_xid: &str, tie_breaker: i64) {
    sqlx::query(
        "INSERT INTO asyncevents.events \
           (generation, producer_xid, tie_breaker, topic, contract_version, payload) \
         OVERRIDING SYSTEM VALUE \
         VALUES (0, $1::xid8, $2, $3, 1, $4::jsonb)",
    )
    .bind(producer_xid)
    .bind(tie_breaker)
    .bind(topic)
    .bind(format!(r#"{{"xid":{producer_xid}}}"#))
    .execute(pool)
    .await
    .unwrap();
}

/// Waits until at least `want` events on `topic` are frontier-eligible.
async fn wait_eligible(pool: &PgPool, topic: &str, want: i64) {
    for _ in 0..200 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM asyncevents.events \
             WHERE topic = $1 AND producer_xid < pg_snapshot_xmin(pg_current_snapshot())",
        )
        .bind(topic)
        .fetch_one(pool)
        .await
        .unwrap();
        if n >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("events on {topic} never became frontier-eligible");
}

async fn insert_sub(
    pool: &PgPool,
    id: &str,
    topic: &str,
    state: &str,
    failures: i32,
    cursor: (i64, String, i64),
) {
    sqlx::query(
        "INSERT INTO asyncevents.subscriptions \
           (subscription_id, topic, contract_version, state, \
            cursor_generation, cursor_xid, cursor_tie, consecutive_failures, next_attempt_at, \
            last_error, spec_hash, start_kind, updated_at) \
         VALUES ($1, $2, 1, $3, $4, $5::xid8, $6, $7, now() + interval '5 min', 'boom', 'h', 'genesis', now())",
    )
    .bind(id)
    .bind(topic)
    .bind(state)
    .bind(cursor.0)
    .bind(cursor.1)
    .bind(cursor.2)
    .bind(failures)
    .execute(pool)
    .await
    .unwrap();
}

/// The event ids on `topic` in log order.
async fn event_ids(pool: &PgPool, topic: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT event_id FROM asyncevents.events WHERE topic = $1 \
         ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(topic)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Puts subscription `id`'s cursor on `(generation 0, xid, tie)` and makes it healthy,
/// as a worker's successful delivery of that event does.
async fn deliver_onto(pool: &PgPool, id: &str, xid: &str, tie: i64) {
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET cursor_generation = 0, cursor_xid = $2::xid8, cursor_tie = $3, \
             consecutive_failures = 0, next_attempt_at = NULL, state = 'active', updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(id)
    .bind(xid)
    .bind(tie)
    .execute(pool)
    .await
    .unwrap();
}

async fn cleanup(pool: &PgPool, topic: &str, id: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

/// `skip` steps past EXACTLY the current failing event: the cursor advances to that
/// event's position (not further), the failure state clears, the reason lands in
/// `last_error`, and the outcome carries the skipped id + payload.
#[tokio::test]
async fn skip_steps_past_one_failing_event_and_records_reason() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.skip");
    let id = unique("sub.skip");
    let pos = append_positions(&pool, &topic, 3).await;
    wait_eligible(&pool, &topic, 1).await;
    // Paused with failures ⇒ eligible for skip; cursor at Genesis (nothing consumed).
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;

    let out = skipped(skip(&pool, &id, "unrecoverable payload", None).await.unwrap());

    // Skipped exactly event 0.
    let (g0, x0, t0) = &pos[0];
    assert_eq!(out.after.cursor, format!("{g0}/{x0}/{t0}"), "cursor must land on event 0 only");
    // jsonb re-renders with spaces (`{"n": 0}`), so compare whitespace-insensitively.
    assert!(
        out.skipped_payload.replace(' ', "").contains("\"n\":0"),
        "payload: {}",
        out.skipped_payload
    );
    assert_eq!(out.after.state, "active", "skip reactivates");
    assert_eq!(out.after.consecutive_failures, 0);
    assert_eq!(out.after.next_attempt_at, None);
    assert!(
        out.after.last_error.as_deref().unwrap().contains("unrecoverable payload"),
        "reason must be recorded: {:?}",
        out.after.last_error
    );
    assert!(out.after.last_error.as_deref().unwrap().contains(&out.skipped_event_id));

    // A second skip (still faulted? no — now active/0-failures) is refused, proving
    // skip never runs away past more than the one failing event on a healthy sub.
    let err = skip(&pool, &id, "again", None).await.unwrap_err();
    assert!(err.to_string().contains("refusing to skip"), "unexpected: {err}");

    cleanup(&pool, &topic, &id).await;
}

/// Regression: `skip` must select the failing event by NUMERIC xid8 order, not the
/// text alias. With two eligible events at producer_xid 999 and 1000 past the cursor,
/// numeric order makes 999 the next (failing) event, so `skip` advances exactly onto
/// it. The old `producer_xid::text AS producer_xid` + bare `ORDER BY` picked 1000
/// (`'1000' < '999'` as text) and skipped the wrong event.
#[tokio::test]
async fn skip_selects_failing_event_by_numeric_xid_order() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.xidorder");
    let id = unique("sub.xidorder");
    let t999 = unique_tie();
    let t1000 = unique_tie();
    // generation 0 ⇒ frontier-eligible vs plane_meta generation 1, so no wait needed.
    insert_synthetic_event(&pool, &topic, "999", t999).await;
    insert_synthetic_event(&pool, &topic, "1000", t1000).await;
    // Paused with failures ⇒ eligible for skip; Genesis cursor (nothing consumed).
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;

    let out = skipped(skip(&pool, &id, "unrecoverable", None).await.unwrap());

    assert_eq!(
        out.after.cursor,
        format!("0/999/{t999}"),
        "skip must advance onto the numerically-next event (999), not 1000"
    );
    assert!(
        out.skipped_payload.replace(' ', "").contains("\"xid\":999"),
        "payload: {}",
        out.skipped_payload
    );

    cleanup(&pool, &topic, &id).await;
}

/// `skip` refuses a healthy (`active`, zero-failure) subscription — it is a
/// poison-recovery verb, not a fast-forward.
#[tokio::test]
async fn skip_refuses_a_healthy_subscription() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.healthy");
    let id = unique("sub.healthy");
    append_positions(&pool, &topic, 1).await;
    insert_sub(&pool, &id, &topic, "active", 0, (0, "0".to_string(), 0)).await;

    let err = skip(&pool, &id, "no", None).await.unwrap_err();
    assert!(matches!(&err, VerbError::Healthy { id: e } if *e == id), "unexpected: {err}");
    assert!(err.to_string().contains("refusing to skip"), "unexpected: {err}");

    cleanup(&pool, &topic, &id).await;
}

/// `skip` serializes against a live worker via the subscription row lock. A simulated
/// worker holds `FOR UPDATE` on the row, advances the cursor onto event 0 and resets
/// failures to 0 (a successful delivery), then commits. Because `skip` claims the same
/// row with plain `FOR UPDATE` it BLOCKS until that commit (never a silent no-op), then
/// re-evaluates the refuse-healthy guard on the now-unlocked row and REFUSES the healed
/// subscription instead of rewinding the checkpoint.
#[tokio::test]
async fn skip_waits_for_the_row_lock_then_refuses_a_worker_healed_sub() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.locked");
    let id = unique("sub.locked");
    let pos = append_positions(&pool, &topic, 3).await;
    wait_eligible(&pool, &topic, 1).await;
    // Paused with failures ⇒ eligible for skip; Genesis cursor (nothing consumed).
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;

    // Simulated worker: lock the row FOR UPDATE, advance onto event 0 + heal to
    // active/0-failures, hold the lock, then commit (release).
    let (g0, x0, t0) = pos[0].clone();
    let (wx0, wt0) = (x0.clone(), t0);
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let worker_pool = pool.clone();
    let worker_id = id.clone();
    let worker = tokio::spawn(async move {
        let mut tx = worker_pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM asyncevents.subscriptions WHERE subscription_id = $1 FOR UPDATE")
            .bind(&worker_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        locked_tx.send(()).unwrap();
        // Hold the lock long enough that skip is provably blocked on it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        sqlx::query(
            "UPDATE asyncevents.subscriptions \
             SET cursor_generation = $2, cursor_xid = $3::xid8, cursor_tie = $4, \
                 consecutive_failures = 0, next_attempt_at = NULL, state = 'active', updated_at = now() \
             WHERE subscription_id = $1",
        )
        .bind(&worker_id)
        .bind(g0)
        .bind(&wx0)
        .bind(wt0)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });

    // Wait until the worker holds the lock, then skip must BLOCK on FOR UPDATE.
    locked_rx.await.unwrap();
    let started = std::time::Instant::now();
    let err = skip(&pool, &id, "operator race", None).await.unwrap_err();
    let waited = started.elapsed();
    worker.await.unwrap();

    assert!(
        waited >= Duration::from_millis(300),
        "skip must have blocked on the worker's row lock, waited only {waited:?}"
    );
    assert!(err.to_string().contains("refusing to skip"), "unexpected: {err}");

    // The cursor is the worker's advance (event 0), NOT rewound by a racing skip.
    let after = snapshot(&pool, &id).await.unwrap();
    assert_eq!(
        after.cursor,
        format!("{g0}/{x0}/{t0}"),
        "cursor must reflect the worker's advance, never a skip rewind"
    );
    assert_eq!(after.consecutive_failures, 0);
    assert_eq!(after.state, "active");

    cleanup(&pool, &topic, &id).await;
}

/// `retry` clears the failure count and backoff window but LEAVES the cursor — the
/// current event is re-attempted, never skipped.
#[tokio::test]
async fn retry_clears_failures_and_keeps_cursor() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.retry");
    let id = unique("sub.retry");
    append_positions(&pool, &topic, 2).await;
    insert_sub(&pool, &id, &topic, "active", 7, (0, "0".to_string(), 0)).await;

    let (before, after) = retry(&pool, &id).await.unwrap();
    assert_eq!(before.consecutive_failures, 7);
    assert!(before.next_attempt_at.is_some());
    assert_eq!(after.consecutive_failures, 0, "retry clears failures");
    assert_eq!(after.next_attempt_at, None, "retry clears the backoff window");
    assert_eq!(after.cursor, before.cursor, "retry must NOT move the cursor");

    cleanup(&pool, &topic, &id).await;
}

/// `info` reports `paused_since` only for subscriptions currently in state
/// `paused`; an active subscription's `paused_since` is `None`.
#[tokio::test]
async fn info_reports_paused_since_only_for_paused_subs() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.pausedsince");
    let paused_id = unique("sub.pausedsince.paused");
    let active_id = unique("sub.pausedsince.active");
    append_positions(&pool, &topic, 1).await;
    insert_sub(&pool, &paused_id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;
    insert_sub(&pool, &active_id, &topic, "active", 0, (0, "0".to_string(), 0)).await;

    let subs = info(&pool).await.unwrap();
    let paused = subs.iter().find(|s| s.id == paused_id).expect("paused sub present");
    let active = subs.iter().find(|s| s.id == active_id).expect("active sub present");

    assert!(paused.paused_since.is_some(), "paused sub must report paused_since");
    assert_eq!(active.paused_since, None, "active sub must not report paused_since");

    cleanup(&pool, &topic, &paused_id).await;
    cleanup(&pool, &topic, &active_id).await;
}

/// A missing subscription id is a loud error, not a silent no-op.
#[tokio::test]
async fn unknown_subscription_errors() {
    let Some(pool) = test_pool().await else { return };
    let err = retry(&pool, "eventctl.does-not-exist").await.unwrap_err();
    assert!(err.to_string().contains("no such subscription"), "unexpected: {err}");
}

/// An unknown id is [`VerbError::NotFound`] from every verb that names one subscription.
#[tokio::test]
async fn unknown_subscription_is_not_found_from_every_verb() {
    let Some(pool) = test_pool().await else { return };
    let id = unique("sub.missing");
    let not_found = |what: &str, err: VerbError| {
        assert!(matches!(&err, VerbError::NotFound { id: e } if *e == id), "{what}: {err}");
    };
    not_found("snapshot", snapshot(&pool, &id).await.unwrap_err());
    not_found("retry", retry(&pool, &id).await.unwrap_err());
    not_found("pause", pause(&pool, &id).await.unwrap_err());
    not_found("resume", resume(&pool, &id).await.unwrap_err());
    not_found("resume_retry", resume_retry(&pool, &id).await.unwrap_err());
    not_found("retire", retire(&pool, &id).await.unwrap_err());
    not_found("skip", skip(&pool, &id, "r", None).await.unwrap_err());
    not_found("skip expected", skip(&pool, &id, "r", Some("e")).await.unwrap_err());
    not_found("poison_event", poison_event(&pool, &id).await.unwrap_err());
}

/// A retired subscription is [`VerbError::Retired`] from every verb that moves it,
/// and from `skip` whatever `expected` is.
#[tokio::test]
async fn retired_subscription_is_refused_by_every_moving_verb() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.retired");
    let id = unique("sub.retired");
    append_positions(&pool, &topic, 1).await;
    insert_sub(&pool, &id, &topic, "retired", 3, (0, "0".to_string(), 0)).await;

    let retired = |what: &str, err: VerbError| {
        assert!(matches!(&err, VerbError::Retired { id: e } if *e == id), "{what}: {err}");
    };
    retired("retry", retry(&pool, &id).await.unwrap_err());
    retired("pause", pause(&pool, &id).await.unwrap_err());
    retired("resume", resume(&pool, &id).await.unwrap_err());
    retired("resume_retry", resume_retry(&pool, &id).await.unwrap_err());
    retired("skip", skip(&pool, &id, "r", None).await.unwrap_err());
    retired("skip expected", skip(&pool, &id, "r", Some("e")).await.unwrap_err());
    assert_eq!(snapshot(&pool, &id).await.unwrap().state, "retired", "nothing moved");

    cleanup(&pool, &topic, &id).await;
}

/// `skip` with no expected event on a faulted subscription with no eligible event past
/// its cursor is [`VerbError::NothingToSkip`]; with an expected event it is a result.
#[tokio::test]
async fn skip_with_no_eligible_event_is_nothing_to_skip() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.empty");
    let id = unique("sub.empty");
    insert_sub(&pool, &id, &topic, "paused", 2, (0, "0".to_string(), 0)).await;
    let before = snapshot(&pool, &id).await.unwrap();

    let err = skip(&pool, &id, "r", None).await.unwrap_err();
    assert!(matches!(&err, VerbError::NothingToSkip { id: e } if *e == id), "unexpected: {err}");
    let resolved = skip(&pool, &id, "r", Some("e")).await.unwrap();
    assert!(matches!(resolved, SkipResult::AlreadyResolved { healthy: false }), "{resolved:?}");
    assert_eq!(snapshot(&pool, &id).await.unwrap(), before, "neither skip moved anything");

    cleanup(&pool, &topic, &id).await;
}

/// `resume_retry` reactivates a paused subscription and clears its failure count,
/// backoff window and last error; the cursor stays.
#[tokio::test]
async fn resume_retry_resets_failures_and_activates() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.resumeretry");
    let id = unique("sub.resumeretry");
    insert_sub(&pool, &id, &topic, "paused", 9, (0, "0".to_string(), 0)).await;

    let (before, after) = resume_retry(&pool, &id).await.unwrap();
    assert_eq!((before.state.as_str(), before.consecutive_failures), ("paused", 9));
    assert_eq!(after.state, "active", "resume_retry activates");
    assert_eq!(after.consecutive_failures, 0, "resume_retry resets the failure count");
    assert_eq!(after.next_attempt_at, None, "resume_retry clears the backoff window");
    assert_eq!(after.last_error, None, "resume_retry clears the last error");
    assert_eq!(after.cursor, before.cursor, "resume_retry must NOT move the cursor");

    cleanup(&pool, &topic, &id).await;
}

/// `resume` reactivates and clears the backoff window but leaves the failure count and
/// last error for `retry`.
#[tokio::test]
async fn resume_activates_but_leaves_failures() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.resume");
    let id = unique("sub.resume");
    insert_sub(&pool, &id, &topic, "paused", 9, (0, "0".to_string(), 0)).await;

    let (before, after) = resume(&pool, &id).await.unwrap();
    assert_eq!(before.state, "paused");
    assert_eq!(after.state, "active", "resume activates");
    assert_eq!(after.consecutive_failures, 9, "resume leaves the failure count");
    assert_eq!(after.last_error, before.last_error, "resume leaves the last error");
    assert_eq!(after.next_attempt_at, None, "resume clears the backoff window");
    assert_eq!(after.cursor, before.cursor, "resume must NOT move the cursor");

    cleanup(&pool, &topic, &id).await;
}

/// `poison_event` is the first eligible event past the cursor (not the event the cursor
/// is on, not a later one), moves nothing, and is `None` once the cursor is on the last
/// event.
#[tokio::test]
async fn poison_event_is_the_first_event_past_the_cursor() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.poison");
    let id = unique("sub.poison");
    let ties = [unique_tie(), unique_tie(), unique_tie()];
    for (xid, tie) in ["101", "102", "103"].into_iter().zip(ties) {
        insert_synthetic_event(&pool, &topic, xid, tie).await;
    }
    let ids = event_ids(&pool, &topic).await;
    // The cursor is ON event 101 (consumed), so 102 is the one the worker attempts next.
    insert_sub(&pool, &id, &topic, "paused", 4, (0, "101".to_string(), ties[0])).await;
    let before = snapshot(&pool, &id).await.unwrap();

    let ev = poison_event(&pool, &id).await.unwrap().expect("an event past the cursor");
    assert_eq!(ev.event_id, ids[1], "the first event past the cursor");
    assert_eq!((ev.topic.as_str(), ev.version), (topic.as_str(), 1));
    assert_eq!(ev.payload["xid"], 102, "payload: {}", ev.payload);
    assert_eq!(snapshot(&pool, &id).await.unwrap(), before, "poison_event moves nothing");

    deliver_onto(&pool, &id, "103", ties[2]).await;
    let none = poison_event(&pool, &id).await.unwrap();
    assert!(none.is_none(), "no event past the last one: {none:?}");

    cleanup(&pool, &topic, &id).await;
}

/// `skip` with the previewed event steps past that event only: the next preview is the
/// event after it.
#[tokio::test]
async fn skip_expected_skips_only_the_previewed_event() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.skipexpected");
    let id = unique("sub.skipexpected");
    let ties = [unique_tie(), unique_tie(), unique_tie()];
    for (xid, tie) in ["201", "202", "203"].into_iter().zip(ties) {
        insert_synthetic_event(&pool, &topic, xid, tie).await;
    }
    let ids = event_ids(&pool, &topic).await;
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;

    let preview = poison_event(&pool, &id).await.unwrap().expect("a poison event");
    assert_eq!(preview.event_id, ids[0]);
    let out = skipped(skip(&pool, &id, "poison", Some(&preview.event_id)).await.unwrap());
    assert_eq!(out.skipped_event_id, preview.event_id);
    assert_eq!(out.after.cursor, format!("0/201/{}", ties[0]), "the cursor lands on the previewed event only");
    assert_eq!((out.after.state.as_str(), out.after.consecutive_failures), ("active", 0));
    let next = poison_event(&pool, &id).await.unwrap().expect("the following event");
    assert_eq!(next.event_id, ids[1], "skip stepped past one event, no more");

    cleanup(&pool, &topic, &id).await;
}

/// A retry delivered the previewed event before the operator's skip: the subscription is
/// healthy, so `skip` with that event is `AlreadyResolved { healthy: true }` and moves
/// nothing (with `None` it would be `Healthy`, see
/// `skip_refuses_a_healthy_subscription`).
#[tokio::test]
async fn skip_expected_after_a_retry_delivered_it_is_already_resolved() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.healed");
    let id = unique("sub.healed");
    let ties = [unique_tie(), unique_tie()];
    for (xid, tie) in ["301", "302"].into_iter().zip(ties) {
        insert_synthetic_event(&pool, &topic, xid, tie).await;
    }
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;
    let preview = poison_event(&pool, &id).await.unwrap().expect("a poison event");

    deliver_onto(&pool, &id, "301", ties[0]).await;
    let before = snapshot(&pool, &id).await.unwrap();
    let resolved = skip(&pool, &id, "late", Some(&preview.event_id)).await.unwrap();
    assert!(matches!(resolved, SkipResult::AlreadyResolved { healthy: true }), "{resolved:?}");
    assert_eq!(snapshot(&pool, &id).await.unwrap(), before, "the delivered cursor stays; 302 is not skipped");

    cleanup(&pool, &topic, &id).await;
}

/// The previewed event is not the next one (another event blocks): `skip` is
/// `AlreadyResolved { healthy: false }` and moves nothing, never skipping the blocker.
#[tokio::test]
async fn skip_expected_behind_another_event_is_already_resolved() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("evctl.blocked");
    let id = unique("sub.blocked");
    for xid in ["401", "402"] {
        insert_synthetic_event(&pool, &topic, xid, unique_tie()).await;
    }
    let ids = event_ids(&pool, &topic).await;
    insert_sub(&pool, &id, &topic, "paused", 20, (0, "0".to_string(), 0)).await;
    let before = snapshot(&pool, &id).await.unwrap();

    let resolved = skip(&pool, &id, "wrong event", Some(&ids[1])).await.unwrap();
    assert!(matches!(resolved, SkipResult::AlreadyResolved { healthy: false }), "{resolved:?}");
    assert_eq!(snapshot(&pool, &id).await.unwrap(), before, "neither event is skipped");

    cleanup(&pool, &topic, &id).await;
}

/// `eventctl` prints a verb error's `Display`; these are its messages, and `Db`
/// prints the whole context chain of its source.
#[test]
fn verb_error_messages_are_the_operator_messages() {
    let id = "s.v1".to_string();
    assert_eq!(VerbError::NotFound { id: id.clone() }.to_string(), "eventctl: no such subscription \"s.v1\"");
    assert_eq!(VerbError::Retired { id: id.clone() }.to_string(), "subscription \"s.v1\" is retired");
    assert_eq!(
        VerbError::Healthy { id: id.clone() }.to_string(),
        "eventctl: refusing to skip \"s.v1\" — it is active with no failures; skip only steps past the \
         CURRENT failing event of a paused/faulted subscription (pause it first, or use retry)"
    );
    assert_eq!(
        VerbError::NothingToSkip { id }.to_string(),
        "eventctl: no eligible event past \"s.v1\"'s cursor to skip — nothing to do"
    );
    let db = VerbError::from(anyhow!("connection refused").context("eventctl: read subscription"));
    assert_eq!(db.to_string(), "eventctl: read subscription: connection refused");
    let wrapped = anyhow::Error::new(db);
    assert_eq!(format!("{wrapped:#}"), "eventctl: read subscription: connection refused", "no repeated chain");
}

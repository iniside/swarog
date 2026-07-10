//! Live-Postgres tests for the operator verbs (house `test_pool()` pattern). The
//! `skip`/`retry` logic lives in the lib exactly so it is testable here without the
//! CLI shell. Events are appended via the plane's own writer; the plane is migrated
//! per test (idempotent).

use super::*;
use std::time::Duration;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — eventctl tests skipped");
            return None;
        }
    };
    if let Err(err) = asyncevents::Plane::new(pool.clone(), dsn).unwrap().migrate().await {
        eprintln!("SKIP: asyncevents migrate failed: {err}");
        return None;
    }
    Some(pool)
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
        asyncevents::store::append(&mut tx, &contract, payload.as_bytes())
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

    let out = skip(&pool, &id, "unrecoverable payload").await.unwrap();

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
    let err = skip(&pool, &id, "again").await.unwrap_err();
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

    let out = skip(&pool, &id, "unrecoverable").await.unwrap();

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

    let err = skip(&pool, &id, "no").await.unwrap_err();
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
    let err = skip(&pool, &id, "operator race").await.unwrap_err();
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

/// A missing subscription id is a loud error, not a silent no-op.
#[tokio::test]
async fn unknown_subscription_errors() {
    let Some(pool) = test_pool().await else { return };
    let err = retry(&pool, "eventctl.does-not-exist").await.unwrap_err();
    assert!(err.to_string().contains("no such subscription"), "unexpected: {err}");
}

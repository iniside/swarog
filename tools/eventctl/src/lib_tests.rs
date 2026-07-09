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
        "SELECT generation, producer_xid::text, tie_breaker FROM asyncevents.events \
         WHERE topic = $1 ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(topic)
    .fetch_all(pool)
    .await
    .unwrap()
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

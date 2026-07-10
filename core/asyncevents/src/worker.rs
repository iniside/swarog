//! The pull-delivery worker: selects one due subscription (`FOR UPDATE SKIP
//! LOCKED` — replicas of one service share the row and form a consumer group by
//! construction), computes the frontier, selects one next event, runs the handler
//! on the SAME connection under a savepoint, advances the cursor, and commits —
//! effect + checkpoint atomically (`TransactionalPg`).
//!
//! Failure state machine: a handler ERROR rolls back to the savepoint, records
//! `consecutive_failures`/`last_error`/`next_attempt_at` (exponential backoff
//! 1s → 5m) and commits immediately; after [`PAUSE_AFTER`] consecutive failures
//! the subscription pauses. There is NO automatic skip, ever — the cursor only
//! advances on a committed effect. A handler TIMEOUT poisons its delivery
//! connection (an in-flight statement makes it unusable): the connection is
//! detached and dropped, the wedged backend is terminated (releasing the row
//! lock), and the backoff is recorded on a FRESH pool connection — never a
//! savepoint rollback on a timed-out connection. Workers never sleep holding the
//! row lock: every wait happens after COMMIT/ROLLBACK.

use std::sync::Arc;
use std::time::Duration;

use bus::{AnyTx, Delivery};
use sqlx::{PgPool, Row};
use tokio::sync::{watch, Notify};

use crate::transport::SubEntry;

/// Handler wall-clock budget per delivery (`ASYNCEVENTS_HANDLER_TIMEOUT`,
/// Go-style duration or plain seconds; default 10s).
const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(10);

/// Consecutive failures after which a subscription pauses (operator resume).
const PAUSE_AFTER: i32 = 20;

const BACKOFF_MIN_SECS: f64 = 1.0;
const BACKOFF_MAX_SECS: f64 = 300.0;

/// Everything one worker task needs. Shared (`Arc`) across the process's worker
/// pool; the subscription list is the [`crate::Plane::start`] snapshot — workers
/// only ever touch locally-registered subscription ids.
pub(crate) struct WorkerCtx {
    pub pool: PgPool,
    pub subs: Vec<SubEntry>,
    pub handler_timeout: Duration,
    /// Wake-up: NOTIFY (`asyncevents_events` via [`crate::wakeup`]) or the global
    /// 1s poll fallback in [`run`] — NOTIFY is best-effort, the poll is the floor.
    pub wakeup: Arc<Notify>,
}

/// `ASYNCEVENTS_HANDLER_TIMEOUT`: `10s`/`500ms`/`5m` or a bare seconds integer.
pub(crate) fn handler_timeout_from_env() -> Duration {
    match std::env::var("ASYNCEVENTS_HANDLER_TIMEOUT") {
        Ok(v) => parse_duration(&v).unwrap_or(DEFAULT_HANDLER_TIMEOUT),
        Err(_) => DEFAULT_HANDLER_TIMEOUT,
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        return n.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(n) = s.strip_suffix('m') {
        return n.trim().parse::<u64>().ok().map(|m| Duration::from_secs(m * 60));
    }
    if let Some(n) = s.strip_suffix('s') {
        return n.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    s.parse::<u64>().ok().map(Duration::from_secs)
}

/// Exponential backoff: 1s doubling per consecutive failure, capped at 5m.
pub(crate) fn backoff_secs(failures: i32) -> f64 {
    let exp = (failures - 1).clamp(0, 30) as u32;
    (BACKOFF_MIN_SECS * f64::from(2u32.saturating_pow(exp))).min(BACKOFF_MAX_SECS)
}

/// The outcome of one delivery attempt on one subscription.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// One event delivered; cursor advanced and committed. Keep draining.
    Delivered,
    /// No eligible event past the cursor (frontier-bounded). Yield this sub.
    Empty,
    /// Not due (backoff/paused/retired) or another worker holds the row lock.
    Skipped,
    /// Handler error or timeout; backoff recorded. Yield this sub.
    Faulted,
}

/// One delivery attempt: lock the subscription row, pick the next eligible
/// event, run the handler under a savepoint on the same connection, advance the
/// cursor, commit. See the module docs for the error/timeout arms.
pub(crate) async fn deliver_one(ctx: &WorkerCtx, entry: &SubEntry) -> anyhow::Result<Step> {
    let mut conn = ctx.pool.acquire().await?;
    sqlx::query("BEGIN").execute(&mut *conn).await?;

    // Due-subscription select, restricted to THIS locally-registered id.
    // `pg_backend_pid()` rides along so the timeout arm can terminate this exact
    // backend (a dropped socket alone may go unnoticed mid-statement).
    let sub = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        "SELECT cursor_generation, cursor_xid::text AS cursor_xid_text, cursor_tie, \
                consecutive_failures, pg_backend_pid() AS pid \
         FROM asyncevents.subscriptions \
         WHERE subscription_id = $1 AND state = 'active' \
           AND (next_attempt_at IS NULL OR next_attempt_at <= now()) \
         FOR UPDATE SKIP LOCKED",
    )
    .bind(entry.spec.id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(sub) = sub else {
        sqlx::query("ROLLBACK").execute(&mut *conn).await?;
        return Ok(Step::Skipped);
    };
    let cursor_gen: i64 = sub.get("cursor_generation");
    let cursor_xid: String = sub.get("cursor_xid_text");
    let cursor_tie: i64 = sub.get("cursor_tie");
    let failures: i32 = sub.get("consecutive_failures");
    let backend_pid: i32 = sub.get("pid");

    // Frontier-bounded next-event select: row-compare past the cursor, exact
    // contract-version match, current-generation rows gated by the snapshot xmin
    // (an in-flight earlier xid can never be left behind as a gap); completed
    // older generations are fully eligible.
    let version = i32::try_from(entry.version).unwrap_or(i32::MAX);
    let ev = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        "SELECT event_id, generation, producer_xid::text AS producer_xid_text, tie_breaker, \
                payload::text AS payload_text \
         FROM asyncevents.events \
         WHERE topic = $1 AND contract_version = $2 \
           AND (generation, producer_xid, tie_breaker) > ($3, $4::xid8, $5) \
           AND (generation < (SELECT generation FROM asyncevents.plane_meta WHERE singleton) \
                OR producer_xid < pg_snapshot_xmin(pg_current_snapshot())) \
         ORDER BY generation, producer_xid, tie_breaker \
         LIMIT 1",
    )
    .bind(&entry.topic)
    .bind(version)
    .bind(cursor_gen)
    .bind(&cursor_xid)
    .bind(cursor_tie)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(ev) = ev else {
        sqlx::query("COMMIT").execute(&mut *conn).await?;
        return Ok(Step::Empty);
    };
    let event_id: String = ev.get("event_id");
    let ev_gen: i64 = ev.get("generation");
    let ev_xid: String = ev.get("producer_xid_text");
    let ev_tie: i64 = ev.get("tie_breaker");
    let payload: String = ev.get("payload_text");

    sqlx::query("SAVEPOINT deliver").execute(&mut *conn).await?;

    let handler = entry.handler.clone();
    let call = handler.call(
        Delivery {
            event_id: &event_id,
            tx: AnyTx::new(&mut *conn),
        },
        payload.into_bytes(),
    );
    match tokio::time::timeout(ctx.handler_timeout, call).await {
        Ok(Ok(())) => {
            sqlx::query(
                "UPDATE asyncevents.subscriptions \
                 SET cursor_generation = $2, cursor_xid = $3::xid8, cursor_tie = $4, \
                     consecutive_failures = 0, next_attempt_at = NULL, last_error = NULL, \
                     updated_at = now() \
                 WHERE subscription_id = $1",
            )
            .bind(entry.spec.id)
            .bind(ev_gen)
            .bind(&ev_xid)
            .bind(ev_tie)
            .execute(&mut *conn)
            .await?;
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            Ok(Step::Delivered)
        }
        Ok(Err(err)) => {
            // Handler error: undo its partial effect, keep the row lock, record
            // the backoff state, commit IMMEDIATELY (never sleep on the lock).
            sqlx::query("ROLLBACK TO SAVEPOINT deliver").execute(&mut *conn).await?;
            let msg = err.to_string();
            tracing::error!(
                subscription = entry.spec.id, %event_id, err = %msg,
                "asyncevents: durable handler failed; backing off (no skip)"
            );
            record_failure(&mut conn, entry.spec.id, failures + 1, &msg).await?;
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            Ok(Step::Faulted)
        }
        Err(_elapsed) => {
            // Timeout: the connection has an in-flight statement and is unusable.
            // Detach it from the pool and drop it (never a savepoint rollback),
            // terminate the wedged backend so the row lock releases NOW, then
            // record the backoff on a FRESH pool connection.
            let raw = conn.detach();
            drop(raw);
            let msg = format!(
                "handler timeout after {:?} (delivery connection poisoned)",
                ctx.handler_timeout
            );
            tracing::error!(
                subscription = entry.spec.id, %event_id, backend_pid,
                "asyncevents: durable handler timed out; poisoning the delivery connection"
            );
            let _ = sqlx::query("SELECT pg_terminate_backend($1)")
                .bind(backend_pid)
                .execute(&ctx.pool)
                .await;
            let mut fresh = ctx.pool.acquire().await?;
            record_failure(&mut fresh, entry.spec.id, failures + 1, &msg).await?;
            Ok(Step::Faulted)
        }
    }
}

/// Writes the failure state machine's transition: bump `consecutive_failures`,
/// set the exponential `next_attempt_at`, record `last_error`, pause at the
/// threshold. Runs on whatever connection the caller hands (the delivery tx on
/// the error arm; a fresh autocommit connection on the timeout arm).
async fn record_failure(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Postgres>,
    sub_id: &str,
    failures: i32,
    error: &str,
) -> anyhow::Result<()> {
    let pause = failures >= PAUSE_AFTER;
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET consecutive_failures = $2, \
             next_attempt_at = now() + make_interval(secs => $3), \
             last_error = $4, \
             state = CASE WHEN $5 THEN 'paused' ELSE state END, \
             updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .bind(failures)
    .bind(backoff_secs(failures))
    .bind(error)
    .bind(pause)
    .execute(&mut **conn)
    .await?;
    if pause {
        tracing::error!(
            subscription = sub_id, failures,
            "asyncevents: subscription PAUSED after consecutive failures — resume via eventctl"
        );
    }
    Ok(())
}

/// One pass over every local subscription, draining each until it has no
/// eligible events (or faults/loses the lock) before yielding. Returns the
/// number of events delivered.
pub(crate) async fn drain_pass(ctx: &WorkerCtx, stop: Option<&watch::Receiver<bool>>) -> u64 {
    let mut delivered = 0u64;
    for entry in &ctx.subs {
        loop {
            if let Some(stop) = stop {
                if *stop.borrow() {
                    return delivered;
                }
            }
            match deliver_one(ctx, entry).await {
                Ok(Step::Delivered) => delivered += 1,
                Ok(Step::Empty | Step::Skipped | Step::Faulted) => break,
                Err(err) => {
                    tracing::error!(subscription = entry.spec.id, %err, "asyncevents: delivery attempt errored");
                    break;
                }
            }
        }
    }
    delivered
}

/// The worker loop: drain every local subscription; when a full pass delivers
/// nothing, wait for a NOTIFY wake-up or the global 1s poll fallback (lost
/// NOTIFYs only delay a row by one tick). All waits happen with no row lock held.
pub(crate) async fn run(ctx: Arc<WorkerCtx>, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        let delivered = drain_pass(&ctx, Some(&stop)).await;
        if delivered == 0 {
            tokio::select! {
                _ = stop.changed() => return,
                _ = ctx.wakeup.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;

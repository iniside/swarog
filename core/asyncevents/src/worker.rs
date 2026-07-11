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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bus::{AnyTx, Delivery};
use anyhow::{bail, Context};
use sqlx::{Connection, PgConnection, Row};
use tokio::sync::{watch, Notify};

use crate::transport::SubEntry;

/// Handler wall-clock budget per delivery (`ASYNCEVENTS_HANDLER_TIMEOUT`,
/// Go-style duration or plain seconds; default 10s).
const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DELIVERIES_PER_SUB_PASS: u64 = 64;

#[derive(Clone, Debug)]
pub(crate) struct ActiveDelivery {
    pub generation: u64,
    pub pid: i32,
    pub backend_start: String,
}

#[derive(Clone, Default)]
pub(crate) struct ActiveDeliveries {
    inner: Arc<Mutex<HashMap<usize, ActiveDelivery>>>,
}

impl ActiveDeliveries {
    pub(crate) fn register(&self, worker_id: usize, generation: u64, pid: i32, backend_start: String) -> ActiveGuard {
        self.inner.lock().unwrap().insert(worker_id, ActiveDelivery {
            generation, pid, backend_start,
        });
        ActiveGuard { active: self.clone(), worker_id, generation }
    }

    /// Snapshot of every in-flight delivery. Called exactly once per
    /// [`crate::Plane::stop`]; `terminate_claim` re-checks the claim against the
    /// live map (generation + pid + backend_start) before acting, and
    /// [`ActiveGuard`]'s generation-checked drop is the stale-removal guard —
    /// no claim-side state machine is needed.
    pub fn claim_active(&self) -> Vec<(usize, ActiveDelivery)> {
        self.inner.lock().unwrap()
            .iter()
            .map(|(&worker_id, delivery)| (worker_id, delivery.clone()))
            .collect()
    }

    pub fn contains(&self, worker_id: usize, generation: u64, pid: i32, backend_start: &str) -> bool {
        self.inner.lock().unwrap().get(&worker_id)
            .is_some_and(|d| d.generation == generation && d.pid == pid && d.backend_start == backend_start)
    }
}

pub(crate) struct ActiveGuard {
    active: ActiveDeliveries,
    worker_id: usize,
    generation: u64,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        let mut inner = self.active.inner.lock().unwrap();
        if inner.get(&self.worker_id).is_some_and(|d| d.generation == self.generation) {
            inner.remove(&self.worker_id);
        }
    }
}

/// Consecutive failures after which a subscription pauses (operator resume).
const PAUSE_AFTER: i32 = 20;

const BACKOFF_MIN_SECS: f64 = 1.0;
const BACKOFF_MAX_SECS: f64 = 300.0;

/// Everything one worker task needs. Shared (`Arc`) across the process's worker
/// pool; the subscription list is the [`crate::Plane::start`] snapshot — workers
/// only ever touch locally-registered subscription ids.
pub(crate) struct WorkerCtx {
    pub dsn: String,
    pub subs: Vec<SubEntry>,
    pub handler_timeout: Duration,
    /// Wake-up: NOTIFY (`asyncevents_events` via [`crate::wakeup`]) or the global
    /// 1s poll fallback in [`run`] — NOTIFY is best-effort, the poll is the floor.
    pub wakeup: Arc<Notify>,
    pub active: ActiveDeliveries,
    /// The plane's health probe; [`run`] stamps it after every healthy pass so
    /// `/readyz` can flag a worker stuck in a reconnect/error loop.
    pub liveness: crate::Liveness,
}

/// `ASYNCEVENTS_HANDLER_TIMEOUT`: `10s`/`500ms`/`5m` or a bare seconds integer.
pub(crate) fn handler_timeout_from_env() -> anyhow::Result<Duration> {
    match std::env::var("ASYNCEVENTS_HANDLER_TIMEOUT") {
        Ok(v) => parse_duration(&v).context("invalid ASYNCEVENTS_HANDLER_TIMEOUT"),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_HANDLER_TIMEOUT),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("ASYNCEVENTS_HANDLER_TIMEOUT is not valid Unicode")
        }
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let duration =
    if let Some(n) = s.strip_suffix("ms") {
        Duration::from_millis(n.trim().parse::<u64>().ok()?)
    } else if let Some(n) = s.strip_suffix('m') {
        Duration::from_secs(n.trim().parse::<u64>().ok()?.checked_mul(60)?)
    } else if let Some(n) = s.strip_suffix('s') {
        Duration::from_secs(n.trim().parse::<u64>().ok()?)
    } else {
        Duration::from_secs(s.parse::<u64>().ok()?)
    };
    (!duration.is_zero()).then_some(duration)
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
    /// Handler error; backoff recorded on the same (still healthy) connection
    /// via savepoint rollback + commit. Yield this sub.
    Faulted,
    /// Handler timeout; backoff recorded, but the delivery connection's backend
    /// was terminated — the connection is unusable and the caller must discard
    /// it (immediate reconnect, no follow-up op, no spurious error).
    Poisoned,
}

/// Opens one delivery-session connection: a direct `PgConnection` with
/// `idle_in_transaction_session_timeout` derived from the handler budget (2x,
/// so a legit slow handler near the configurable `ASYNCEVENTS_HANDLER_TIMEOUT`
/// is never killed by it). This is a belt against THIS worker leaking its OWN
/// open transaction (a dropped future between statements would leave the
/// session idle-in-transaction, holding the row lock and pinning xmin — the
/// timeout arm's `pg_terminate_backend` only covers a backend wedged INSIDE a
/// statement). It deliberately does NOT cover a rogue idle-in-tx session
/// elsewhere in the cluster pinning the frontier: that is an ops concern —
/// a global `idle_in_transaction_session_timeout` (postgresql.conf /
/// ALTER SYSTEM) plus alerting on the existing
/// `asyncevents_safe_frontier_age_seconds` gauge.
pub(crate) async fn connect(dsn: &str, handler_timeout: Duration) -> sqlx::Result<PgConnection> {
    let mut conn = PgConnection::connect(dsn).await?;
    // SET takes no bind parameters; the value is a locally computed integer
    // (milliseconds), never user input.
    let timeout_ms = handler_timeout.saturating_mul(2).as_millis().min(u128::from(u32::MAX));
    sqlx::query(&format!("SET idle_in_transaction_session_timeout = {timeout_ms}"))
        .execute(&mut conn)
        .await?;
    Ok(conn)
}

/// One delivery attempt: lock the subscription row, pick the next eligible
/// event, run the handler under a savepoint on the same connection, advance the
/// cursor, commit. See the module docs for the error/timeout arms.
pub(crate) async fn deliver_one(ctx: &WorkerCtx, entry: &SubEntry) -> anyhow::Result<Step> {
    let mut conn = connect(&ctx.dsn, ctx.handler_timeout).await?;
    deliver_one_on(ctx, entry, &mut conn).await
}

async fn deliver_one_on(ctx: &WorkerCtx, entry: &SubEntry, conn: &mut PgConnection) -> anyhow::Result<Step> {
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
            record_failure(
                &mut *conn,
                entry.spec.id,
                failures + 1,
                &msg,
                cursor_gen,
                &cursor_xid,
                cursor_tie,
                failures,
            )
            .await?;
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            Ok(Step::Faulted)
        }
        Err(_elapsed) => {
            // Timeout: the private worker session is unusable. Terminate that
            // exact backend through another direct connection so its row lock
            // releases now, then record backoff through a fresh direct session.
            let msg = format!(
                "handler timeout after {:?} (delivery connection poisoned)",
                ctx.handler_timeout
            );
            tracing::error!(
                subscription = entry.spec.id, %event_id, backend_pid,
                "asyncevents: durable handler timed out; poisoning the delivery connection"
            );
            if let Ok(mut control) = PgConnection::connect(&ctx.dsn).await {
                let _ = sqlx::query("SELECT pg_terminate_backend($1)")
                    .bind(backend_pid)
                    .execute(&mut control)
                    .await;
            }
            let mut fresh = PgConnection::connect(&ctx.dsn).await?;
            record_failure(
                &mut fresh,
                entry.spec.id,
                failures + 1,
                &msg,
                cursor_gen,
                &cursor_xid,
                cursor_tie,
                failures,
            )
            .await?;
            Ok(Step::Poisoned)
        }
    }
}

/// Writes the failure state machine's transition: bump `consecutive_failures`,
/// set the exponential `next_attempt_at`, record `last_error`, pause at the
/// threshold. Runs on whatever connection the caller hands (the delivery tx on
/// the error arm; a fresh autocommit connection on the timeout arm).
///
/// CAS-guarded on the claim-time state: the UPDATE only lands if the row still
/// carries the `(cursor_generation, cursor_xid, cursor_tie)` and
/// `consecutive_failures` the caller read when it claimed the subscription. On
/// the error arm the row lock is still held, so the guard is trivially true. On
/// the timeout arm the lock was released (terminated backend) BEFORE this runs,
/// so a healthy replica that delivered (advancing the cursor) or an operator
/// `retry` (resetting failures) in the terminate-to-record window makes this
/// UPDATE match zero rows — the stale backoff/pause is dropped. The
/// `consecutive_failures` leg is not redundant with the cursor: the cursor does
/// NOT move on failure/retry/resume, so a cursor-only CAS would have an ABA where
/// a stale `failures + 1` could pause a subscription an operator just reset.
#[allow(clippy::too_many_arguments)]
async fn record_failure(
    conn: &mut PgConnection,
    sub_id: &str,
    failures: i32,
    error: &str,
    cursor_generation: i64,
    cursor_xid_text: &str,
    cursor_tie: i64,
    claimed_failures: i32,
) -> anyhow::Result<()> {
    let pause = failures >= PAUSE_AFTER;
    let result = sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET consecutive_failures = $2, \
             next_attempt_at = now() + make_interval(secs => $3), \
             last_error = $4, \
             state = CASE WHEN $5 THEN 'paused' ELSE state END, \
             updated_at = now() \
         WHERE subscription_id = $1 \
           AND cursor_generation = $6 AND cursor_xid = $7::xid8 AND cursor_tie = $8 \
           AND consecutive_failures = $9",
    )
    .bind(sub_id)
    .bind(failures)
    .bind(backoff_secs(failures))
    .bind(error)
    .bind(pause)
    .bind(cursor_generation)
    .bind(cursor_xid_text)
    .bind(cursor_tie)
    .bind(claimed_failures)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() == 0 {
        tracing::info!(
            subscription = sub_id,
            "asyncevents: subscription state changed concurrently; stale failure not recorded"
        );
        return Ok(());
    }
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
        for _ in 0..DELIVERIES_PER_SUB_PASS {
            if let Some(stop) = stop {
                if *stop.borrow() {
                    return delivered;
                }
            }
            match deliver_one(ctx, entry).await {
                Ok(Step::Delivered) => delivered += 1,
                // `Poisoned` killed a per-call connection that is dropped right
                // here anyway — nothing to reconnect, just yield the sub.
                Ok(Step::Empty | Step::Skipped | Step::Faulted | Step::Poisoned) => break,
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
pub(crate) async fn run(worker_id: usize, ctx: Arc<WorkerCtx>, mut stop: watch::Receiver<bool>) {
    // The persistent connection travels with its session identity: pid and
    // backend_start are constant per connection, so they are queried ONCE at
    // (re)connect instead of every pass.
    let mut conn: Option<(PgConnection, i32, String)> = None;
    let mut generation = 0u64;
    loop {
        if *stop.borrow() {
            return;
        }
        if conn.is_none() {
            let established: sqlx::Result<(PgConnection, i32, String)> = async {
                let mut c = connect(&ctx.dsn, ctx.handler_timeout).await?;
                let (pid, backend_start): (i32, String) = sqlx::query_as(
                    "SELECT pg_backend_pid(), backend_start::text FROM pg_stat_activity WHERE pid = pg_backend_pid()"
                )
                .fetch_one(&mut c)
                .await?;
                Ok((c, pid, backend_start))
            }
            .await;
            match established {
                Ok(v) => conn = Some(v),
                Err(err) => {
                    tracing::error!(%err, "asyncevents: worker connection failed");
                    tokio::select! {
                        _ = stop.changed() => return,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                    continue;
                }
            }
        }
        let (connection, pid, backend_start) = conn.as_mut().expect("connected");
        generation = generation.wrapping_add(1);
        let _active = ctx.active.register(worker_id, generation, *pid, backend_start.clone());
        let (delivered, healthy) = drain_pass_on(&ctx, Some(&stop), connection).await;
        if healthy {
            ctx.liveness.mark_pass_ok();
        } else {
            conn = None;
        }
        if delivered == 0 {
            tokio::select! {
                _ = stop.changed() => return,
                _ = ctx.wakeup.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

async fn drain_pass_on(
    ctx: &WorkerCtx,
    stop: Option<&watch::Receiver<bool>>,
    conn: &mut PgConnection,
) -> (u64, bool) {
    let mut delivered = 0;
    for entry in &ctx.subs {
        for _ in 0..DELIVERIES_PER_SUB_PASS {
            if stop.is_some_and(|s| *s.borrow()) { return (delivered, true); }
            match deliver_one_on(ctx, entry, conn).await {
                Ok(Step::Delivered) => delivered += 1,
                Ok(Step::Empty | Step::Skipped | Step::Faulted) => break,
                // The timeout arm terminated this connection's own backend: the
                // session is unusable. Report unhealthy so the caller reconnects
                // NOW instead of failing (and error-logging) the next op.
                Ok(Step::Poisoned) => return (delivered, false),
                Err(err) => {
                    tracing::error!(subscription = entry.spec.id, %err, "asyncevents: delivery attempt errored; reconnecting");
                    return (delivered, false);
                }
            }
        }
    }
    (delivered, true)
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;

//! `asyncevents` — the durable async-events plane. It owns schema `asyncevents`
//! (an XID-ordered shared event log + consumer-owned pull subscriptions with
//! transactional checkpoints) and implements [`bus::Transport`]. It is NOT a
//! `lifecycle::Module`: the plane is process infrastructure, like the HTTP
//! listener — `core/app::run` constructs a [`Plane`] iff the process has a DB,
//! injects its transport at `Context` construction (`Bus::with_transport`),
//! migrates its schema before module migrations, starts its workers after module
//! starts, and halts delivery before any module stops. Modules declare nothing:
//! DB ⇒ plane.
//!
//! A producer reaches it purely via `bus.emit_tx` (one `asyncevents.append_event`
//! call inside the producer's own domain tx); a consumer via `bus.on_tx`/
//! `on_tx_raw` (a durable handler the pull worker runs inside the delivery tx,
//! atomically with the cursor advance). Neither ever sees the log, the
//! subscriptions table, or the workers. Delivery is topology-transparent: every
//! process reads the ONE shared log, restricted to its own subscription ids —
//! there is no producer-side subscriber routing, no origin, no HTTP sink.
//!
//! Crate layout: [`store`] is the log + writer protocol (positions, generations,
//! `asyncevents.append_event`, startup guards); [`transport`] the
//! [`bus::Transport`] over it; [`catalog`] materializes `SubscriptionSpec`s into
//! rows (cursor discipline); [`worker`] the pull loop + failure state machine;
//! [`wakeup`] the NOTIFY listener; [`plane_metrics`] the lag/frontier gauges;
//! [`retention`] the checkpoint-coupled GC task.

mod catalog;
mod plane_metrics;
mod retention;
pub mod store;
mod transport;
mod wakeup;
mod worker;

pub use transport::LogTransport;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use bus::Transport;
use futures::FutureExt;
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use std::time::Duration;

/// Pull workers per plane. Each drains due subscriptions independently;
/// `FOR UPDATE SKIP LOCKED` arbitrates, so the count is throughput tuning, not
/// correctness.
const WORKERS: usize = 2;
const DEFAULT_PLANE_STOP_GRACE_MS: u64 = 5_000;

async fn terminate_claim(
    dsn: &str,
    active: &worker::ActiveDeliveries,
    worker_id: usize,
    delivery: &worker::ActiveDelivery,
) -> anyhow::Result<bool> {
    // The worker may have completed after stop claimed it. Never act on a stale
    // generation, and bind PostgreSQL's stable session identity as well as PID:
    // an OS PID can be reused by a newly connected, unrelated backend.
    if !active.contains(
        worker_id,
        delivery.generation,
        delivery.pid,
        &delivery.backend_start,
    ) {
        return Ok(false);
    }
    let mut control = PgConnection::connect(dsn).await?;
    let terminated: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT pg_terminate_backend(pid) \
         FROM pg_stat_activity WHERE pid = $1 AND backend_start::text = $2), false)",
    )
    .bind(delivery.pid)
    .bind(&delivery.backend_start)
    .fetch_one(&mut control)
    .await?;
    Ok(terminated)
}

/// Drops the LEGACY push-plane storage (outbox/inbox/notify trigger and the
/// pre-rename `messaging` schema). The fresh-start decision makes this a plain
/// wipe: no data migrates — V2 positions are a different coordinate system.
const LEGACY_DROP_DDL: &str = r#"
DROP TABLE IF EXISTS asyncevents.outbox CASCADE;
DROP TABLE IF EXISTS asyncevents.inbox CASCADE;
DROP FUNCTION IF EXISTS asyncevents.notify_outbox() CASCADE;
DROP SCHEMA IF EXISTS messaging CASCADE;"#;

/// Coarse monotonic seconds since the first call in this process — the base for
/// the delivery-staleness probe. Deliberately not wall-clock (a clock jump must
/// not flap `/readyz`, and tests never depend on the system time).
fn coarse_now_secs() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_secs()
}

/// A cloneable worker-health probe: [`Liveness::dead`] is flipped once if any
/// worker task dies while the plane is running (panic or premature exit);
/// [`Liveness::delivery_stalled`] flags a worker pool that is alive but has not
/// completed a healthy pass recently (e.g. looping on connection errors — that
/// loop never exits, so `dead` alone would keep `/readyz` green forever).
/// `app::run` folds both into `/readyz` as a named `httpmw::ReadyCheck` — a
/// process whose delivery loop is gone must stop taking traffic that expects
/// its effects. [`Liveness::retention_dead`] is a SEPARATE bit for the retention
/// GC task (its own named `asyncevents-retention` check): a GC outage is storage
/// growth, not a serving outage, so it must never ride the delivery `dead` flag.
/// [`Liveness::retention_stalled`] is the retention twin of `delivery_stalled` —
/// a GC task alive but whose sweeps persistently fail — folded into that same
/// named check.
#[derive(Clone, Default)]
pub struct Liveness {
    dead: Arc<AtomicBool>,
    /// Flipped once if the retention GC task dies (panic or premature exit)
    /// while the plane is running. Deliberately NOT folded into [`Self::dead`].
    retention_dead: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    /// Coarse-clock second of the last healthy worker pass; `0` = no workers
    /// hosted (never seeded), which never counts as stalled.
    last_ok_secs: Arc<AtomicU64>,
    /// Coarse-clock second of the last successful retention sweep; `0` = never
    /// seeded, which never counts as stalled. The retention twin of
    /// [`Self::last_ok_secs`]: a GC task that is alive but whose sweeps keep
    /// failing (a revoked function, a broken query) never flips
    /// [`Self::retention_dead`], so `dead` alone would keep the
    /// `asyncevents-retention` check green while the log grows unbounded.
    retention_ok_secs: Arc<AtomicU64>,
}

impl Liveness {
    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// True iff the retention GC task died while the plane was running.
    pub fn retention_dead(&self) -> bool {
        self.retention_dead.load(Ordering::SeqCst)
    }

    /// True iff this process hosts delivery workers and none has completed a
    /// healthy pass within `max_age`. Never true while the plane is stopping
    /// (a controlled stop is not a stall) or when the plane hosts no workers
    /// (the clock was never seeded).
    pub fn delivery_stalled(&self, max_age: Duration) -> bool {
        if self.stopping.load(Ordering::SeqCst) {
            return false;
        }
        let last = self.last_ok_secs.load(Ordering::SeqCst);
        if last == 0 {
            return false;
        }
        coarse_now_secs().saturating_sub(last) > max_age.as_secs()
    }

    /// Stamps "healthy delivery progress now". `Plane::start` seeds it (starting
    /// from 0 would read as an infinite age and flap `/readyz` right after
    /// startup, since HTTP serves before the first pass on a cold DB); the
    /// worker stamps it after EVERY delivered event (a full pass can legitimately
    /// take up to 64×subs×handler_timeout, far past `DELIVERY_STALL_MAX` — the
    /// clock must reflect progress, not pass boundaries) and after every healthy
    /// `drain_pass_on`. `max(1)` because `0` is the "no workers" sentinel.
    pub(crate) fn mark_pass_ok(&self) {
        self.last_ok_secs.store(coarse_now_secs().max(1), Ordering::SeqCst);
    }

    /// True iff this process's retention sweep has not succeeded within `max_age`.
    /// Exact mirror of [`Self::delivery_stalled`]: never true while the plane is
    /// stopping (a controlled stop is not a stall) or before the clock was seeded
    /// (`0` sentinel — `Plane::start` seeds it, so this only reads `0` outside a
    /// running plane).
    pub fn retention_stalled(&self, max_age: Duration) -> bool {
        if self.stopping.load(Ordering::SeqCst) {
            return false;
        }
        let last = self.retention_ok_secs.load(Ordering::SeqCst);
        if last == 0 {
            return false;
        }
        coarse_now_secs().saturating_sub(last) > max_age.as_secs()
    }

    /// Stamps "healthy retention sweep now". `Plane::start` seeds it (the first
    /// sweep lands one housekeep interval in, far past HTTP serving — age must
    /// start at 0, not "infinite"); [`retention::run`] stamps it after every
    /// `Ok` sweep. `max(1)` because `0` is the "never seeded" sentinel.
    pub(crate) fn mark_retention_ok(&self) {
        self.retention_ok_secs.store(coarse_now_secs().max(1), Ordering::SeqCst);
    }
}

/// The durable async-events plane of ONE process. Owned and driven by
/// `core/app::run` (never by a module or a `cmd/*` main): constructed when the
/// process has a DB, [`Plane::transport`] injected at `Context` construction,
/// [`Plane::migrate`] before module migrations, [`Plane::start`] after module
/// starts (the subscription snapshot must see every wiring-time `on_tx`),
/// [`Plane::stop`] before any module stops (delivery halts first, so a stopping
/// module never receives).
pub struct Plane {
    inner: Arc<LogTransport>,
    pool: PgPool,
    /// The DSN for the dedicated wake-up LISTEN connection — passed in by app
    /// (its authoritative `cfg.database_url`), never re-read from env here.
    listen_dsn: String,
    liveness: Liveness,
    /// Cancellation + background tasks, present between `start` and `stop`.
    stop: Option<(watch::Sender<bool>, Vec<JoinHandle<()>>)>,
    stop_grace: Duration,
    active_deliveries: worker::ActiveDeliveries,
}

impl Plane {
    /// No env reads, no I/O — construction is wiring-safe; the first DB touch is
    /// [`Plane::migrate`]. (`ASYNCEVENTS_HANDLER_TIMEOUT` is read at `start`.)
    pub fn new(pool: PgPool, listen_dsn: String) -> anyhow::Result<Plane> {
        Ok(Plane {
            inner: Arc::new(LogTransport::new()),
            pool,
            listen_dsn,
            liveness: Liveness::default(),
            stop: None,
            stop_grace: Duration::from_millis(DEFAULT_PLANE_STOP_GRACE_MS),
            active_deliveries: worker::ActiveDeliveries::default(),
        })
    }

    /// The [`bus::Transport`] to inject at `Context` construction
    /// (`Bus::with_transport`) — live from birth, so any wiring-time `on_tx`
    /// (module `init` or stub-factory `register`) records rather than panics.
    pub fn transport(&self) -> Arc<dyn Transport> {
        self.inner.clone()
    }

    /// The worker-health probe for `/readyz` (see [`Liveness`]).
    pub fn liveness(&self) -> Liveness {
        self.liveness.clone()
    }

    /// Creates the V2 event-log schema, seeds `plane_meta`, runs the [`store`]
    /// startup guards (cluster identity, prepared-transaction ban) — the earliest
    /// point with a pool, so a broken position model fails the boot, not the
    /// first emit — and DROPs the legacy push-plane tables (wipe-acceptable).
    /// Idempotent. Runs BEFORE module migrations so a module's first `emit_tx`
    /// always finds `asyncevents.append_event`.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        store::ensure_schema(&self.pool).await?;
        sqlx::raw_sql(LEGACY_DROP_DDL).execute(&self.pool).await?;
        store::startup_guards(&self.pool).await?;
        Ok(())
    }

    /// Launches delivery: reconcile every registered subscription into
    /// `asyncevents.subscriptions` (cursor materialized from `StartPosition` —
    /// see [`catalog`]; a spec-hash mismatch on an existing row FAILS STARTUP),
    /// then the worker pool, the NOTIFY wake-up listener, and the metrics
    /// refresh. Called after all module inits and stub registers (app calls this
    /// after `App::start`), so the snapshot is complete. Each task roots on a
    /// shared `watch` cancel; [`Plane::stop`] flips it and awaits every task.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Parse configuration before the subscription catalog is reconciled: an
        // invalid value must fail startup without mutating durable state.
        let handler_timeout = worker::handler_timeout_from_env()?;
        let housekeep_interval = retention::interval_from_env()?;
        let subs = self.inner.snapshot();
        catalog::reconcile(&self.pool, &subs).await?;

        let (stop_tx, stop_rx) = watch::channel(false);
        let mut tasks = Vec::new();

        self.liveness.stopping.store(false, Ordering::SeqCst);
        let ids: Vec<String> = subs.iter().map(|s| s.spec.id.to_string()).collect();
        if !subs.is_empty() {
            let wakeup = Arc::new(Notify::new());
            let ctx = Arc::new(worker::WorkerCtx {
                dsn: self.listen_dsn.clone(),
                subs,
                handler_timeout,
                wakeup: wakeup.clone(),
                active: self.active_deliveries.clone(),
                liveness: self.liveness.clone(),
            });
            // Seed the staleness clock: the first pass may lag on a cold DB while
            // HTTP is already serving — age must start at 0, not "infinite".
            self.liveness.mark_pass_ok();
            for worker_id in 0..WORKERS {
                let liveness = self.liveness.clone();
                let worker_ctx = ctx.clone();
                let worker_stop = stop_rx.clone();
                tasks.push(tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(worker::run(worker_id, worker_ctx, worker_stop))
                        .catch_unwind()
                        .await;
                    if !liveness.stopping.load(Ordering::SeqCst) {
                        if result.is_err() {
                            tracing::error!("asyncevents worker panicked while the plane was running");
                        } else {
                            tracing::error!("asyncevents worker exited while the plane was running");
                        }
                        liveness.dead.store(true, Ordering::SeqCst);
                    }
                }));
            }
            // Supervised like the workers, but with a LATENCY-only disposition:
            // the listener's loss never degrades correctness (the 1s poll is the
            // floor), so its death is a loud log + counter, never `dead`/readyz.
            let liveness = self.liveness.clone();
            let listen = wakeup::listen(self.listen_dsn.clone(), wakeup, stop_rx.clone());
            tasks.push(tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(listen).catch_unwind().await;
                if !liveness.stopping.load(Ordering::SeqCst) {
                    wakeup::listener_deaths().inc();
                    if result.is_err() {
                        tracing::error!("asyncevents wake-up listener panicked while the plane was running; delivery degrades to the 1s poll");
                    } else {
                        tracing::error!("asyncevents wake-up listener exited while the plane was running; delivery degrades to the 1s poll");
                    }
                }
            }));
        }
        // Checkpoint-coupled retention GC (interval from EVENTS_HOUSEKEEP_INTERVAL,
        // validated above). Runs regardless of whether this process hosts
        // subscriptions — GC is a DB-global property of the shared log, safe to run
        // redundantly per process. Its death flips ONLY `retention_dead` (its own
        // named readyz check): a GC outage is storage growth, not a serving outage,
        // so it must never take delivery out of rotation via the `dead` flag.
        // Seed the retention staleness clock before the task starts: the first
        // sweep lands one housekeep interval in (the immediate tick is consumed)
        // while HTTP already serves — age must start at 0, not "infinite".
        self.liveness.mark_retention_ok();
        let liveness = self.liveness.clone();
        let gc = retention::run(
            self.pool.clone(),
            housekeep_interval,
            self.liveness.clone(),
            stop_rx.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(gc).catch_unwind().await;
            if !liveness.stopping.load(Ordering::SeqCst) {
                if result.is_err() {
                    tracing::error!("asyncevents retention task panicked while the plane was running");
                } else {
                    tracing::error!("asyncevents retention task exited while the plane was running");
                }
                liveness.retention_dead.store(true, Ordering::SeqCst);
            }
        }));
        // Metrics refresh death is log-only: pure observability, never readyz.
        let refresh = plane_metrics::refresh_loop(self.pool.clone(), ids, stop_rx);
        tasks.push(tokio::spawn(async move {
            if std::panic::AssertUnwindSafe(refresh).catch_unwind().await.is_err() {
                tracing::error!("asyncevents metrics refresh task panicked while the plane was running");
            }
        }));

        self.stop = Some((stop_tx, tasks));
        Ok(())
    }

    /// Halts delivery FIRST (app calls this before `Bus::close`/`App::stop`, so
    /// no module receives while tearing down), then awaits the background loops
    /// — an in-flight delivery finishes its commit before the worker exits.
    /// Idempotent — a never-started plane is a no-op.
    pub async fn stop(&mut self) {
        if let Some((stop_tx, mut tasks)) = self.stop.take() {
            self.liveness.stopping.store(true, Ordering::SeqCst);
            let _ = stop_tx.send(true);
            let deadline = tokio::time::Instant::now() + self.stop_grace;
            // Reserve part of the one global budget for backend termination and
            // task cancellation. This avoids spending the whole deadline merely
            // waiting for a handler that is blocked in DB/network I/O.
            let cooperative = self.stop_grace.saturating_sub(
                self.stop_grace.min(Duration::from_secs(1)).max(self.stop_grace / 2),
            );
            let graceful = async {
                for task in &mut tasks { let _ = task.await; }
            };
            if tokio::time::timeout(cooperative, graceful).await.is_ok() {
                return;
            }
            // The timed join loop may already have consumed some handles. Do
            // not poll a completed JoinHandle a second time.
            tasks.retain(|task| !task.is_finished());

            let claims = self.active_deliveries.claim_active();
            for (worker_id, delivery) in claims {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() { break; }
                let dsn = self.listen_dsn.clone();
                let active = self.active_deliveries.clone();
                let claim = delivery.clone();
                let terminate = async move {
                    terminate_claim(&dsn, &active, worker_id, &claim).await
                };
                match tokio::time::timeout(remaining, terminate).await {
                    Ok(Ok(true)) => tracing::warn!(worker_id, generation = delivery.generation, pid = delivery.pid, "asyncevents: terminated stuck delivery backend"),
                    Ok(Ok(false)) => tracing::warn!(worker_id, generation = delivery.generation, pid = delivery.pid, "asyncevents: delivery backend was already gone"),
                    Ok(Err(err)) => tracing::warn!(worker_id, generation = delivery.generation, pid = delivery.pid, %err, "asyncevents: failed to terminate delivery backend; private socket will be force-closed"),
                    Err(_) => tracing::warn!(worker_id, generation = delivery.generation, pid = delivery.pid, "asyncevents: deadline exhausted terminating delivery backend; private socket will be force-closed"),
                }
            }

            for task in &tasks { task.abort(); }
            for mut task in tasks {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() { break; }
                let _ = tokio::time::timeout(remaining, &mut task).await;
            }
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn with_stop_grace(mut self, grace: Duration) -> Self {
        self.stop_grace = grace;
        self
    }
}

/// Test-only helpers — the single owner of the plane's physical table names
/// outside the plane itself. Plain `pub` items (NOT `#[cfg(test)]` — a
/// cross-crate `[dev-dependencies]` consumer can't see test-gated items).
pub mod testing {
    use std::sync::Arc;

    use bus::Transport;
    use sqlx::PgPool;

    use crate::LogTransport;

    /// Counts durable log events for a topic whose JSON payload has
    /// `payload_key == payload_value` (the V2 replacement for the old
    /// `outbox_count`). The key is a bind param (`payload->>$2`) so one prepared
    /// shape serves every caller.
    pub async fn events_count(
        pool: &PgPool,
        topic: &str,
        payload_key: &str,
        payload_value: &str,
    ) -> sqlx::Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM asyncevents.events WHERE topic = $1 AND payload->>$2 = $3",
        )
        .bind(topic)
        .bind(payload_key)
        .bind(payload_value)
        .fetch_one(pool)
        .await?;
        Ok(n)
    }

    /// Deletes log events whose JSON payload has `payload_key == payload_value`,
    /// returning the number of rows removed. Test teardown only (cursors row-compare,
    /// so a deleted position is simply skipped over).
    pub async fn cleanup_events(
        pool: &PgPool,
        payload_key: &str,
        payload_value: &str,
    ) -> sqlx::Result<u64> {
        let result = sqlx::query("DELETE FROM asyncevents.events WHERE payload->>$1 = $2")
            .bind(payload_key)
            .bind(payload_value)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// A bare durable transport plus a test-driveable worker, with no background
    /// tasks behind it. Module integration tests hand [`TestTransport::handle`]
    /// to `Context::with_db_and_transport` to get real log appends + `on_tx`
    /// recording, and call [`TestTransport::deliver_all`] to run a full
    /// reconcile-and-drain pass for emit→deliver round-trips.
    pub struct TestTransport {
        inner: Arc<LogTransport>,
        pool: PgPool,
        /// The DSN the drive-by worker sessions connect with — kept alongside the
        /// pool so `deliver_all`'s workers hit the SAME database `reconcile`
        /// targeted (a `PgPool` cannot be asked for its DSN back).
        dsn: String,
    }

    pub fn transport(pool: PgPool) -> TestTransport {
        transport_with_dsn(
            pool,
            std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
                    .to_string()
            }),
        )
    }

    /// Like [`transport`], but with an explicit worker DSN — for a test whose
    /// pool was NOT opened from `DATABASE_URL`.
    pub fn transport_with_dsn(pool: PgPool, dsn: String) -> TestTransport {
        TestTransport {
            inner: Arc::new(LogTransport::new()),
            pool,
            dsn,
        }
    }

    impl TestTransport {
        pub fn handle(&self) -> Arc<dyn Transport> {
            self.inner.clone()
        }

        /// Reconciles the registered subscriptions, then drains every one until
        /// no eligible events remain. Returns the number of deliveries. NOTE:
        /// eligibility is frontier-bounded — a concurrently open foreign
        /// transaction can defer a just-committed event, so round-trip tests
        /// poll this rather than calling it once.
        pub async fn deliver_all(&self) -> anyhow::Result<u64> {
            let subs = self.inner.snapshot();
            crate::catalog::reconcile(&self.pool, &subs).await?;
            let ctx = crate::worker::WorkerCtx {
                dsn: self.dsn.clone(),
                subs,
                handler_timeout: std::time::Duration::from_secs(10),
                wakeup: Arc::new(tokio::sync::Notify::new()),
                active: crate::worker::ActiveDeliveries::default(),
                liveness: crate::Liveness::default(),
            };
            let mut total = 0u64;
            loop {
                let n = crate::worker::drain_pass(&ctx, None).await;
                total += n;
                if n == 0 {
                    return Ok(total);
                }
            }
        }
    }
}

// ============================================================================
// Integration tests — live Postgres (the local DB is the test DB). Each guarded
// by a `test_pool` that SKIPs (early-returns with a message) when Postgres is
// down. In-crate (not `tests/`) so they can drive the private worker/catalog.
// ============================================================================
#[cfg(test)]
mod tests;

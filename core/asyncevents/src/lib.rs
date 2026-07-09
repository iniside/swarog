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
//! [`wakeup`] the NOTIFY listener; [`plane_metrics`] the lag/frontier gauges.

mod catalog;
mod plane_metrics;
pub mod store;
mod transport;
mod wakeup;
mod worker;

pub use transport::LogTransport;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bus::Transport;
use sqlx::PgPool;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

/// Pull workers per plane. Each drains due subscriptions independently;
/// `FOR UPDATE SKIP LOCKED` arbitrates, so the count is throughput tuning, not
/// correctness.
const WORKERS: usize = 2;

/// Drops the LEGACY push-plane storage (outbox/inbox/notify trigger and the
/// pre-rename `messaging` schema). The fresh-start decision makes this a plain
/// wipe: no data migrates — V2 positions are a different coordinate system.
const LEGACY_DROP_DDL: &str = r#"
DROP TABLE IF EXISTS asyncevents.outbox CASCADE;
DROP TABLE IF EXISTS asyncevents.inbox CASCADE;
DROP FUNCTION IF EXISTS asyncevents.notify_outbox() CASCADE;
DROP SCHEMA IF EXISTS messaging CASCADE;"#;

/// A cloneable worker-health probe: flipped once if any worker task dies while
/// the plane is running (panic or premature exit). `app::run` folds it into
/// `/readyz` as a named `httpmw::ReadyCheck` — a process whose delivery loop is
/// gone must stop taking traffic that expects its effects.
#[derive(Clone, Default)]
pub struct Liveness {
    dead: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
}

impl Liveness {
    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
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
        let subs = self.inner.snapshot();
        catalog::reconcile(&self.pool, &subs).await?;

        let (stop_tx, stop_rx) = watch::channel(false);
        let mut tasks = Vec::new();

        let ids: Vec<String> = subs.iter().map(|s| s.spec.id.to_string()).collect();
        if !subs.is_empty() {
            let wakeup = Arc::new(Notify::new());
            let ctx = Arc::new(worker::WorkerCtx {
                pool: self.pool.clone(),
                subs,
                handler_timeout: worker::handler_timeout_from_env(),
                wakeup: wakeup.clone(),
            });
            self.liveness.stopping.store(false, Ordering::SeqCst);
            for _ in 0..WORKERS {
                let inner = tokio::spawn(worker::run(ctx.clone(), stop_rx.clone()));
                let liveness = self.liveness.clone();
                // Supervisor: a worker that ends while the plane is NOT stopping
                // (panic or bug) marks the plane dead — /readyz goes 503.
                tasks.push(tokio::spawn(async move {
                    let res = inner.await;
                    if !liveness.stopping.load(Ordering::SeqCst) {
                        if let Err(err) = &res {
                            tracing::error!(%err, "asyncevents worker task died");
                        } else {
                            tracing::error!("asyncevents worker exited while the plane was running");
                        }
                        liveness.dead.store(true, Ordering::SeqCst);
                    }
                }));
            }
            tasks.push(tokio::spawn(wakeup::listen(
                self.listen_dsn.clone(),
                wakeup,
                stop_rx.clone(),
            )));
        }
        tasks.push(tokio::spawn(plane_metrics::refresh_loop(
            self.pool.clone(),
            ids,
            stop_rx,
        )));

        self.stop = Some((stop_tx, tasks));
        Ok(())
    }

    /// Halts delivery FIRST (app calls this before `Bus::close`/`App::stop`, so
    /// no module receives while tearing down), then awaits the background loops
    /// — an in-flight delivery finishes its commit before the worker exits.
    /// Idempotent — a never-started plane is a no-op.
    pub async fn stop(&mut self) {
        if let Some((stop_tx, tasks)) = self.stop.take() {
            self.liveness.stopping.store(true, Ordering::SeqCst);
            let _ = stop_tx.send(true);
            for t in tasks {
                let _ = t.await;
            }
        }
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
    }

    pub fn transport(pool: PgPool) -> TestTransport {
        TestTransport {
            inner: Arc::new(LogTransport::new()),
            pool,
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
                pool: self.pool.clone(),
                subs,
                handler_timeout: std::time::Duration::from_secs(10),
                wakeup: Arc::new(tokio::sync::Notify::new()),
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

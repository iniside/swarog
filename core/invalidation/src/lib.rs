//! `invalidation` — the broadcast cache-refresh plane. It is app-owned process
//! infrastructure (like the durable-events plane and the HTTP listener), NOT a
//! `lifecycle::Module`: `core/app::run` constructs an [`InvalidationPlane`] iff the
//! process has a DB (DB ⇒ plane), exposes its registration [`handle`](InvalidationPlane::handle)
//! at `Context` construction, starts it after module `start`, and stops it before any
//! module stops. Processes without a DB (`gateway-svc`) host no plane — and have no
//! cache consumers.
//!
//! **What it promises: FRESHNESS, not delivery.** A replica-local cache (config's
//! `Service`/`CachedConfig`, inventory's starter spec) registers an authoritative
//! refresh callback under a Postgres LISTEN/NOTIFY channel; every committed NOTIFY on
//! that channel re-runs the matching callbacks. This is deliberately NOT a durable
//! subscription: it carries no checkpoint, replays nothing on its own, and guarantees
//! only that after a change settles, every registered refresh has re-run. Because there
//! is no cursor, one process's failure to refresh cannot block another's — the opposite
//! of consumer-group semantics, which is exactly why replica-local caches belong here
//! and not on the durable plane (where only one replica would ever refresh).
//!
//! **Atomicity is the callback's job.** A refresh must swap the whole cache in one
//! step (build a fresh map, then replace under the lock) — never mutate in place — so a
//! concurrent reader always sees a coherent snapshot. The plane guarantees only that the
//! callback is *invoked*; the callback owns its own consistency.
//!
//! **Freshness floors.** One `PgListener` per process LISTENs on every registered
//! channel. Each committed NOTIFY invokes that channel's callbacks independently (a
//! failing callback never blocks a sibling). A reconnect performs a full refresh of
//! every callback — PG queues no NOTIFY for a dead session, so any change missed while
//! disconnected heals on the next connect. A 30s poll (`INVALIDATION_POLL_INTERVAL_MS`)
//! is the lost-NOTIFY fallback, re-running every callback regardless of NOTIFY. Startup
//! runs each callback's first refresh synchronously and fails loudly if one fails;
//! readiness ([`Health`]) reports unready once any callback has gone 60s without a
//! successful refresh.
//!
//! **Callbacks are deadline-bounded.** Every refresh runs under a timeout
//! (`INVALIDATION_CALLBACK_TIMEOUT_MS`, default 10s) so a hung callback can't wedge the
//! NOTIFY fan-out, the poll fallback, or startup — like every other plane in this repo,
//! the refresh path is time-bounded. A startup first-refresh timeout fails boot (the
//! same fail-loud contract as any first-refresh error); a steady-state timeout counts as
//! a refresh failure (logged + failure gauge), so the stale clock keeps ticking and
//! readiness eventually reports it. [`stop`](InvalidationPlane::stop) is likewise bounded
//! (5s per background task, then `abort`) so a hung callback can't stall teardown.

mod gauges;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use sqlx::postgres::PgListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Default lost-NOTIFY poll interval; overridden by `INVALIDATION_POLL_INTERVAL_MS`.
const DEFAULT_POLL: Duration = Duration::from_secs(30);

/// Default per-callback refresh deadline; overridden by `INVALIDATION_CALLBACK_TIMEOUT_MS`.
const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Grace for a background task to exit after `stop` signals before it is aborted.
/// Deliberately a compile-time constant, NOT an env knob (like `core/app`'s
/// `READY_CHECK_TIMEOUT`): teardown promptness is not a per-deployment tuning surface.
const DEFAULT_STOP_GRACE_MS: u64 = 5000;

/// Readiness threshold: a callback with no successful refresh in this long → unready.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

type RefreshFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
type RefreshFn = Arc<dyn Fn() -> RefreshFuture + Send + Sync>;

/// One registered refresh: a NOTIFY `channel`, a stable `name` (readiness/metrics
/// label), and the authoritative reload closure. `Clone` is cheap — the closure is an
/// `Arc` — so the plane snapshots registrations by clone at `start`.
#[derive(Clone)]
struct Registration {
    channel: String,
    name: String,
    refresh: RefreshFn,
}

impl Registration {
    /// Runs the refresh under deadline `d`. A timeout is a loud error (not a hang): in
    /// steady state it flows through [`RunCtx::run_one`]'s failure path; at startup it
    /// fails `start` — so a wedged callback can never silently stall the plane.
    async fn run(&self, d: Duration) -> anyhow::Result<()> {
        match tokio::time::timeout(d, (self.refresh)()).await {
            Ok(r) => r,
            Err(_) => anyhow::bail!("refresh timed out after {d:?}"),
        }
    }
}

/// The registration handle exposed via `Context::invalidation()`. A cache consumer
/// calls [`register`](Invalidation::register) during module `init`/`register` (no I/O
/// then — the closure only runs once the plane starts). Always present on a `Context`
/// (mirroring the bus): a DB-less process's handle simply has no plane draining it, and
/// such a process hosts no cache consumers.
#[derive(Default)]
pub struct Invalidation {
    regs: Mutex<Vec<Registration>>,
}

impl Invalidation {
    pub fn new() -> Invalidation {
        Invalidation {
            regs: Mutex::new(Vec::new()),
        }
    }

    /// Registers an authoritative refresh `callback` fired on every committed NOTIFY on
    /// `channel` (plus reconnect heals and the poll fallback). `name` labels the callback
    /// in readiness and metrics. Wiring-only: the closure does not run until the plane
    /// starts.
    pub fn register<F, Fut>(&self, channel: &str, name: &str, callback: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        assert!(
            !channel.is_empty(),
            "invalidation channel must not be empty"
        );
        assert!(
            !name.is_empty(),
            "invalidation callback name must not be empty"
        );

        let mut regs = self.regs.lock().unwrap();
        if regs.iter().any(|registration| registration.name == name) {
            drop(regs);
            panic!("duplicate invalidation callback name {name:?}");
        }
        regs.push(Registration {
            channel: channel.to_string(),
            name: name.to_string(),
            refresh: Arc::new(move || Box::pin(callback())),
        });
    }

    fn snapshot(&self) -> Vec<Registration> {
        self.regs.lock().unwrap().clone()
    }
}

/// Per-callback last-success clock backing `/readyz` and the age gauge. Cloneable so
/// `app::run` can fold a probe into `httpmw::READINESS_SLOT` without owning the plane.
#[derive(Clone, Default)]
pub struct Health {
    last_success: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Health {
    fn mark(&self, name: &str) {
        self.last_success
            .lock()
            .unwrap()
            .insert(name.to_string(), Instant::now());
    }

    /// Names whose last successful refresh is older than `max_age`, sorted — the
    /// unready set `/readyz` reports. Empty (including before `start` seeds it, which
    /// is never observable: HTTP serves only after `start`) means ready.
    pub fn stale(&self, max_age: Duration) -> Vec<String> {
        let now = Instant::now();
        let mut out: Vec<String> = self
            .last_success
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| now.duration_since(**t) > max_age)
            .map(|(n, _)| n.clone())
            .collect();
        out.sort();
        out
    }

    fn ages(&self) -> Vec<(String, f64)> {
        let now = Instant::now();
        self.last_success
            .lock()
            .unwrap()
            .iter()
            .map(|(n, t)| (n.clone(), now.duration_since(*t).as_secs_f64()))
            .collect()
    }
}

/// The running fan-out state shared by the listener, poll, and reconnect paths.
struct RunCtx {
    all: Vec<Registration>,
    by_channel: HashMap<String, Vec<Registration>>,
    health: Health,
    gauges: gauges::Gauges,
    /// Per-callback refresh deadline applied to every steady-state run.
    callback_timeout: Duration,
}

impl RunCtx {
    /// Runs one callback, isolating its outcome: success marks the health clock; an
    /// error (including a deadline timeout) is logged and counted, never propagated (a
    /// sibling must still run).
    async fn run_one(&self, reg: &Registration) {
        match reg.run(self.callback_timeout).await {
            Ok(()) => self.health.mark(&reg.name),
            Err(err) => {
                tracing::error!(callback = %reg.name, %err, "invalidation refresh failed");
                self.gauges.inc_failure(&reg.name);
            }
        }
    }

    /// Every callback registered on `channel`, each isolated — a NOTIFY fan-out.
    async fn run_channel(&self, channel: &str) {
        let Some(regs) = self.by_channel.get(channel) else {
            tracing::warn!(%channel, "invalidation NOTIFY on unregistered channel");
            return;
        };
        for reg in regs {
            self.run_one(reg).await;
        }
    }

    /// A full refresh of every callback — the reconnect heal and the poll fallback.
    async fn refresh_all(&self) {
        for reg in &self.all {
            self.run_one(reg).await;
        }
    }
}

/// The broadcast cache-invalidation plane of ONE process. Owned and driven by
/// `core/app::run`: constructed when the process has a DB, [`handle`](Self::handle)
/// injected at `Context` construction so module `init` can register callbacks,
/// [`start`](Self::start) after module starts (the snapshot must see every wiring-time
/// registration), [`stop`](Self::stop) before any module stops.
pub struct InvalidationPlane {
    registrar: Arc<Invalidation>,
    /// The DSN for the dedicated LISTEN connection — app's authoritative
    /// `cfg.database_url`, never re-read from env here.
    listen_dsn: String,
    poll: Duration,
    callback_timeout: Duration,
    health: Health,
    gauges: gauges::Gauges,
    /// Cancellation + background tasks, present between `start` and `stop`.
    stop: Option<(watch::Sender<bool>, Vec<JoinHandle<()>>)>,
}

impl InvalidationPlane {
    /// No env reads beyond the poll interval, no I/O — construction is wiring-safe; the
    /// first DB touch is [`start`](Self::start).
    pub fn new(listen_dsn: String) -> InvalidationPlane {
        InvalidationPlane {
            registrar: Arc::new(Invalidation::new()),
            listen_dsn,
            poll: poll_from_env(),
            callback_timeout: callback_timeout_from_env(),
            health: Health::default(),
            gauges: gauges::Gauges::new(),
            stop: None,
        }
    }

    /// The Prometheus collectors for `core/metrics::register` (called once by `app::run`,
    /// which owns the process registry — this crate must not depend on `core/metrics`).
    pub fn collectors(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.gauges.collectors()
    }

    /// Test-only override of the poll interval (prod reads `INVALIDATION_POLL_INTERVAL_MS`).
    pub fn with_poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// Test-only override of the per-callback refresh deadline (prod reads
    /// `INVALIDATION_CALLBACK_TIMEOUT_MS`).
    pub fn with_callback_timeout(mut self, timeout: Duration) -> Self {
        self.callback_timeout = timeout;
        self
    }

    /// The registration handle to hand `Context` (`Context::with_invalidation`) — live
    /// from birth, so any wiring-time `register` records rather than panics.
    pub fn handle(&self) -> Arc<Invalidation> {
        self.registrar.clone()
    }

    /// The last-success clock for `/readyz` (see [`Health`]).
    pub fn readiness(&self) -> Health {
        self.health.clone()
    }

    /// Runs each registered callback's FIRST refresh synchronously — a failure fails
    /// startup loudly (the boot guarantee: no cache is stale-ready) — then spawns the
    /// NOTIFY listener, the poll fallback, and the metrics loop. Called after every
    /// module `init`/`start` so the snapshot is complete.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let regs = self.registrar.snapshot();
        for reg in &regs {
            reg.run(self.callback_timeout)
                .await
                .with_context(|| format!("invalidation first refresh for {:?}", reg.name))?;
            self.health.mark(&reg.name);
        }
        if regs.is_empty() {
            return Ok(());
        }

        let mut by_channel: HashMap<String, Vec<Registration>> = HashMap::new();
        for reg in &regs {
            by_channel.entry(reg.channel.clone()).or_default().push(reg.clone());
        }
        let channels: Vec<String> = by_channel.keys().cloned().collect();
        let ctx = Arc::new(RunCtx {
            all: regs,
            by_channel,
            health: self.health.clone(),
            gauges: self.gauges.clone(),
            callback_timeout: self.callback_timeout,
        });

        let (stop_tx, stop_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(listen(
                self.listen_dsn.clone(),
                channels,
                ctx.clone(),
                stop_rx.clone(),
            )),
            tokio::spawn(poll_loop(ctx.clone(), self.poll, stop_rx.clone())),
            tokio::spawn(gauges::refresh_loop(
                self.gauges.clone(),
                self.health.clone(),
                stop_rx,
            )),
        ];
        self.stop = Some((stop_tx, tasks));
        Ok(())
    }

    /// Halts the background loops and awaits their exit, bounded per task. Idempotent — a
    /// never-started plane is a no-op. A task that hasn't exited within
    /// [`DEFAULT_STOP_GRACE_MS`] (e.g. blocked in a hung callback mid-refresh) is aborted
    /// so one wedged callback can't stall teardown — safe because a task only holds a
    /// `PgListener`, dropped at the abort's await point.
    pub async fn stop(&mut self) {
        if let Some((stop_tx, tasks)) = self.stop.take() {
            let _ = stop_tx.send(true);
            let grace = Duration::from_millis(DEFAULT_STOP_GRACE_MS);
            for mut t in tasks {
                if tokio::time::timeout(grace, &mut t).await.is_err() {
                    t.abort();
                }
            }
        }
    }
}

/// Keeps one dedicated `PgListener` LISTENing on every registered channel. Never dies on
/// a DB outage: each (re)connect backs off on failure. Every connect does a full refresh
/// (the reconnect heal — PG queues no NOTIFY for a dead session), then fans each NOTIFY
/// out to its channel's callbacks.
async fn listen(
    dsn: String,
    channels: Vec<String>,
    ctx: Arc<RunCtx>,
    mut stop: watch::Receiver<bool>,
) {
    while !*stop.borrow() {
        let mut listener = match PgListener::connect(&dsn).await {
            Ok(l) => l,
            Err(err) => {
                tracing::error!(%err, "invalidation listener connect failed");
                backoff(&mut stop).await;
                continue;
            }
        };
        if let Err(err) = listener.listen_all(channels.iter().map(String::as_str)).await {
            tracing::error!(%err, "invalidation LISTEN failed");
            backoff(&mut stop).await;
            continue;
        }
        // Reconnect heal (also covers a change committed between `start`'s boot refresh
        // and this LISTEN): a full refresh catches anything missed while disconnected.
        ctx.refresh_all().await;
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                res = listener.recv() => match res {
                    Ok(notif) => ctx.run_channel(notif.channel()).await,
                    Err(err) => {
                        tracing::error!(%err, "invalidation listener recv failed");
                        break; // reconnect via the outer loop (conn dropped on break)
                    }
                }
            }
        }
        backoff(&mut stop).await;
    }
}

/// The lost-NOTIFY fallback: a full refresh of every callback each interval, regardless
/// of NOTIFY delivery. The immediate first tick is consumed (boot + connect already
/// refreshed), so the first fallback refresh is one interval out.
async fn poll_loop(ctx: Arc<RunCtx>, interval: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => ctx.refresh_all().await,
        }
    }
}

/// Waits ~1s, returning early if `stop` flips so shutdown stays prompt and a reconnect
/// storm never tight-spins.
async fn backoff(stop: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = stop.changed() => {}
        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
}

/// Reads `INVALIDATION_POLL_INTERVAL_MS` (positive ms), else [`DEFAULT_POLL`].
fn poll_from_env() -> Duration {
    match std::env::var("INVALIDATION_POLL_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => DEFAULT_POLL,
    }
}

/// Reads `INVALIDATION_CALLBACK_TIMEOUT_MS` (positive ms), else [`DEFAULT_CALLBACK_TIMEOUT`].
fn callback_timeout_from_env() -> Duration {
    match std::env::var("INVALIDATION_CALLBACK_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => DEFAULT_CALLBACK_TIMEOUT,
    }
}

#[cfg(test)]
mod tests;

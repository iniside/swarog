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

/// Dedicated Postgres sessions this plane holds while running: exactly one
/// `PgListener` per process, LISTENing on every registered channel (see the
/// "Freshness floors" doc above). Public so the fleet's Postgres session budget
/// (`tools/processctl`) reserves it as a per-DB-process singleton.
pub const LISTEN_SESSIONS: usize = 1;

/// Grace for a background task to exit after `stop` signals before it is aborted.
/// Deliberately a compile-time constant, NOT an env knob (like `core/app`'s
/// `READY_CHECK_TIMEOUT`): teardown promptness is not a per-deployment tuning surface.
const DEFAULT_STOP_GRACE_MS: u64 = 5000;

/// Bounded grace to await an already-aborted task's unwind before moving on. A
/// cooperative future drops its `PgListener`/lock at the abort's `.await` point and joins
/// within this window; a non-cooperative (CPU-spinning, never-yielding) future — which
/// `abort()` cannot preempt anyway — is left detached rather than stalling teardown, so
/// this stays a bound, not an unbounded join. A small fraction of [`DEFAULT_STOP_GRACE_MS`].
const POST_ABORT_JOIN_GRACE_MS: u64 = 500;

/// Readiness threshold: a callback with no successful refresh in this long → unready.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

type RefreshFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
type RefreshFn = Arc<dyn Fn() -> RefreshFuture + Send + Sync>;

/// One registered refresh: a NOTIFY `channel`, a stable `name` (readiness/metrics
/// label), the authoritative reload closure, and a per-callback serialization lock.
/// `Clone` is cheap — both the closure and the lock are `Arc`s — so the plane snapshots
/// registrations by clone at `start` and every clone shares the SAME `lock` (an `Arc`
/// clone is a refcount bump). The lock is created ONCE at [`Invalidation::register`], so
/// the `Clone` fanning a registration into `RunCtx::all` and `RunCtx::by_channel`
/// serializes the callback against itself across the listener, poll, and reconnect tasks.
#[derive(Clone)]
struct Registration {
    channel: String,
    name: String,
    refresh: RefreshFn,
    /// Held for the duration of one refresh so a given callback never overlaps itself.
    /// Different callbacks hold different locks and still run concurrently. Shared across
    /// every `Clone` of this registration (Arc), so the listener and poll tasks contend on
    /// the same lock for the same callback — the reorder window (an older snapshot landing
    /// after a newer one) closes when combined with the callback's own monotonic guard.
    lock: Arc<tokio::sync::Mutex<()>>,
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
    ///
    /// **The callback MUST be idempotent and apply-only-newer (monotonic).** The plane
    /// serializes a callback against ITSELF (a per-callback lock: two of the listener,
    /// poll, and reconnect tasks never run the same callback concurrently), but it does
    /// NOT order a refresh against a newer EXTERNAL write — a refresh that queried before
    /// the latest commit can still finish after a later refresh that saw it. So the
    /// callback owns freshness: build a fresh snapshot, then apply it only if it is newer
    /// than what the cache already holds (config's `revision <= guard.revision` guard),
    /// swapping the whole cache in one step (see the module doc — "atomicity is the
    /// callback's job"). Without the callback's own monotonic guard the plane alone does
    /// not prevent an older snapshot from overwriting a newer one.
    ///
    /// **Head-of-line bound.** Because same-callback runs QUEUE on the lock (never skip),
    /// a poll pass reaching callback A while the listener holds A's lock waits up to A's
    /// remaining `INVALIDATION_CALLBACK_TIMEOUT_MS` before running A — bounded and
    /// acceptable for a freshness floor. Queueing (not `try_lock`-skip) is deliberate:
    /// skipping could drop the newest snapshot, since the in-flight run may have queried
    /// before the latest commit.
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
            // Created ONCE here, then shared (Arc) across every `Clone` into `all` and
            // `by_channel` — a fresh Mutex per clone would serialize nothing.
            lock: Arc::new(tokio::sync::Mutex::new(())),
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
    /// sibling must still run). Holds the callback's per-registration lock across the
    /// (timeout-wrapped) refresh so a given callback never overlaps itself across the
    /// listener/poll/reconnect tasks — the second invocation reads a fresh snapshot only
    /// after the first completes. Different callbacks contend on different locks and still
    /// run concurrently. The wait for a contended lock is bounded by the in-flight run's
    /// remaining `callback_timeout` (the head-of-line bound documented on `register`).
    async fn run_one(&self, reg: &Registration) {
        let _serialize = reg.lock.lock().await;
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
    /// No I/O — construction is wiring-safe; the first DB touch is
    /// [`start`](Self::start). Fails loudly (never silently defaults) on an EXPLICIT
    /// zero or overflowing `INVALIDATION_POLL_INTERVAL_MS`/`INVALIDATION_CALLBACK_TIMEOUT_MS`
    /// (mirroring `asyncevents::retention::Config`'s posture: absent → default, malformed
    /// → default, explicit zero → fail, overflow → fail) — so `core/app::run` propagates a
    /// bad knob into a boot failure instead of a plane that silently reverts to the default
    /// poll/timeout.
    pub fn new(listen_dsn: String) -> anyhow::Result<InvalidationPlane> {
        Ok(InvalidationPlane {
            registrar: Arc::new(Invalidation::new()),
            listen_dsn,
            poll: poll_from_env()?,
            callback_timeout: callback_timeout_from_env()?,
            health: Health::default(),
            gauges: gauges::Gauges::new(),
            stop: None,
        })
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
    ///
    /// After `abort()` we await the task once more, BOUNDED by
    /// [`POST_ABORT_JOIN_GRACE_MS`]: a cooperative future unwinds (dropping its
    /// `PgListener`/lock) and joins within that window, so a subsequent `Module::stop`
    /// does not race a still-unwinding task. The bound is essential — an UNBOUNDED
    /// `(&mut t).await` here would re-introduce the very stall the pre-abort timeout
    /// exists to prevent, because `abort()` cannot preempt a non-cooperative
    /// (CPU-spinning) future; such a future is left detached rather than stalling teardown.
    pub async fn stop(&mut self) {
        if let Some((stop_tx, tasks)) = self.stop.take() {
            let _ = stop_tx.send(true);
            let grace = Duration::from_millis(DEFAULT_STOP_GRACE_MS);
            let post_abort = Duration::from_millis(POST_ABORT_JOIN_GRACE_MS);
            for mut t in tasks {
                if tokio::time::timeout(grace, &mut t).await.is_err() {
                    t.abort();
                    // Bounded confirm-unwind: cooperative tasks join fast; a
                    // non-cooperative one is left detached rather than stalling teardown.
                    let _ = tokio::time::timeout(post_abort, &mut t).await;
                }
            }
        }
    }
}

/// Keeps one dedicated `PgListener` LISTENing on every registered channel. Never dies on
/// a DB outage: each (re)connect backs off on failure. Every connect does a full refresh
/// (the reconnect heal — PG queues no NOTIFY for a dead session), then fans each NOTIFY
/// out to its channel's callbacks. A lost connection (a terminated backend / dropped
/// session) surfaces from `try_recv` as `Ok(None)` or `Err` and drives a reconnect +
/// heal; see the inner-loop comment for why `try_recv` (not `recv`) is required on darwin.
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
        // We own reconnection via THIS outer loop (each connect re-LISTENs and refreshes),
        // so disable `PgListener`'s transparent auto-reconnect — see the `try_recv` comment
        // in the inner loop for why surfacing the loss to us (rather than sqlx silently
        // healing it) is load-bearing on darwin.
        listener.eager_reconnect(false);
        if let Err(err) = listener.listen_all(channels.iter().map(String::as_str)).await {
            tracing::error!(%err, "invalidation LISTEN failed");
            backoff(&mut stop).await;
            continue;
        }
        // Reconnect heal (also covers a change committed between `start`'s boot refresh
        // and this LISTEN): a full refresh catches anything missed while disconnected.
        ctx.refresh_all().await;
        loop {
            // `try_recv` (not `recv`) is deliberate and platform-load-bearing. A terminated
            // backend must surface as a reconnect trigger HERE so the outer loop re-LISTENs
            // and `refresh_all`s (the heal). `recv` masks that: it internally loops
            // `try_recv`, and when the connection drops it transparently reconnects and
            // blocks on the fresh (healthy) session — so on darwin, where a graceful FIN
            // from `pg_terminate_backend` surfaces only as an I/O close (never as a Linux
            // FATAL `ErrorResponse` that `recv` would forward as `Err`), `recv` heals the
            // socket silently and NEVER returns, and the reconnect-refresh never fires.
            // `try_recv` instead returns `Ok(None)` on that I/O close (observed ~12ms on
            // darwin) and `Err` on a Linux FATAL message; both break to the outer loop and
            // refresh. A NOTIFY still wakes `try_recv` immediately, so happy-path latency is
            // unchanged; the 30s poll remains the freshness floor for a silent (no-FIN)
            // partition that neither path can observe promptly.
            tokio::select! {
                _ = stop.changed() => return,
                res = listener.try_recv() => match res {
                    Ok(Some(notif)) => ctx.run_channel(notif.channel()).await,
                    Ok(None) => {
                        tracing::warn!("invalidation listener connection lost; reconnecting");
                        break; // reconnect via the outer loop (conn dropped on break)
                    }
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
fn poll_from_env() -> anyhow::Result<Duration> {
    parse_ms_env("INVALIDATION_POLL_INTERVAL_MS", DEFAULT_POLL)
}

/// Reads `INVALIDATION_CALLBACK_TIMEOUT_MS` (positive ms), else [`DEFAULT_CALLBACK_TIMEOUT`].
fn callback_timeout_from_env() -> anyhow::Result<Duration> {
    parse_ms_env("INVALIDATION_CALLBACK_TIMEOUT_MS", DEFAULT_CALLBACK_TIMEOUT)
}

/// The shared decision authority for both millisecond-duration env knobs on this
/// plane. Mirrors `asyncevents::retention::Config`'s posture: absent (unset or
/// blank) → `default`; malformed (non-numeric garbage) → `default` (the historical
/// "garbage is tolerated" fallback); an EXPLICIT zero fails loudly rather than
/// silently reverting to the default — zero would otherwise busy-loop the
/// poller/callback deadline with no operator signal that their setting was ignored;
/// and a numeric OVERFLOW likewise fails loudly (retention bails on overflow too):
/// an all-digits value past `u64::MAX` milliseconds is parseable-but-invalid
/// operator INPUT, not a typo, so folding it into the garbage fallback would
/// silently ignore a value the operator clearly meant.
fn parse_ms_env(name: &str, default: Duration) -> anyhow::Result<Duration> {
    let raw = match std::env::var(name) {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(default),
        // Present-but-non-Unicode is malformed, not absent; malformed falls back to
        // the default (same policy as an unparseable numeric string below).
        Err(std::env::VarError::NotUnicode(_)) => return Ok(default),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    match trimmed.parse::<u64>() {
        Ok(0) => anyhow::bail!(
            "{name}=0 is invalid: a zero duration is rejected rather than silently \
             falling back to the default ({default:?}) — set a positive millisecond \
             value or unset {name} entirely"
        ),
        Ok(ms) => Ok(Duration::from_millis(ms)),
        // Overflow is a deliberate huge number, not garbage — fail loud (the same
        // posture as the retention parser, which bails on overflow).
        Err(err) if *err.kind() == std::num::IntErrorKind::PosOverflow => anyhow::bail!(
            "{name} overflows u64 milliseconds: set a smaller positive value or \
             unset {name} entirely (default {default:?})"
        ),
        // Malformed (non-numeric, negative) mirrors the historical "garbage is
        // tolerated" knob convention: fall back to the default, never fail.
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests;

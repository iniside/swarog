//! The drain: the loop that turns `pending` outbox rows into delivered mail.
//!
//! Shaped after `modules/scheduler`'s emission loop — a `start`-spawned task under a
//! `catch_unwind` supervision wrapper, one shared budget per pass, a `watch` stop signal
//! honoured between rows, and grace-then-abort in `stop`. It reacts to no event: a
//! `scheduler.fired` cadence is seconds-granularity and one delivery per fire, which is
//! right for a daily sweep and wrong for "send this the moment it is enqueued".
//!
//! ## Why a claim burns an attempt up front
//! There is deliberately no `sending` state (see [`crate::SCHEMA_DDL`]). The claim is a
//! COMMITTED update that bumps `attempts` and `generation` and pushes `next_attempt_at`
//! out by [`claim_lease`], so a process that dies mid-send leaves the row due again when
//! the lease expires, at the cost of one burnt attempt — fail-closed toward parking rather
//! than toward an unbounded resend loop. The lease is 3x the send budget, and
//! [`write_budget`] spends the remainder, so on a healthy pass the status write lands
//! inside the lease whenever `MAIL_SEND_TIMEOUT_MS >= ACQUIRE_DEADLINE`. Below that the
//! checkout floor wins and a re-claim can overlap a send in flight. What keeps the row
//! correct then is the `generation` leg of the status write's CAS, and only because that
//! counter is MONOTONE: the superseded attempt's write matches nothing and is counted.
//! `attempts` cannot serve as that leg — an operator requeue resets it to 0, so an older
//! attempt's value becomes reachable again after one fresh claim and its write would flip
//! a row that is being delivered right now to `sent`. The cost of the overlap is a
//! duplicate delivery, which is the at-least-once contract, not a new failure mode.
//!
//! ## Why the pool connection is not held across a send
//! A split service's pool is 2 connections (`SPLIT_SERVICE_POOL_MAX`). Holding one idle
//! for the length of a third-party SMTP dialogue would leave the process one connection
//! for everything else, so each DB step takes its own bounded checkout and gives it back
//! before the send. Every statement runs under a `SET LOCAL statement_timeout` inside its
//! own transaction — a `SET` on a pooled connection would leak into whoever gets it next.
//!
//! ## Why `/readyz` needs a stamp, not just a died flag
//! A loop that is alive but failing every pass never exits, so `dead` alone would keep
//! `/readyz` green forever. [`Liveness`] carries a coarse-monotonic stamp of the last
//! fully-healthy pass, aged out at [`stall_max`] — which is DERIVED from [`pass_budget`],
//! never an independent literal beside the budget it shadows.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use prometheus::{Gauge, IntCounter, IntGauge};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::watch;

use crate::providers::{Outgoing, SendError, Sender};
use crate::store::{truncate_error, Claimed, Disposition, Store};

/// How often the drain looks for due rows. It bounds send LATENCY, not correctness —
/// `next_attempt_at` is the authority, so a slow pass never sends twice.
const DRAIN_INTERVAL: Duration = Duration::from_secs(1);

/// Rows one pass will attempt before yielding to the next interval tick. A cap, not a
/// target: the pass ends earlier when the outbox runs dry or the budget runs out.
const DRAIN_BATCH: usize = 16;

/// FLOOR for the budget one pass shares (scheduler's model). A pass that hits its budget
/// ends and the next tick continues — leased rows are simply due again.
const DRAIN_DEADLINE: Duration = Duration::from_secs(30);

/// Bound on OBTAINING a pool connection. Dropping a pending checkout carries no session
/// state, so cancelling it is safe — unlike cancelling in-flight work, which the session
/// `statement_timeout` bounds instead.
pub(crate) const ACQUIRE_DEADLINE: Duration = Duration::from_secs(5);

/// How long `stop` waits for the loop to exit before ABORTING it. Deliberately under
/// `core/app`'s 5s `MODULE_STOP_GRACE_MS`, so this module resolves before the lifecycle
/// abandons the stop future and leaves the task detached.
const STOP_GRACE: Duration = Duration::from_secs(4);

/// The budget one pass shares. [`DRAIN_DEADLINE`] is a FLOOR, not the value: a send
/// budget wider than the pass budget would make the "no room for a full attempt" guard
/// below true on the very first row of every pass, so the drain would claim nothing,
/// forever, while every pass still reported healthy.
pub(crate) fn pass_budget(send_timeout: Duration) -> Duration {
    DRAIN_DEADLINE.max(send_timeout.saturating_add(ACQUIRE_DEADLINE))
}

/// `/readyz` flags the drain when no fully-healthy pass completed for this long. DERIVED
/// from [`pass_budget`] (2x): one budget-length wedge errors, and the next pass gets a
/// full window to recover before readiness flips.
pub(crate) fn stall_max(send_timeout: Duration) -> Duration {
    pass_budget(send_timeout).saturating_mul(2)
}

const BACKOFF_MIN_SECS: f64 = 1.0;

/// The backoff ceiling, and with it the longest gap the retry ladder ever waits. Also the
/// authority for [`crate::config::MAX_SEND_TIMEOUT_MS`]: an attempt allowed to run longer
/// than the longest gap between attempts inverts the ladder, and `MAIL_MAX_ATTEMPTS` stops
/// bounding anything useful.
pub(crate) const BACKOFF_MAX_SECS: u64 = 300;

/// Exponential backoff, 1s doubling per burnt attempt, capped at 5m — the shape of
/// `core/asyncevents/src/worker.rs`'s. The `clamp` and `saturating_pow` are overflow
/// guards, not style: `attempts` comes from a column an operator can edit.
pub(crate) fn backoff_secs(attempts: i32) -> f64 {
    let exp = (attempts - 1).clamp(0, 30) as u32;
    (BACKOFF_MIN_SECS * f64::from(2u32.saturating_pow(exp))).min(BACKOFF_MAX_SECS as f64)
}

/// How far out a claim pushes the row it just took. 3x the send budget, which is what
/// [`write_budget`] then divides up so the whole send-plus-status-write fits inside it.
pub(crate) fn claim_lease(send_timeout: Duration) -> Duration {
    send_timeout.saturating_mul(3)
}

/// The `statement_timeout` for a status write. DERIVED from the lease, not from what is
/// left of the pass: a write cut short by an exhausted pass would lose an attempt's
/// outcome, and a write that runs past the lease lets another replica re-claim a row this
/// pass already sent. What remains of the lease after the send's own budget and one
/// checkout is exactly the room the write may take; [`ACQUIRE_DEADLINE`] is the floor, so
/// a send budget under it trades the lease guarantee for a usable write window.
pub(crate) fn write_budget(send_timeout: Duration) -> Duration {
    claim_lease(send_timeout)
        .saturating_sub(send_timeout)
        .saturating_sub(ACQUIRE_DEADLINE)
        .max(ACQUIRE_DEADLINE)
}

/// The row write one attempt implies. Pure and total over the send taxonomy, so every
/// branch — including "the last attempt failed transiently, park it" — is provable with
/// no relay and no DB.
pub(crate) fn disposition(
    result: Result<(), SendError>,
    attempts: i32,
    max_attempts: i32,
) -> Disposition {
    match result {
        Ok(()) => Disposition::Sent,
        Err(e @ SendError::Rejected(_)) => Disposition::Parked {
            last_error: truncate_error(&format!("{e:#}")),
        },
        Err(e @ SendError::Infra(_)) => {
            let last_error = truncate_error(&format!("{e:#}"));
            if attempts >= max_attempts {
                Disposition::Parked { last_error }
            } else {
                Disposition::Retry {
                    last_error,
                    backoff_secs: backoff_secs(attempts),
                }
            }
        }
    }
}

// ============================================================================
// Metrics. A live-but-ineffective loop never flips `Liveness::dead`, so these counters
// are how a drain that claims and fails forever becomes visible beside the stamp.
// ============================================================================

pub(crate) fn send_attempts() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "mail_send_attempts_total",
            "Outbox rows claimed and handed to the configured provider.",
        )
        .expect("valid mail send_attempts counter");
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

pub(crate) fn send_errors() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "mail_send_errors_total",
            "Send attempts the provider refused or could not complete (parked or backed off).",
        )
        .expect("valid mail send_errors counter");
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

pub(crate) fn send_cas_misses() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "mail_send_cas_misses_total",
            "Status writes that matched no row because the outbox row left 'pending' or was \
             re-claimed while the send was in flight (an operator cancel, or an expired lease).",
        )
        .expect("valid mail send_cas_misses counter");
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

fn parked_gauge() -> &'static IntGauge {
    static G: OnceLock<IntGauge> = OnceLock::new();
    G.get_or_init(|| {
        let g = IntGauge::new(
            "mail_outbox_parked",
            "Outbox rows in the 'parked' state, awaiting an operator.",
        )
        .expect("valid mail outbox_parked gauge");
        let _ = metrics::register(Box::new(g.clone()));
        g
    })
}

fn oldest_pending_gauge() -> &'static Gauge {
    static G: OnceLock<Gauge> = OnceLock::new();
    G.get_or_init(|| {
        let g = Gauge::new(
            "mail_outbox_oldest_pending_age_seconds",
            "How long the earliest pending outbox row has been past its next_attempt_at \
             (0 = nothing is due).",
        )
        .expect("valid mail outbox_oldest_pending gauge");
        let _ = metrics::register(Box::new(g.clone()));
        g
    })
}

// ============================================================================
// Loop liveness — the `"mail"` /readyz probe when a provider IS configured (the
// unconfigured process contributes a permanently-failing check under the same name).
// ============================================================================

/// Coarse monotonic seconds since the first call in this process. Deliberately not
/// wall-clock: a clock jump must not flap `/readyz`, and a test must not race a real
/// clock. Same shape as `scheduler`'s and `asyncevents`', private to each owner. It reads
/// TOKIO's clock, which outside a paused runtime IS the std monotonic clock — so the
/// failure-half proof advances past [`stall_max`] instead of waiting out 60 real seconds.
fn coarse_now_secs() -> u64 {
    static BASE: OnceLock<tokio::time::Instant> = OnceLock::new();
    BASE.get_or_init(tokio::time::Instant::now).elapsed().as_secs()
}

/// Pure staleness predicate behind [`Liveness::check`]. `last_ok_secs == 0` means the loop
/// never seeded the stamp (no provider, or `start` not reached) — never a stall; a
/// controlled stop is not a stall either.
pub(crate) fn stalled_from(
    last_ok_secs: u64,
    now_secs: u64,
    stopping: bool,
    max_age: Duration,
) -> bool {
    !stopping && last_ok_secs != 0 && now_secs.saturating_sub(last_ok_secs) > max_age.as_secs()
}

#[derive(Clone, Default)]
pub(crate) struct Liveness {
    dead: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    /// Coarse-clock second of the last fully-healthy pass; `0` = never seeded.
    last_ok_secs: Arc<AtomicU64>,
}

impl Liveness {
    pub(crate) fn check(&self, stall_max: Duration) -> Result<(), String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err("mail drain loop task died".to_string());
        }
        let last = self.last_ok_secs.load(Ordering::SeqCst);
        let stopping = self.stopping.load(Ordering::SeqCst);
        if stalled_from(last, coarse_now_secs(), stopping, stall_max) {
            return Err(format!("no healthy mail drain pass in >{}s", stall_max.as_secs()));
        }
        Ok(())
    }

    /// Stamps "fully-healthy pass completed now". Seeded at loop entry — HTTP serves
    /// before the first pass on a cold boot, so the age must start at 0, not at infinity.
    /// `max(1)` because `0` is the never-seeded sentinel.
    fn mark_pass_ok(&self) {
        self.last_ok_secs
            .store(coarse_now_secs().max(1), Ordering::SeqCst);
    }

    pub(crate) fn set_stopping(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
}

// ============================================================================
// The pass.
// ============================================================================

/// Everything a pass needs, built once in `start`.
pub(crate) struct Drain {
    pub(crate) pool: PgPool,
    pub(crate) sender: Arc<dyn Sender>,
    pub(crate) from: String,
    pub(crate) send_timeout: Duration,
    pub(crate) max_attempts: i32,
}

/// Begins a pool-owned transaction under `SET LOCAL statement_timeout`, so a wedged
/// statement ERRORS instead of stalling the loop and the bound reverts at commit — a bare
/// `SET` would leak into the next borrower of this pooled connection. The checkout itself
/// is bounded by [`ACQUIRE_DEADLINE`]: dropping a PENDING checkout carries no session
/// state, so cancelling it is safe.
pub(crate) async fn bounded_tx(
    pool: &PgPool,
    budget: Duration,
) -> anyhow::Result<Transaction<'static, Postgres>> {
    let mut tx = tokio::time::timeout(ACQUIRE_DEADLINE, pool.begin())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "mail: pool checkout timed out after {}s",
                ACQUIRE_DEADLINE.as_secs()
            )
        })??;
    // `SET` takes no bind parameters; the value is a locally computed integer (ms).
    // Clamped to Postgres's int32 ms domain: an out-of-range value is refused by the
    // SERVER, which would make every pass fail before it claimed a row — the drain silent
    // for a reason that has nothing to do with mail.
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = {}",
        budget.as_millis().clamp(1, i32::MAX as u128)
    ))
    .execute(&mut *tx)
    .await?;
    Ok(tx)
}

/// One send attempt under the SOLE bound on the dialogue. An SMTP delivery is
/// multi-round-trip, and lettre's tokio transport bounds only the TCP connect (see
/// [`crate::smtp`]) — everything after it is unbounded, so this aggregate deadline is what
/// keeps a stalled relay from holding a claimed row past its lease. An elapsed budget is
/// infrastructure, never a rejection — the message may well be deliverable.
pub(crate) async fn attempt(drain: &Drain, row: &Claimed) -> Result<(), SendError> {
    let outgoing = Outgoing {
        from: &drain.from,
        to: &row.recipient,
        subject: &row.subject,
        body: &row.body,
        kind: &row.kind,
    };
    match tokio::time::timeout(drain.send_timeout, drain.sender.send(&outgoing)).await {
        Ok(result) => result,
        Err(_) => Err(SendError::Infra(anyhow::anyhow!(
            "send exceeded {}ms",
            drain.send_timeout.as_millis()
        ))),
    }
}

/// One drain pass. `Err` is INFRASTRUCTURE only (a checkout, claim, status write or gauge
/// read failed) — that is what withholds the [`Liveness`] stamp. A refused or undeliverable
/// message is not a pass failure: it is per-row state the backoff/park machinery and the
/// counters own, and flipping `/readyz` red for one bad recipient would take the whole
/// channel down. Budget exhaustion is not a failure either — it is a backlog, which the
/// two gauges report.
pub(crate) async fn drain_pass(
    drain: &Drain,
    store: &Store,
    stop: &watch::Receiver<bool>,
    pass_deadline: Instant,
) -> anyhow::Result<()> {
    let lease_secs = claim_lease(drain.send_timeout).as_secs_f64();
    for _ in 0..DRAIN_BATCH {
        if *stop.borrow() {
            break;
        }
        let remaining = pass_deadline.saturating_duration_since(Instant::now());
        // Never claim a row this pass cannot fully attempt: the claim burns the row's
        // attempt whether or not a send follows it.
        if remaining < drain.send_timeout {
            tracing::info!(
                budget_secs = pass_budget(drain.send_timeout).as_secs(),
                "mail: drain budget exhausted; remaining due rows wait for the next pass"
            );
            break;
        }
        let mut tx = bounded_tx(&drain.pool, remaining).await?;
        let claimed = store.claim_due_tx(&mut tx, lease_secs).await?;
        tx.commit().await?;
        let Some(row) = claimed else {
            break;
        };

        send_attempts().inc();
        let result = attempt(drain, &row).await;
        if let Err(e) = &result {
            send_errors().inc();
            tracing::warn!(mail_id = %row.id, kind = %row.kind, error = %e, "mail: send failed");
        }
        let disposition = disposition(result, row.attempts, drain.max_attempts);

        let mut tx = bounded_tx(&drain.pool, write_budget(drain.send_timeout)).await?;
        let matched = store
            .finish_tx(&mut tx, &row, &disposition, drain.sender.name())
            .await?;
        tx.commit().await?;
        if matched == 0 {
            send_cas_misses().inc();
            tracing::warn!(
                mail_id = %row.id,
                "mail: status write matched no row — it left 'pending' or was re-claimed \
                 while the send was in flight; the attempt's outcome is dropped"
            );
        }
    }

    let mut tx = bounded_tx(&drain.pool, ACQUIRE_DEADLINE).await?;
    let gauges = store.gauges_tx(&mut tx).await?;
    tx.commit().await?;
    parked_gauge().set(gauges.parked);
    oldest_pending_gauge().set(gauges.oldest_pending_overdue_secs);
    Ok(())
}

/// Drains every [`DRAIN_INTERVAL`] until `stop` flips. The `select!` races ONLY the stop
/// signal against the ticker; the pass itself runs outside it and re-checks the signal
/// between rows, so a graceful stop lands at a row boundary with the in-flight row's
/// status already written. `stop` aborts a loop still running past the grace, which can
/// drop a send future mid-dialogue — safe: the claim is already committed, so the row is
/// due again when its lease expires.
pub(crate) async fn run_loop(
    drain: Drain,
    store: Store,
    liveness: Liveness,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
) {
    // Seed the staleness clock: HTTP serves before the first pass on a cold boot.
    liveness.mark_pass_ok();
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = ticker.tick() => {}
        }
        if *stop.borrow() {
            break;
        }
        let pass_deadline = Instant::now() + pass_budget(drain.send_timeout);
        match drain_pass(&drain, &store, &stop, pass_deadline).await {
            Ok(()) => liveness.mark_pass_ok(),
            Err(e) => tracing::error!(error = %e, "mail drain pass failed"),
        }
    }
}

/// Spawns the loop under a supervision wrapper: a panic inside a pass, or the loop
/// exiting while the module is running, flips [`Liveness::dead`] so `/readyz` goes red
/// instead of the drain dying silently.
pub(crate) fn spawn(
    drain: Drain,
    store: Store,
    liveness: Liveness,
    stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    use futures::FutureExt;
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(run_loop(
            drain,
            store,
            liveness.clone(),
            DRAIN_INTERVAL,
            stop,
        ))
        .catch_unwind()
        .await;
        if !liveness.stopping.load(Ordering::SeqCst) {
            if result.is_err() {
                tracing::error!("mail drain loop panicked while the module was running");
            } else {
                tracing::error!("mail drain loop exited while the module was running");
            }
            liveness.dead.store(true, Ordering::SeqCst);
        }
    })
}

/// Signals the loop and awaits its exit, bounded by [`STOP_GRACE`], then ABORTS and
/// AWAITS the aborted handle — the await is what lets the task's connection drop complete
/// rather than detaching it.
pub(crate) async fn stop_tasks(
    stop_tx: Option<watch::Sender<bool>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
) {
    if let Some(tx) = stop_tx {
        let _ = tx.send(true);
    }
    for mut t in tasks {
        match tokio::time::timeout(STOP_GRACE, &mut t).await {
            Ok(Ok(())) => {}
            // The supervision wrapper catches pass panics, so a JoinError here means the
            // wrapper itself died — never swallow it silently.
            Ok(Err(e)) => {
                tracing::error!(error = %e, "mail drain loop task terminated abnormally");
            }
            Err(_) => {
                tracing::error!(
                    grace_secs = STOP_GRACE.as_secs(),
                    "mail drain loop did not exit within the stop grace; aborting it — a send \
                     in flight loses its status write and the row is due again once its \
                     lease expires (delivery is at-least-once)"
                );
                t.abort();
                match t.await {
                    Ok(()) => {}
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => tracing::error!(
                        error = %e,
                        "mail aborted drain loop task terminated abnormally"
                    ),
                }
            }
        }
    }
}

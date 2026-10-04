//! The lease sweep: a module-owned `start()` loop (the `mail` drain precedent) rather than a
//! scheduler fire, because a seconds-scale cadence would append a durable `scheduler.fired`
//! every few seconds.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::Service;

pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Parties one pass selects; the rest are picked up on the next tick.
pub(crate) const SWEEP_BATCH: i64 = 64;

/// Bounds a pool checkout. Dropping a PENDING checkout carries no session state, so
/// cancelling it is safe.
pub(crate) const SWEEP_ACQUIRE_DEADLINE: Duration = Duration::from_secs(2);

/// `SET LOCAL statement_timeout` on every sweep transaction: bounds each statement,
/// including the wait for a party's advisory lock, in the database rather than by
/// cancelling a future that holds the transaction.
pub(crate) const SWEEP_STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounds the post-commit handle lookup — a network call holding no database state.
pub(crate) const SWEEP_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// No new party is started once a pass has run this long; the rest wait for the next tick.
pub(crate) const SWEEP_PASS_BUDGET: Duration = Duration::from_secs(20);

/// Statements one party's sweep transaction can issue: the timeout `SET`, the lock, the
/// expired DELETE, the leader pick, `nextval`, the party UPDATE or the two dissolution
/// DELETEs, one `player_revisions` upsert per removed member, the snapshot, and the commit.
const PARTY_TX_STATEMENTS_MAX: u64 = 9 + partyapi::MAX_MEMBERS as u64;

/// One party's worst case: checkout, every statement at its timeout, then the lookup.
const PARTY_WORST_SECS: u64 = SWEEP_ACQUIRE_DEADLINE.as_secs()
    + PARTY_TX_STATEMENTS_MAX * SWEEP_STATEMENT_TIMEOUT.as_secs()
    + SWEEP_LOOKUP_TIMEOUT.as_secs();

/// The pass's two bounded single-statement transactions (expired-party pick, expired-invite
/// delete): each a checkout, the timeout `SET`, the statement and the commit.
const SETUP_WORST_SECS: u64 =
    2 * (SWEEP_ACQUIRE_DEADLINE.as_secs() + 3 * SWEEP_STATEMENT_TIMEOUT.as_secs());

/// A pass's worst case: the budget is checked between parties, so the last party started
/// inside it may run its full worst case past it.
const PASS_WORST_SECS: u64 = SETUP_WORST_SECS + SWEEP_PASS_BUDGET.as_secs() + PARTY_WORST_SECS;

/// `/readyz` turns red past this age of the last fully healthy pass: the longest gap between
/// two healthy stamps is one tick plus one worst-case pass.
pub(crate) const STALL_MAX: Duration =
    Duration::from_secs(SWEEP_INTERVAL.as_secs() + PASS_WORST_SECS);

/// Sweep health for the `"party"` readiness check.
#[derive(Clone)]
pub(crate) struct Liveness {
    dead: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    /// Coarse-clock second of the last fully healthy pass; `0` = never seeded.
    last_ok_secs: Arc<AtomicU64>,
    base: Arc<tokio::time::Instant>,
}

impl Default for Liveness {
    fn default() -> Self {
        Liveness {
            dead: Arc::default(),
            stopping: Arc::default(),
            last_ok_secs: Arc::default(),
            base: Arc::new(tokio::time::Instant::now()),
        }
    }
}

impl Liveness {
    fn now_secs(&self) -> u64 {
        self.base.elapsed().as_secs()
    }

    pub(crate) fn check(&self, stall_max: Duration) -> Result<(), String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err("party sweep loop task died".to_string());
        }
        let last = self.last_ok_secs.load(Ordering::SeqCst);
        let stopping = self.stopping.load(Ordering::SeqCst);
        if !stopping && last != 0 && self.now_secs().saturating_sub(last) > stall_max.as_secs() {
            return Err(format!(
                "no healthy party sweep pass in >{}s",
                stall_max.as_secs()
            ));
        }
        Ok(())
    }

    /// `max(1)` because `0` is the never-seeded sentinel.
    pub(crate) fn mark_pass_ok(&self) {
        self.last_ok_secs
            .store(self.now_secs().max(1), Ordering::SeqCst);
    }

    pub(crate) fn set_stopping(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
}

/// One pass: parties with an expired member (re-checked under each party's lock), then
/// expired invites. A failing party does not stop the pass; the pass reports that it failed,
/// so the healthy stamp is withheld.
pub(crate) async fn sweep_pass(svc: &Service, stop: &watch::Receiver<bool>) -> anyhow::Result<()> {
    let started = tokio::time::Instant::now();
    let parties = svc.store.expired_parties(SWEEP_BATCH).await?;
    let mut failed = 0usize;
    for party_id in &parties {
        if *stop.borrow() || started.elapsed() >= SWEEP_PASS_BUDGET {
            break;
        }
        if let Err(e) = svc.sweep_party(party_id).await {
            failed += 1;
            tracing::warn!(error = %e, party_id, "party: lease sweep of one party failed");
        }
    }
    svc.store.delete_expired_invites().await?;
    if failed > 0 {
        anyhow::bail!("{failed} of {} parties failed to sweep", parties.len());
    }
    Ok(())
}

pub(crate) async fn run_loop(
    svc: Arc<Service>,
    liveness: Liveness,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = ticker.tick() => {}
        }
        if *stop.borrow() {
            break;
        }
        match sweep_pass(&svc, &stop).await {
            Ok(()) => liveness.mark_pass_ok(),
            Err(e) => tracing::error!(error = %e, "party lease sweep pass failed"),
        }
    }
}

/// Spawns the loop under a supervision wrapper: a panicking pass, or the loop exiting while
/// the module is running, flips the liveness to dead so `/readyz` goes red.
pub(crate) fn spawn(
    svc: Arc<Service>,
    liveness: Liveness,
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    use futures::FutureExt;
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(run_loop(
            svc,
            liveness.clone(),
            SWEEP_INTERVAL,
            stop,
        ))
        .catch_unwind()
        .await;
        if !liveness.stopping.load(Ordering::SeqCst) {
            if result.is_err() {
                tracing::error!("party sweep loop panicked while the module was running");
            } else {
                tracing::error!("party sweep loop exited while the module was running");
            }
            liveness.dead.store(true, Ordering::SeqCst);
        }
    })
}

/// Signals the loop and awaits its exit, bounded by [`STOP_GRACE`], then aborts and awaits
/// the aborted handle so its connection is dropped rather than detached.
pub(crate) async fn stop_task(stop_tx: Option<watch::Sender<bool>>, task: Option<JoinHandle<()>>) {
    if let Some(tx) = stop_tx {
        let _ = tx.send(true);
    }
    let Some(mut task) = task else {
        return;
    };
    match tokio::time::timeout(STOP_GRACE, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!(error = %e, "party sweep task terminated abnormally"),
        Err(_) => {
            tracing::error!(
                grace_secs = STOP_GRACE.as_secs(),
                "party sweep did not exit within the stop grace; aborting it"
            );
            task.abort();
            match task.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => tracing::error!(error = %e, "party aborted sweep task terminated abnormally"),
            }
        }
    }
}

/// Under `core/app`'s 5s `MODULE_STOP_GRACE_MS`, so the module resolves its own stop before
/// the lifecycle abandons it.
const STOP_GRACE: Duration = Duration::from_secs(4);

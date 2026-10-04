//! The lease sweep: a module-owned `start()` loop (the `mail` drain precedent) rather than a
//! scheduler fire, because a seconds-scale cadence would append a durable `scheduler.fired`
//! every few seconds.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::Service;

pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Parties one pass visits; the rest are picked up on the next tick.
pub(crate) const SWEEP_BATCH: i64 = 64;

/// Under `core/app`'s 5s `MODULE_STOP_GRACE_MS`, so the module resolves its own stop before
/// the lifecycle abandons it.
const STOP_GRACE: Duration = Duration::from_secs(4);

/// One pass: every party with an expired member (re-checked under its lock), then expired
/// invites. A failing party does not stop the pass; the pass reports that it failed.
pub(crate) async fn sweep_pass(svc: &Service, stop: &watch::Receiver<bool>) -> anyhow::Result<()> {
    let parties = svc.store.expired_parties(SWEEP_BATCH).await?;
    let mut failed = 0usize;
    for party_id in &parties {
        if *stop.borrow() {
            return Ok(());
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

pub(crate) async fn run_loop(svc: Arc<Service>, interval: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = ticker.tick() => {}
        }
        if *stop.borrow() {
            break;
        }
        if let Err(e) = sweep_pass(&svc, &stop).await {
            tracing::error!(error = %e, "party lease sweep pass failed");
        }
    }
}

pub(crate) fn spawn(svc: Arc<Service>, stop: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(run_loop(svc, SWEEP_INTERVAL, stop))
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

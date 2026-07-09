//! Worker wake-up: one dedicated `PgListener` per process on the
//! `asyncevents_events` channel (fired by the log's `AFTER INSERT` trigger)
//! wakes every idle worker. NOTIFY is best-effort — the workers' global 1s poll
//! (in [`crate::worker::run`]) is the lost-NOTIFY floor, so this loop never has
//! to be perfect, only prompt. Never dies on a DB outage: each (re)connect backs
//! off on failure.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Notify};

const NOTIFY_CHANNEL: &str = "asyncevents_events";

pub(crate) async fn listen(dsn: String, wakeup: Arc<Notify>, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        match sqlx::postgres::PgListener::connect(&dsn).await {
            Ok(mut listener) => match listener.listen(NOTIFY_CHANNEL).await {
                Ok(()) => {
                    // Events may have committed between worker start and this
                    // LISTEN; kick once so they aren't stranded until the poll.
                    wakeup.notify_waiters();
                    loop {
                        tokio::select! {
                            _ = stop.changed() => return,
                            res = listener.recv() => match res {
                                Ok(_) => wakeup.notify_waiters(),
                                Err(err) => {
                                    tracing::error!(%err, "asyncevents wake-up listener wait failed");
                                    break; // reconnect via the outer loop
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "asyncevents wake-up LISTEN failed");
                }
            },
            Err(err) => {
                tracing::error!(%err, "asyncevents wake-up listener connect failed");
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

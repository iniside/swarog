//! The invalidation plane's observability series. Owned per-plane (raw prometheus, not
//! the `core/metrics` crate — see the crate's Cargo.toml for the cycle this avoids);
//! `app::run` registers [`Gauges::collectors`] into the process's private registry so
//! they ride the same `GET /metrics` scrape. Two series, both labeled `{callback}`:
//! - `invalidation_last_success_age_seconds` — seconds since the callback last refreshed
//!   successfully (a periodic 10s loop recomputes it from [`Health`]),
//! - `invalidation_refresh_failures_total` — failures, incremented on the refresh path.

use std::time::Duration;

use prometheus::{GaugeVec, IntCounterVec, Opts};
use tokio::sync::watch;

use crate::Health;

/// The plane's Prometheus series. `Clone` is cheap (both vecs are `Arc`-backed): the
/// plane keeps clones for updates while `app::run` registers [`collectors`](Self::collectors).
/// Not a process-global static — exactly one plane per process (a DB-backed one) is
/// registered, so no duplicate-registration guard is needed.
#[derive(Clone)]
pub(crate) struct Gauges {
    last_success_age: GaugeVec,
    failures: IntCounterVec,
}

impl Gauges {
    pub(crate) fn new() -> Gauges {
        let last_success_age = GaugeVec::new(
            Opts::new(
                "invalidation_last_success_age_seconds",
                "Seconds since this invalidation callback last refreshed successfully.",
            ),
            &["callback"],
        )
        .expect("valid last_success_age gauge");
        let failures = IntCounterVec::new(
            Opts::new(
                "invalidation_refresh_failures_total",
                "Total invalidation refresh failures, labeled by callback.",
            ),
            &["callback"],
        )
        .expect("valid failures counter");
        Gauges {
            last_success_age,
            failures,
        }
    }

    /// The collectors `app::run` hands to `core/metrics::register` (once per process).
    pub(crate) fn collectors(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![
            Box::new(self.last_success_age.clone()),
            Box::new(self.failures.clone()),
        ]
    }

    pub(crate) fn inc_failure(&self, name: &str) {
        self.failures.with_label_values(&[name]).inc();
    }
}

/// The 10s age-gauge refresh loop, one task per plane. Recomputes each callback's
/// last-success age from [`Health`] so the delivery paths never touch a gauge.
pub(crate) async fn refresh_loop(gauges: Gauges, health: Health, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => {
                for (name, age) in health.ages() {
                    gauges.last_success_age.with_label_values(&[&name]).set(age);
                }
            }
        }
    }
}

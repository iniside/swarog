//! The plane's observability series, registered into the process's private
//! Prometheus registry (`core/metrics::register`) so they ride the same
//! `GET /metrics` scrape as the HTTP collectors. A periodic refresh task
//! (10s) recomputes them from the shared tables — delivery hot paths never
//! touch a gauge.
//!
//! Series (per the plan's Step-3 list):
//! - `asyncevents_subscription_lag_events{subscription}` — events past the cursor,
//! - `asyncevents_subscription_lag_age_seconds{subscription}` — age of the oldest,
//! - `asyncevents_subscription_consecutive_failures{subscription}`,
//! - `asyncevents_subscriptions_paused` — paused count among LOCAL subscriptions,
//! - `asyncevents_subscription_paused_state{subscription}` — 1 if this subscription
//!   is currently paused, 0 otherwise. Deliberately NOT named
//!   `asyncevents_subscription_paused` — that differs from the existing unlabeled
//!   count gauge `asyncevents_subscriptions_paused` by a single letter ("paused"
//!   vs "paused_state"), which would be a dashboard/alert-rule footgun if the two
//!   were one letter apart instead of a clearly distinct name,
//! - `asyncevents_safe_frontier_age_seconds` — how long the oldest not-yet-eligible
//!   current-generation event has been waiting on the snapshot frontier (a
//!   long-running foreign transaction delays delivery; this is the alarm for it).

use std::sync::OnceLock;
use std::time::Duration;

use prometheus::{Gauge, GaugeVec, IntGauge, IntGaugeVec, Opts};
use sqlx::{PgPool, Row};
use tokio::sync::watch;

struct Gauges {
    lag_events: IntGaugeVec,
    lag_age: GaugeVec,
    failures: IntGaugeVec,
    paused: IntGauge,
    paused_state: IntGaugeVec,
    frontier_age: Gauge,
}

fn gauges() -> &'static Gauges {
    static G: OnceLock<Gauges> = OnceLock::new();
    G.get_or_init(|| {
        let lag_events = IntGaugeVec::new(
            Opts::new(
                "asyncevents_subscription_lag_events",
                "Eligible-or-not events past this subscription's cursor.",
            ),
            &["subscription"],
        )
        .expect("valid lag_events gauge");
        let lag_age = GaugeVec::new(
            Opts::new(
                "asyncevents_subscription_lag_age_seconds",
                "Age of the oldest event past this subscription's cursor.",
            ),
            &["subscription"],
        )
        .expect("valid lag_age gauge");
        let failures = IntGaugeVec::new(
            Opts::new(
                "asyncevents_subscription_consecutive_failures",
                "Consecutive delivery failures (pauses at the threshold).",
            ),
            &["subscription"],
        )
        .expect("valid failures gauge");
        let paused = IntGauge::new(
            "asyncevents_subscriptions_paused",
            "Locally-hosted subscriptions in state 'paused'.",
        )
        .expect("valid paused gauge");
        let paused_state = IntGaugeVec::new(
            Opts::new(
                "asyncevents_subscription_paused_state",
                "1 if this subscription is currently paused, 0 otherwise.",
            ),
            &["subscription"],
        )
        .expect("valid paused_state gauge");
        let frontier_age = Gauge::new(
            "asyncevents_safe_frontier_age_seconds",
            "Age of the oldest current-generation event still behind the snapshot frontier.",
        )
        .expect("valid frontier_age gauge");
        // One registration per process; a second Plane in the same process (tests)
        // reuses the statics, so a duplicate-register error cannot occur here.
        let _ = metrics::register(Box::new(lag_events.clone()));
        let _ = metrics::register(Box::new(lag_age.clone()));
        let _ = metrics::register(Box::new(failures.clone()));
        let _ = metrics::register(Box::new(paused.clone()));
        let _ = metrics::register(Box::new(paused_state.clone()));
        let _ = metrics::register(Box::new(frontier_age.clone()));
        Gauges {
            lag_events,
            lag_age,
            failures,
            paused,
            paused_state,
            frontier_age,
        }
    })
}

/// Test-only accessor for the per-subscription paused-state gauge, so
/// `worker_tests` can assert the labeled value directly instead of scraping
/// text output.
#[cfg(test)]
pub(crate) fn paused_state_gauge() -> &'static IntGaugeVec {
    &gauges().paused_state
}

pub(crate) async fn refresh(pool: &PgPool, local_ids: &[String]) -> anyhow::Result<()> {
    let g = gauges();

    let rows = sqlx::query(
        "SELECT s.subscription_id, s.state, s.consecutive_failures, \
                count(e.event_id) AS lag, \
                COALESCE(extract(epoch FROM now() - min(e.created_at)), 0)::float8 AS lag_age \
         FROM asyncevents.subscriptions s \
         LEFT JOIN asyncevents.events e \
           ON e.topic = s.topic AND e.contract_version = s.contract_version \
          AND (e.generation, e.producer_xid, e.tie_breaker) \
              > (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
         WHERE s.subscription_id = ANY($1) \
         GROUP BY s.subscription_id, s.state, s.consecutive_failures",
    )
    .bind(local_ids)
    .fetch_all(pool)
    .await?;
    let mut paused = 0i64;
    for row in rows {
        let id: String = row.get("subscription_id");
        let state: String = row.get("state");
        let failures: i32 = row.get("consecutive_failures");
        let lag: i64 = row.get("lag");
        let lag_age: f64 = row.get("lag_age");
        if state == "paused" {
            paused += 1;
        }
        g.lag_events.with_label_values(&[&id]).set(lag);
        g.lag_age.with_label_values(&[&id]).set(lag_age);
        g.failures.with_label_values(&[&id]).set(i64::from(failures));
        g.paused_state
            .with_label_values(&[&id])
            .set(i64::from(state == "paused"));
    }
    g.paused.set(paused);

    let frontier_age: f64 = sqlx::query_scalar(
        "SELECT COALESCE(extract(epoch FROM now() - min(e.created_at)), 0)::float8 \
         FROM asyncevents.events e, asyncevents.plane_meta m \
         WHERE m.singleton AND e.generation = m.generation \
           AND e.producer_xid >= pg_snapshot_xmin(pg_current_snapshot())",
    )
    .fetch_one(pool)
    .await?;
    g.frontier_age.set(frontier_age);
    Ok(())
}

/// The 10s refresh loop, one task per plane.
pub(crate) async fn refresh_loop(pool: PgPool, local_ids: Vec<String>, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => {
                if let Err(err) = refresh(&pool, &local_ids).await {
                    tracing::warn!(%err, "asyncevents metrics refresh failed");
                }
            }
        }
    }
}

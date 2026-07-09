//! Checkpoint-coupled retention GC for the shared event log. One task per process
//! (spawned by [`crate::Plane::start`], stopped by [`crate::Plane::stop`]) sweeps on
//! an interval and, per `(topic, contract_version)` that carries a
//! `history_contracts` row, deletes only events that are BOTH strictly below every
//! subscription's checkpoint AND older than the topic's `min_retention_days`.
//!
//! Normative rules (docs/plans/2026-07-09-2234-durable-event-log-fresh-plan.md):
//! - Floor = the MINIMUM cursor position over the topic's ACTIVE **and PAUSED**
//!   subscriptions (row-compare on `(cursor_generation, cursor_xid, cursor_tie)`).
//!   A never-run `Genesis` subscription's `(0, '0', 0)` cursor pins EVERYTHING.
//!   A paused subscription therefore blocks GC — surfaced via
//!   [`asyncevents_retention_blocked_age_seconds`].
//! - A topic with a `keep_forever` policy is never deleted from.
//! - **Conservative GC:** a topic with NO `history_contracts` row is never deleted
//!   from — an unknown retention promise is treated as "keep".
//! - The `min_retention_days` bound is a `created_at` predicate. There is no index
//!   on `created_at`; the resulting seq scan is ACCEPTED at this project's scale.
//! - Deletes are batch-bounded (`ctid IN (… LIMIT {BATCH})`) so a sweep never holds
//!   a long lock; the day/generation bounds ride as bound params, never interpolated.

use std::sync::OnceLock;
use std::time::Duration;

use prometheus::Gauge;
use sqlx::{PgPool, Row};
use tokio::sync::watch;

/// Bounds each retention DELETE so a sweep never takes a long lock.
const BATCH: i64 = 1000;

/// Default sweep interval when `EVENTS_HOUSEKEEP_INTERVAL` is unset (1h).
const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

/// The paused-subscription-blocks-GC alarm (see the module docs). Registered once
/// per process into `core/metrics`'s private registry, like [`crate::plane_metrics`].
fn blocked_gauge() -> &'static Gauge {
    static G: OnceLock<Gauge> = OnceLock::new();
    G.get_or_init(|| {
        let g = Gauge::new(
            "asyncevents_retention_blocked_age_seconds",
            "Age of the oldest GC-eligible event a PAUSED subscription is holding \
             back past its topic's min_retention window (0 = nothing blocked).",
        )
        .expect("valid retention_blocked gauge");
        // OnceLock guards the single registration; a second Plane in one process
        // (tests) reuses the static and never re-registers.
        let _ = metrics::register(Box::new(g.clone()));
        g
    })
}

/// `EVENTS_HOUSEKEEP_INTERVAL`: a Go-style duration (`1h`/`30m`/`45s`/`500ms`) or a
/// bare seconds integer; unset/unparseable falls back to [`DEFAULT_INTERVAL`].
pub(crate) fn interval_from_env() -> Duration {
    match std::env::var("EVENTS_HOUSEKEEP_INTERVAL") {
        Ok(v) => parse_go_duration(&v).unwrap_or(DEFAULT_INTERVAL),
        Err(_) => DEFAULT_INTERVAL,
    }
}

/// Single-unit Go-duration parser (`h`/`m`/`s`/`ms`, or a bare seconds integer).
/// Restored from the pre-cutover housekeeping helper; the plane's other knob
/// ([`crate::worker`]) has its own copy without `h` (a delivery timeout in hours is
/// nonsensical).
fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        return n.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(n) = s.strip_suffix('h') {
        return n.trim().parse::<u64>().ok().map(|h| Duration::from_secs(h * 3600));
    }
    if let Some(n) = s.strip_suffix('m') {
        return n.trim().parse::<u64>().ok().map(|m| Duration::from_secs(m * 60));
    }
    if let Some(n) = s.strip_suffix('s') {
        return n.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    s.parse::<u64>().ok().map(Duration::from_secs)
}

/// The retention task: sweep on a ticker until stopped. The first sweep lands one
/// interval in (the immediate tick is consumed), so boot is never blocked on GC.
pub(crate) async fn run(pool: PgPool, interval: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => {
                if let Err(err) = sweep(&pool).await {
                    tracing::error!(%err, "asyncevents retention sweep failed");
                }
            }
        }
    }
}

/// One retention pass: GC every `min_retention` topic to its checkpoint floor, then
/// refresh the paused-blocker gauge. `keep_forever` topics and topics with no
/// `history_contracts` row are skipped entirely (conservative GC).
pub(crate) async fn sweep(pool: &PgPool) -> anyhow::Result<()> {
    let contracts = sqlx::query(
        "SELECT topic, contract_version, min_retention_days \
         FROM asyncevents.history_contracts WHERE policy = 'min_retention'",
    )
    .fetch_all(pool)
    .await?;

    for row in &contracts {
        let topic: String = row.get("topic");
        let version: i32 = row.get("contract_version");
        let days: i32 = row.get("min_retention_days");
        if let Err(err) = gc_topic(pool, &topic, version, days).await {
            tracing::error!(%topic, version, %err, "asyncevents retention: topic GC failed");
        }
    }

    refresh_blocked_gauge(pool).await?;
    Ok(())
}

/// Deletes retained-past-policy events for one `(topic, version)` in bounded
/// batches: strictly below the checkpoint floor (MIN cursor over active+paused
/// subscriptions) AND older than `min_retention_days`. A topic with no active/paused
/// subscription has no floor, so only the day bound applies.
async fn gc_topic(pool: &PgPool, topic: &str, version: i32, days: i32) -> anyhow::Result<()> {
    // Floor = the lexicographically smallest active/paused cursor. Postgres cannot
    // MIN a composite, so ORDER BY the row and take the first.
    let floor = sqlx::query(
        "SELECT cursor_generation, cursor_xid::text AS cursor_xid, cursor_tie \
         FROM asyncevents.subscriptions \
         WHERE topic = $1 AND contract_version = $2 AND state IN ('active','paused') \
         ORDER BY cursor_generation, cursor_xid, cursor_tie \
         LIMIT 1",
    )
    .bind(topic)
    .bind(version)
    .fetch_optional(pool)
    .await?;

    loop {
        let deleted = match &floor {
            Some(f) => {
                let fg: i64 = f.get("cursor_generation");
                let fx: String = f.get("cursor_xid");
                let ft: i64 = f.get("cursor_tie");
                sqlx::query(
                    "DELETE FROM asyncevents.events WHERE ctid IN ( \
                       SELECT ctid FROM asyncevents.events \
                       WHERE topic = $1 AND contract_version = $2 \
                         AND created_at < now() - make_interval(days => $3) \
                         AND (generation, producer_xid, tie_breaker) < ($4, $5::xid8, $6) \
                       LIMIT $7)",
                )
                .bind(topic)
                .bind(version)
                .bind(days)
                .bind(fg)
                .bind(&fx)
                .bind(ft)
                .bind(BATCH)
                .execute(pool)
                .await?
                .rows_affected()
            }
            // No active/paused subscription pins this topic: only the day bound
            // applies (MinRetention promises `days`, nothing more).
            None => sqlx::query(
                "DELETE FROM asyncevents.events WHERE ctid IN ( \
                   SELECT ctid FROM asyncevents.events \
                   WHERE topic = $1 AND contract_version = $2 \
                     AND created_at < now() - make_interval(days => $3) \
                   LIMIT $4)",
            )
            .bind(topic)
            .bind(version)
            .bind(days)
            .bind(BATCH)
            .execute(pool)
            .await?
            .rows_affected(),
        };
        if deleted < BATCH as u64 {
            return Ok(());
        }
    }
}

/// Sets [`asyncevents_retention_blocked_age_seconds`] to the age of the oldest
/// event that a PAUSED subscription (of a `min_retention` topic) is holding back
/// past its retention window — the exact events GC would otherwise remove. 0 when
/// nothing is blocked.
async fn refresh_blocked_gauge(pool: &PgPool) -> anyhow::Result<()> {
    let age: f64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(EXTRACT(epoch FROM now() - blocked.oldest)), 0)::float8 \
         FROM ( \
           SELECT MIN(e.created_at) AS oldest \
           FROM asyncevents.subscriptions s \
           JOIN asyncevents.history_contracts h \
             ON h.topic = s.topic AND h.contract_version = s.contract_version \
            AND h.policy = 'min_retention' \
           JOIN asyncevents.events e \
             ON e.topic = s.topic AND e.contract_version = s.contract_version \
            AND (e.generation, e.producer_xid, e.tie_breaker) \
                > (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
            AND e.created_at < now() - make_interval(days => h.min_retention_days) \
           WHERE s.state = 'paused' \
           GROUP BY s.subscription_id \
         ) blocked",
    )
    .fetch_one(pool)
    .await?;
    blocked_gauge().set(age);
    Ok(())
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod retention_tests;

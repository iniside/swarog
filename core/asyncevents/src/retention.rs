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
//!   The same posture covers the correlated `subscriptions` anti-join in
//!   [`gc_topic`]: the subscriptions table is small (one row per subscription),
//!   so no supporting index is warranted.
//! - Deletes are batch-bounded (`ctid IN (… LIMIT {BATCH})`) so a sweep never holds
//!   a long lock; the day bound rides as a bound param and the checkpoint floor is a
//!   correlated `NOT EXISTS` subquery over typed cursor columns — never interpolated,
//!   and recomputed per DELETE statement (see [`gc_topic`]) so a subscription that
//!   registers mid-sweep is honored from the next batch.

use std::sync::OnceLock;
use std::time::Duration;

use prometheus::{Gauge, IntCounter};
use sqlx::{PgPool, Row};
use tokio::sync::watch;

use crate::Liveness;

/// Bounds each retention DELETE so a sweep never takes a long lock.
const BATCH: i64 = 1000;

/// Default sweep interval when `EVENTS_HOUSEKEEP_INTERVAL` is unset (1h).
const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) interval: Duration,
    pub(crate) stall_after: Duration,
}

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

/// Counts retention sweep failures (a top-level sweep query error or a per-topic
/// GC error) while the plane runs. A live-but-ineffective GC task never flips
/// [`crate::Liveness::retention_dead`], so this counter (plus
/// [`crate::Liveness::retention_stalled`] on `/readyz`) is how persistent
/// ineffectiveness surfaces. Registered once per process into `core/metrics`'s
/// private registry, like [`blocked_gauge`].
pub(crate) fn sweep_errors() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "asyncevents_retention_sweep_errors_total",
            "Times a retention sweep failed (top-level query error or a per-topic \
             GC error) while the plane was running (the log may grow unbounded).",
        )
        .expect("valid retention sweep_errors counter");
        // OnceLock guards the single registration; a second Plane in one process
        // (tests) reuses the static and never re-registers.
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

/// Parse the retention interval once while constructing the plane and derive its
/// readiness threshold from the same authoritative value. The env syntax is a
/// single Go-style unit (`1h`/`30m`/`45s`/`500ms`) or bare seconds; unset uses
/// [`DEFAULT_INTERVAL`]. Malformed values retain the historical default fallback;
/// zero, overflowing, and clock-unobservable thresholds fail startup.
impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Config> {
        Self::from_var_result(std::env::var("EVENTS_HOUSEKEEP_INTERVAL"))
    }

    fn from_var_result(value: Result<String, std::env::VarError>) -> anyhow::Result<Config> {
        match value {
            Ok(value) => Self::from_value(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::from_value(None),
            // Present-but-non-Unicode is malformed, not absent. The historical
            // policy for malformed input is the default fallback.
            Err(std::env::VarError::NotUnicode(_)) => Self::from_value(Some("")),
        }
    }

    pub(crate) fn from_value(value: Option<&str>) -> anyhow::Result<Config> {
        let interval = match value {
            None => DEFAULT_INTERVAL,
            Some(value) => match parse_go_duration(value) {
                Ok(Some(interval)) => interval,
                Ok(None) => DEFAULT_INTERVAL,
                Err(()) => anyhow::bail!(
                    "EVENTS_HOUSEKEEP_INTERVAL={value:?} overflows its duration unit"
                ),
            },
        };
        if interval.is_zero() {
            anyhow::bail!(
                "EVENTS_HOUSEKEEP_INTERVAL={value:?} parses to a zero interval; set a \
                 positive duration or unset it (default {DEFAULT_INTERVAL:?})"
            );
        }
        let stall_after = interval.checked_mul(3).ok_or_else(|| {
            anyhow::anyhow!(
                "EVENTS_HOUSEKEEP_INTERVAL={value:?} is too large: deriving the 3x \
                 retention staleness threshold overflowed"
            )
        })?;
        if stall_after.as_millis() >= u128::from(u64::MAX - 1) {
            anyhow::bail!(
                "EVENTS_HOUSEKEEP_INTERVAL={value:?} is too large: its 3x retention \
                 staleness threshold must be less than u64::MAX - 1 milliseconds \
                 so the capped liveness clock can exceed it"
            );
        }
        Ok(Config { interval, stall_after })
    }
}

/// Test-only panic injection: when set, the retention task panics immediately on
/// entry (before its first tick), so the plane's per-task supervision
/// (`Liveness::retention_dead` → the `asyncevents-retention` readyz check) is
/// testable without waiting out an interval. `#[cfg(test)]`-gated — never
/// compiled into a shipping binary.
#[cfg(test)]
pub(crate) static RETENTION_PANIC_ONCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Single-unit Go-duration parser (`h`/`m`/`s`/`ms`, or a bare seconds integer).
/// Restored from the pre-cutover housekeeping helper; the plane's other knob
/// ([`crate::worker`]) has its own copy without `h` (a delivery timeout in hours is
/// nonsensical).
fn parse_number(s: &str) -> Result<Option<u64>, ()> {
    match s.trim().parse::<u64>() {
        Ok(value) => Ok(Some(value)),
        Err(err)
            if matches!(
                err.kind(),
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
            ) =>
        {
            Err(())
        }
        Err(_) => Ok(None),
    }
}

fn parse_go_duration(s: &str) -> Result<Option<Duration>, ()> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        return Ok(parse_number(n)?.map(Duration::from_millis));
    }
    if let Some(n) = s.strip_suffix('h') {
        return parse_number(n)?
            .map(|hours| hours.checked_mul(3600).map(Duration::from_secs).ok_or(()))
            .transpose();
    }
    if let Some(n) = s.strip_suffix('m') {
        return parse_number(n)?
            .map(|minutes| minutes.checked_mul(60).map(Duration::from_secs).ok_or(()))
            .transpose();
    }
    if let Some(n) = s.strip_suffix('s') {
        return Ok(parse_number(n)?.map(Duration::from_secs));
    }
    Ok(parse_number(s)?.map(Duration::from_secs))
}

/// The retention task: sweep on a ticker until stopped. The first sweep lands one
/// interval in (the immediate tick is consumed), so boot is never blocked on GC.
pub(crate) async fn run(
    pool: PgPool,
    interval: Duration,
    liveness: Liveness,
    mut stop: watch::Receiver<bool>,
) {
    #[cfg(test)]
    if RETENTION_PANIC_ONCE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        panic!("test-injected retention panic (RETENTION_PANIC_ONCE)");
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => {
                match sweep(&pool).await {
                    // A healthy pass advances the staleness clock; the readyz
                    // check reads it via `Liveness::retention_stalled`.
                    Ok(()) => liveness.mark_retention_ok(),
                    Err(err) => {
                        sweep_errors().inc();
                        tracing::error!(%err, "asyncevents retention sweep failed");
                    }
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
         FROM asyncevents.history_contracts WHERE policy = 'min_retention' \
         ORDER BY topic, contract_version",
    )
    .fetch_all(pool)
    .await?;

    let mut failures = Vec::new();
    for row in &contracts {
        let topic: String = row.get("topic");
        let version: i32 = row.get("contract_version");
        let days: i32 = row.get("min_retention_days");
        if let Err(err) = gc_topic(pool, &topic, version, days).await {
            failures.push(format!("topic {topic:?} version {version}: {err:#}"));
        }
    }

    if let Err(err) = refresh_blocked_gauge(pool).await {
        failures.push(format!("blocked-age gauge refresh: {err:#}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("retention pass failed: {}", failures.join("; "))
    }
}

/// Deletes retained-past-policy events for one `(topic, version)` in bounded
/// batches: strictly below the checkpoint floor (MIN cursor over active+paused
/// subscriptions) AND older than `min_retention_days`. A topic with no active/paused
/// subscription has no floor, so only the day bound applies.
///
/// The floor is recomputed PER DELETE STATEMENT: the correlated `NOT EXISTS`
/// subquery carries the full floor predicate, so it is re-evaluated against the
/// live `subscriptions` table on every batch. This is the fix for a mid-sweep
/// stale-floor race: the old code fetched the floor ONCE before the batch loop, so
/// a subscription registering between batches (with a cursor below the previously
/// computed floor) could lose events at/above its brand-new cursor. Folding the
/// floor into the DELETE closes that inter-batch window entirely.
///
/// A residual window remains and is DELIBERATE: within a single DELETE statement,
/// under READ COMMITTED, the correlated subquery reads one snapshot, so a
/// subscription registering DURING that one statement's execution is not observed.
/// This is acceptable — the `MinRetention` contract promises only `days` of
/// retention (see [`crate::HistoryPolicy`]); a subscription must be registered
/// before the events it needs age past `days`. The audit's proposed
/// catalog↔GC lock (serializing subscription registration against GC) was REJECTED
/// as contract-overreach: it would buy a guarantee the contract never made
/// (docs/plans/2026-07-13-1415-audit-remediation-plan.md, Step 18 / rejected C2).
///
/// The `state IN ('active','paused')` filter inside the subquery is MANDATORY: it
/// is the inverse of the stale-floor defect. Without it, a `retired`/`completed`
/// subscription's low cursor would satisfy `NOT EXISTS` and pin GC forever. The
/// composite comparison rides TYPED columns (never a text-aliased `xid8`), so
/// numeric xid8 order governs — the module's `floor_uses_numeric_xid_order_not_text`
/// regression is the precedent. Cursors are `NOT NULL`, so there is no NULL seam.
async fn gc_topic(pool: &PgPool, topic: &str, version: i32, days: i32) -> anyhow::Result<()> {
    loop {
        // ORDER BY before LIMIT: each batch deletes floor-UPWARD in log order,
        // deterministically. Without it batch composition is a planner accident —
        // an unordered batch could reap HIGH positions that a subscription
        // registering between batches (the exact race the NOT EXISTS fold closes)
        // still wanted; the fresh floor only protects events earlier batches have
        // not already taken. Ordered batches make "everything at/above a mid-sweep
        // cursor survives" a guarantee, not a plan-dependent coincidence.
        let deleted = sqlx::query(
            "DELETE FROM asyncevents.events WHERE ctid IN ( \
               SELECT e.ctid FROM asyncevents.events e \
               WHERE e.topic = $1 AND e.contract_version = $2 \
                 AND e.created_at < now() - make_interval(days => $3) \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM asyncevents.subscriptions s \
                   WHERE s.topic = $1 AND s.contract_version = $2 \
                     AND s.state IN ('active','paused') \
                     AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
                         <= (e.generation, e.producer_xid, e.tie_breaker)) \
               ORDER BY e.generation, e.producer_xid, e.tie_breaker \
               LIMIT $4)",
        )
        .bind(topic)
        .bind(version)
        .bind(days)
        .bind(BATCH)
        .execute(pool)
        .await?
        .rows_affected();
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

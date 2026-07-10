//! `eventctl` operator logic over the shared `asyncevents` schema. The mutating
//! verbs live here (not in `main.rs`) so they are unit-testable against live
//! Postgres; `main.rs` is a thin arg dispatcher. Every mutating command returns a
//! before/after [`StateSnapshot`] pair so the CLI can PRINT the transition — a
//! checkpoint is never advanced silently.

use anyhow::{anyhow, bail, Context, Result};
use sqlx::{PgPool, Row};

/// Dev-default DSN (mirrors CLAUDE.md); overridden by `DATABASE_URL`.
pub const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The connection string every command uses: `DATABASE_URL` or the dev default.
pub fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

/// A subscription projection for `list`/`lag`: identity, delivery state, cursor, and
/// the event-count + oldest-age lag past that cursor.
pub struct SubInfo {
    pub id: String,
    pub topic: String,
    pub version: i32,
    pub state: String,
    pub cursor: String,
    pub consecutive_failures: i32,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub lag_events: i64,
    pub lag_age_seconds: f64,
}

/// The before/after view a mutating command prints. Text-only (cursor/next_attempt
/// rendered as strings) so the CLI never needs to name an xid8/timestamp codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSnapshot {
    pub id: String,
    pub state: String,
    pub cursor: String,
    pub consecutive_failures: i32,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
}

impl StateSnapshot {
    /// One-line rendering for the before/after print.
    pub fn describe(&self) -> String {
        format!(
            "state={} cursor={} failures={} next_attempt={} last_error={}",
            self.state,
            self.cursor,
            self.consecutive_failures,
            self.next_attempt_at.as_deref().unwrap_or("-"),
            self.last_error.as_deref().unwrap_or("-"),
        )
    }
}

/// The outcome of [`skip`]: the state transition plus the exact event that was
/// stepped over (id + payload), so the caller can log both to stderr.
#[derive(Debug)]
pub struct SkipOutcome {
    pub before: StateSnapshot,
    pub after: StateSnapshot,
    pub skipped_event_id: String,
    pub skipped_payload: String,
}

/// Every subscription with its lag (events + oldest age past the cursor). Powers both
/// `list` and `lag`.
pub async fn info(pool: &PgPool) -> Result<Vec<SubInfo>> {
    let rows = sqlx::query(
        "SELECT s.subscription_id, s.topic, s.contract_version, s.state, \
                s.cursor_generation, s.cursor_xid::text AS cursor_xid, s.cursor_tie, \
                s.consecutive_failures, s.last_error, s.next_attempt_at::text AS next_attempt_at, \
                count(e.event_id) AS lag_events, \
                COALESCE(extract(epoch FROM now() - min(e.created_at)), 0)::float8 AS lag_age \
         FROM asyncevents.subscriptions s \
         LEFT JOIN asyncevents.events e \
           ON e.topic = s.topic AND e.contract_version = s.contract_version \
          AND (e.generation, e.producer_xid, e.tie_breaker) \
              > (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
         GROUP BY s.subscription_id, s.topic, s.contract_version, s.state, \
                  s.cursor_generation, s.cursor_xid, s.cursor_tie, \
                  s.consecutive_failures, s.last_error, s.next_attempt_at \
         ORDER BY s.subscription_id",
    )
    .fetch_all(pool)
    .await
    .context("eventctl: query subscriptions")?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let g: i64 = r.get("cursor_generation");
            let x: String = r.get("cursor_xid");
            let t: i64 = r.get("cursor_tie");
            SubInfo {
                id: r.get("subscription_id"),
                topic: r.get("topic"),
                version: r.get("contract_version"),
                state: r.get("state"),
                cursor: format!("{g}/{x}/{t}"),
                consecutive_failures: r.get("consecutive_failures"),
                next_attempt_at: r.get("next_attempt_at"),
                last_error: r.get("last_error"),
                lag_events: r.get("lag_events"),
                lag_age_seconds: r.get("lag_age"),
            }
        })
        .collect())
}

/// A before/after snapshot of one subscription (its identity, state, cursor, failure
/// state). Errors if the id is unknown.
pub async fn snapshot(pool: &PgPool, id: &str) -> Result<StateSnapshot> {
    let row = sqlx::query(
        "SELECT subscription_id, state, cursor_generation, cursor_xid::text AS cursor_xid, \
                cursor_tie, consecutive_failures, next_attempt_at::text AS next_attempt_at, last_error \
         FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("eventctl: read subscription")?
    .ok_or_else(|| anyhow!("eventctl: no such subscription {id:?}"))?;
    let g: i64 = row.get("cursor_generation");
    let x: String = row.get("cursor_xid");
    let t: i64 = row.get("cursor_tie");
    Ok(StateSnapshot {
        id: row.get("subscription_id"),
        state: row.get("state"),
        cursor: format!("{g}/{x}/{t}"),
        consecutive_failures: row.get("consecutive_failures"),
        next_attempt_at: row.get("next_attempt_at"),
        last_error: row.get("last_error"),
    })
}

/// `retry`: clear the backoff window and failure count so the next worker pass
/// re-attempts the CURRENT event — the cursor is untouched (no skip).
pub async fn retry(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let before = snapshot(pool, id).await?;
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET consecutive_failures = 0, next_attempt_at = NULL, last_error = NULL, updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("eventctl: retry update")?;
    Ok((before, snapshot(pool, id).await?))
}

/// `pause`: stop delivery (the worker's due-select ignores non-`active` rows).
pub async fn pause(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    set_state(pool, id, "paused").await
}

/// `resume`: return to `active` and clear the backoff window so delivery restarts
/// promptly. The failure count is left for `retry` to reset — resume/pause are the
/// state toggle, retry is the backoff reset.
pub async fn resume(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let before = snapshot(pool, id).await?;
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET state = 'active', next_attempt_at = NULL, updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("eventctl: resume update")?;
    Ok((before, snapshot(pool, id).await?))
}

/// `retire`: mark the subscription retired (an explicit operator decision — deleting
/// code is NOT retirement). The worker never delivers a retired row, and retention
/// excludes it from the GC floor.
pub async fn retire(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    set_state(pool, id, "retired").await
}

async fn set_state(pool: &PgPool, id: &str, state: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let before = snapshot(pool, id).await?;
    sqlx::query(
        "UPDATE asyncevents.subscriptions SET state = $2, updated_at = now() WHERE subscription_id = $1",
    )
    .bind(id)
    .bind(state)
    .execute(pool)
    .await
    .context("eventctl: state update")?;
    Ok((before, snapshot(pool, id).await?))
}

/// `skip`: advance the cursor PAST the current failing event ONLY — the next eligible
/// event at or after the cursor — clear the failure/backoff and reactivate, recording
/// `reason` in `last_error`. Refuses a healthy subscription (`state = 'active'` AND
/// `consecutive_failures = 0`): skip is a poison-recovery verb, not a fast-forward.
/// Never advances more than one event.
pub async fn skip(pool: &PgPool, id: &str, reason: &str) -> Result<SkipOutcome> {
    let before = snapshot(pool, id).await?;
    if before.state == "active" && before.consecutive_failures == 0 {
        bail!(
            "eventctl: refusing to skip {id:?} — it is active with no failures; skip only steps \
             past the CURRENT failing event of a paused/faulted subscription (pause it first, or \
             use retry)"
        );
    }

    // The failing event = the next event the worker would attempt: exact
    // (topic, contract_version) match, position past the cursor, frontier-eligible
    // (current-generation rows gated by the snapshot xmin, older generations fully
    // eligible) — the same selection the worker uses.
    let sub = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        "SELECT topic, contract_version, cursor_generation, cursor_xid::text AS cursor_xid_text, cursor_tie \
         FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("eventctl: read subscription for skip")?;
    let topic: String = sub.get("topic");
    let version: i32 = sub.get("contract_version");
    let cg: i64 = sub.get("cursor_generation");
    let cx: String = sub.get("cursor_xid_text");
    let ct: i64 = sub.get("cursor_tie");

    let ev = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        "SELECT event_id, generation, producer_xid::text AS producer_xid_text, tie_breaker, payload::text AS payload_text \
         FROM asyncevents.events \
         WHERE topic = $1 AND contract_version = $2 \
           AND (generation, producer_xid, tie_breaker) > ($3, $4::xid8, $5) \
           AND (generation < (SELECT generation FROM asyncevents.plane_meta WHERE singleton) \
                OR producer_xid < pg_snapshot_xmin(pg_current_snapshot())) \
         ORDER BY generation, producer_xid, tie_breaker \
         LIMIT 1",
    )
    .bind(&topic)
    .bind(version)
    .bind(cg)
    .bind(&cx)
    .bind(ct)
    .fetch_optional(pool)
    .await
    .context("eventctl: select failing event")?
    .ok_or_else(|| {
        anyhow!("eventctl: no eligible event past {id:?}'s cursor to skip — nothing to do")
    })?;
    let event_id: String = ev.get("event_id");
    let eg: i64 = ev.get("generation");
    let ex: String = ev.get("producer_xid_text");
    let et: i64 = ev.get("tie_breaker");
    let payload: String = ev.get("payload_text");

    let note = format!("skipped event {event_id}: {reason}");
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET cursor_generation = $2, cursor_xid = $3::xid8, cursor_tie = $4, \
             consecutive_failures = 0, next_attempt_at = NULL, last_error = $5, \
             state = 'active', updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(id)
    .bind(eg)
    .bind(&ex)
    .bind(et)
    .bind(&note)
    .execute(pool)
    .await
    .context("eventctl: advance cursor past the skipped event")?;

    Ok(SkipOutcome {
        after: snapshot(pool, id).await?,
        before,
        skipped_event_id: event_id,
        skipped_payload: payload,
    })
}

/// `bump-generation`: fence the current log generation (offline operator action; see
/// [`asyncevents::store::bump_generation`]). Returns `(before, after)` generations.
pub async fn bump_generation(pool: &PgPool) -> Result<(i64, i64)> {
    let before: i64 =
        sqlx::query_scalar("SELECT generation FROM asyncevents.plane_meta WHERE singleton")
            .fetch_one(pool)
            .await
            .context("eventctl: read current generation")?;
    let after = asyncevents::store::bump_generation(pool)
        .await
        .context("eventctl: bump generation")?;
    Ok((before, after))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;

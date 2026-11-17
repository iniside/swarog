//! Operator operations over the plane's subscriptions and log. Every mutating
//! command returns a before/after [`StateSnapshot`] pair so the caller can PRINT
//! the transition — a checkpoint is never advanced silently.

use anyhow::{anyhow, bail, Context, Result};
use sqlx::{PgPool, Row};

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
    /// `updated_at` (as text) iff `state = 'paused'`, else `None`. Note:
    /// `updated_at` moves on ANY row update (retry, resume, skip, worker
    /// checkpoint advance), so for a paused row it reads as "last
    /// state/failure write", not strictly "when it entered `paused`" — exact
    /// enough for an operator, since a pause stops further writes to the row.
    pub paused_since: Option<String>,
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

/// What [`skip`] did. `AlreadyResolved` arises only when the caller named the event it
/// expects to skip and that event is no longer the one blocking the subscription.
#[derive(Debug)]
pub enum SkipResult {
    Skipped(Box<SkipOutcome>),
    /// Nothing moved. `healthy` is true when the subscription is active with no failures.
    AlreadyResolved { healthy: bool },
}

/// Every subscription with its lag (events + oldest age past the cursor). Powers both
/// `list` and `lag`. `paused_since` is derived from `updated_at` — see the field doc
/// on [`SubInfo::paused_since`] for the exact semantics.
pub async fn info(pool: &PgPool) -> Result<Vec<SubInfo>> {
    let rows = sqlx::query(
        "SELECT s.subscription_id, s.topic, s.contract_version, s.state, \
                s.cursor_generation, s.cursor_xid::text AS cursor_xid, s.cursor_tie, \
                s.consecutive_failures, s.last_error, s.next_attempt_at::text AS next_attempt_at, \
                CASE WHEN s.state = 'paused' THEN s.updated_at::text END AS paused_since, \
                count(e.event_id) AS lag_events, \
                COALESCE(extract(epoch FROM now() - min(e.created_at)), 0)::float8 AS lag_age \
         FROM asyncevents.subscriptions s \
         LEFT JOIN asyncevents.events e \
           ON e.topic = s.topic AND e.contract_version = s.contract_version \
          AND (e.generation, e.producer_xid, e.tie_breaker) \
              > (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
         GROUP BY s.subscription_id, s.topic, s.contract_version, s.state, \
                  s.cursor_generation, s.cursor_xid, s.cursor_tie, \
                  s.consecutive_failures, s.last_error, s.next_attempt_at, s.updated_at \
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
                paused_since: r.get("paused_since"),
            }
        })
        .collect())
}

/// One subscription's delivery state, without the lag [`info`] counts.
pub struct SubState {
    pub id: String,
    pub state: String,
    pub consecutive_failures: i32,
    /// As [`SubInfo::paused_since`].
    pub paused_since: Option<String>,
}

/// Every subscription's delivery state, by id. Reads only the subscription rows.
pub async fn states(pool: &PgPool) -> Result<Vec<SubState>> {
    let rows = sqlx::query(
        "SELECT subscription_id, state, consecutive_failures, \
                CASE WHEN state = 'paused' THEN updated_at::text END AS paused_since \
         FROM asyncevents.subscriptions ORDER BY subscription_id",
    )
    .fetch_all(pool)
    .await
    .context("asyncevents: read subscription states")?;
    Ok(rows
        .into_iter()
        .map(|r| SubState {
            id: r.get("subscription_id"),
            state: r.get("state"),
            consecutive_failures: r.get("consecutive_failures"),
            paused_since: r.get("paused_since"),
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

/// The error a verb returns when it refuses to move a retired subscription; callers
/// find it with `anyhow::Error::downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub id: String,
}

impl std::fmt::Display for Retired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "subscription {:?} is retired", self.id)
    }
}

impl std::error::Error for Retired {}

/// Applies `set` (a static SET list; `$1` binds the id) to a subscription that is not
/// retired, returning the before/after pair. The refusal is the UPDATE's own
/// `state <> 'retired'` predicate, so a `retire` landing after `before` is never undone.
async fn update_unretired(
    pool: &PgPool,
    id: &str,
    set: &'static str,
    what: &'static str,
) -> Result<(StateSnapshot, StateSnapshot)> {
    let before = snapshot(pool, id).await?;
    let sql = format!(
        "UPDATE asyncevents.subscriptions SET {set}, updated_at = now() \
         WHERE subscription_id = $1 AND state <> 'retired'"
    );
    let moved = sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .with_context(|| format!("eventctl: {what} update"))?
        .rows_affected();
    if moved == 0 {
        return Err(refusal(pool, id).await);
    }
    Ok((before, snapshot(pool, id).await?))
}

/// Why a guarded UPDATE matched no row: [`Retired`], or the snapshot's unknown-id error.
async fn refusal(pool: &PgPool, id: &str) -> anyhow::Error {
    match snapshot(pool, id).await {
        Ok(now) if now.state == "retired" => Retired { id: id.to_string() }.into(),
        Ok(now) => anyhow!("eventctl: update of {id:?} matched no row in state {}", now.state),
        Err(error) => error,
    }
}

/// `retry`: clear the backoff window and failure count so the next worker pass
/// re-attempts the CURRENT event — the cursor is untouched (no skip). [`Retired`] for
/// a retired subscription.
pub async fn retry(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let set = "consecutive_failures = 0, next_attempt_at = NULL, last_error = NULL";
    update_unretired(pool, id, set, "retry").await
}

/// `pause`: stop delivery (the worker's due-select ignores non-`active` rows).
/// [`Retired`] for a retired subscription.
pub async fn pause(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    update_unretired(pool, id, "state = 'paused'", "pause").await
}

/// `resume`: return to `active` and clear the backoff window so delivery restarts
/// promptly. The failure count is left for `retry` to reset — resume/pause are the
/// state toggle, retry is the backoff reset. [`Retired`] for a retired subscription.
pub async fn resume(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    update_unretired(pool, id, "state = 'active', next_attempt_at = NULL", "resume").await
}

/// `retire`: mark the subscription retired (an explicit operator decision — deleting
/// code is NOT retirement). The worker never delivers a retired row, and retention
/// excludes it from the GC floor. Permanent: retry, pause, resume, resume_retry and skip
/// refuse a retired row with [`Retired`].
pub async fn retire(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let before = snapshot(pool, id).await?;
    sqlx::query(
        "UPDATE asyncevents.subscriptions SET state = 'retired', updated_at = now() WHERE subscription_id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("eventctl: retire update")?;
    Ok((before, snapshot(pool, id).await?))
}

/// The selection `skip` steps past and `poison_event` reports, so both name the same
/// event. Binds `$1` topic, `$2` contract version, `$3..$5` the cursor position.
macro_rules! next_eligible_event {
    () => {
        "FROM asyncevents.events \
         WHERE topic = $1 AND contract_version = $2 \
           AND (generation, producer_xid, tie_breaker) > ($3, $4::xid8, $5) \
           AND (generation < (SELECT generation FROM asyncevents.plane_meta WHERE singleton) \
                OR producer_xid < pg_snapshot_xmin(pg_current_snapshot())) \
         ORDER BY generation, producer_xid, tie_breaker \
         LIMIT 1"
    };
}

/// `skip`: advance the cursor PAST the current failing event ONLY — the next eligible
/// event at or after the cursor — clear the failure/backoff and reactivate, recording
/// `reason` in `last_error`. Refuses a healthy subscription (`state = 'active'` AND
/// `consecutive_failures = 0`): skip is a poison-recovery verb, not a fast-forward.
/// Never advances more than one event.
///
/// With `expected = Some(event_id)`, a healthy subscription, no eligible event, or a
/// next eligible event other than `event_id` returns [`SkipResult::AlreadyResolved`]
/// and moves nothing; with `None` the first two are errors.
///
/// The read-pick-update runs in ONE transaction that claims the subscription row
/// `FOR UPDATE` — the same row workers serialize their delivery/checkpoint on — so
/// skip never races a live worker's cursor advance. Plain `FOR UPDATE` (not
/// `SKIP LOCKED`): the operator WAITS for an in-flight delivery to finish rather than
/// silently no-oping. If a worker advanced past the failure first, the guard re-checks
/// on the locked row and finds `consecutive_failures = 0`, so skip REFUSES instead of
/// rewinding the checkpoint — the correct outcome. A retired subscription, read on the
/// locked row, is a [`Retired`] error whatever `expected` is.
pub async fn skip(pool: &PgPool, id: &str, reason: &str, expected: Option<&str>) -> Result<SkipResult> {
    // Cosmetic before/after reads stay pool-side; the real work is the tx below.
    let before = snapshot(pool, id).await?;

    let mut tx = pool.begin().await.context("eventctl: begin skip tx")?;

    // Claim the subscription row FOR UPDATE (worker's exact shape) so the guard and
    // cursor advance see — and hold — a consistent, worker-serialized view.
    let sub = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        "SELECT topic, contract_version, cursor_generation, cursor_xid::text AS cursor_xid_text, \
                cursor_tie, state, consecutive_failures \
         FROM asyncevents.subscriptions WHERE subscription_id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .context("eventctl: claim subscription for skip")?
    .ok_or_else(|| anyhow!("eventctl: no such subscription {id:?}"))?;
    let topic: String = sub.get("topic");
    let version: i32 = sub.get("contract_version");
    let cg: i64 = sub.get("cursor_generation");
    let cx: String = sub.get("cursor_xid_text");
    let ct: i64 = sub.get("cursor_tie");
    let state: String = sub.get("state");
    let failures: i32 = sub.get("consecutive_failures");

    if state == "retired" {
        tx.rollback().await.context("eventctl: rollback skip of a retired subscription")?;
        return Err(Retired { id: id.to_string() }.into());
    }

    // Re-evaluate the refuse-healthy guard on the LOCKED row: a worker that advanced
    // past the failure (resetting failures to 0) between our `before` read and the lock
    // makes this fire — skip refuses rather than rewinding a healthy checkpoint.
    if state == "active" && failures == 0 {
        tx.rollback().await.context("eventctl: rollback skip guard")?;
        if expected.is_some() {
            return Ok(SkipResult::AlreadyResolved { healthy: true });
        }
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
    let ev = sqlx::query(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column.
        concat!(
            "SELECT event_id, generation, producer_xid::text AS producer_xid_text, tie_breaker, payload::text AS payload_text ",
            next_eligible_event!()
        ),
    )
    .bind(&topic)
    .bind(version)
    .bind(cg)
    .bind(&cx)
    .bind(ct)
    .fetch_optional(&mut *tx)
    .await
    .context("eventctl: select failing event")?;
    let ev = match (ev, expected) {
        (Some(ev), Some(want)) if ev.get::<String, _>("event_id") != want => None,
        (ev, _) => ev,
    };
    let Some(ev) = ev else {
        tx.rollback().await.context("eventctl: rollback skip with no event")?;
        if expected.is_some() {
            return Ok(SkipResult::AlreadyResolved { healthy: false });
        }
        bail!("eventctl: no eligible event past {id:?}'s cursor to skip — nothing to do");
    };
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
    .execute(&mut *tx)
    .await
    .context("eventctl: advance cursor past the skipped event")?;

    tx.commit().await.context("eventctl: commit skip tx")?;

    Ok(SkipResult::Skipped(Box::new(SkipOutcome {
        after: snapshot(pool, id).await?,
        before,
        skipped_event_id: event_id,
        skipped_payload: payload,
    })))
}

/// `bump-generation`: fence the current log generation (offline operator action; see
/// [`crate::store::bump_generation`]). Returns `(before, after)` generations.
pub async fn bump_generation(pool: &PgPool) -> Result<(i64, i64)> {
    let before: i64 =
        sqlx::query_scalar("SELECT generation FROM asyncevents.plane_meta WHERE singleton")
            .fetch_one(pool)
            .await
            .context("eventctl: read current generation")?;
    let after = crate::store::bump_generation(pool)
        .await
        .context("eventctl: bump generation")?;
    Ok((before, after))
}

/// The event a worker would attempt next on a subscription — the one [`skip`] would
/// step over.
#[derive(Debug, Clone)]
pub struct PoisonEvent {
    pub event_id: String,
    pub topic: String,
    pub version: i32,
    pub payload: serde_json::Value,
    pub appended_at: String,
}

/// The next eligible event past subscription `id`'s cursor (the one [`skip`] steps
/// over), read without moving anything. It does not apply `skip`'s refuse-healthy
/// guard, so on a healthy subscription it is just the next event due. `None` when no
/// event past the cursor is eligible. Errors if the id is unknown.
pub async fn poison_event(pool: &PgPool, id: &str) -> Result<Option<PoisonEvent>> {
    let sub = sqlx::query(
        "SELECT topic, contract_version, cursor_generation, cursor_xid::text AS cursor_xid_text, \
                cursor_tie \
         FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("asyncevents: read subscription for poison event")?
    .ok_or_else(|| anyhow!("asyncevents: no such subscription {id:?}"))?;
    let topic: String = sub.get("topic");
    let version: i32 = sub.get("contract_version");
    let cg: i64 = sub.get("cursor_generation");
    let cx: String = sub.get("cursor_xid_text");
    let ct: i64 = sub.get("cursor_tie");

    let ev = sqlx::query(concat!(
        "SELECT event_id, topic, contract_version, payload, created_at::text AS appended_at ",
        next_eligible_event!()
    ))
    .bind(&topic)
    .bind(version)
    .bind(cg)
    .bind(&cx)
    .bind(ct)
    .fetch_optional(pool)
    .await
    .context("asyncevents: select poison event")?;
    Ok(ev.map(|r| PoisonEvent {
        event_id: r.get("event_id"),
        topic: r.get("topic"),
        version: r.get("contract_version"),
        payload: r.get("payload"),
        appended_at: r.get("appended_at"),
    }))
}

/// [`resume`] plus the [`retry`] reset: also clears the failure count and last error,
/// which `resume` deliberately leaves. [`Retired`] for a retired subscription.
pub async fn resume_retry(pool: &PgPool, id: &str) -> Result<(StateSnapshot, StateSnapshot)> {
    let set = "state = 'active', consecutive_failures = 0, next_attempt_at = NULL, last_error = NULL";
    update_unretired(pool, id, set, "resume-retry").await
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod admin_tests;

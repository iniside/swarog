use std::sync::Arc;

use bus::{AnyTx, Bus, Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

/// The retention sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. `Genesis` because the sweep must run on a boot that has never
/// subscribed before, the same reasoning as `notifications.prune-on-scheduler.v1`.
pub(crate) const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "groups.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::GROUPS_PRUNE;

pub(crate) const DEFAULT_RETENTION_DAYS: i32 = 30;

pub(crate) const RETENTION_ENV: &str = "GROUPS_RETENTION_DAYS";

/// Rows per statement. The sweep LOOPS batches until a short one — a per-fire CAP would
/// leave retention permanently behind any inflow above one batch per day, since this
/// schedule fires once every 86400s — and each batch resumes from the previous batch's
/// highest `created_at`. That watermark is load-bearing, not an optimisation: every batch
/// runs inside the ONE still-open delivery transaction, where the tuples this transaction
/// already deleted are neither killable nor prunable from the index, so a watermark-less
/// scan re-walks all `256 x (k-1)` of them and the loop goes quadratic.
pub(crate) const PRUNE_BATCH: i64 = 256;

/// The whole sweep's wall-clock budget. Exhausting it ends the fire with the batches so far
/// KEPT — they commit with the checkpoint, and the next fire resumes from there.
pub(crate) const PRUNE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Ten years: far past any group policy, and far inside the timestamp range `make_interval`
/// can subtract from `now()`. Without the ceiling an out-of-range value passes startup and
/// then raises `22008` on EVERY delivery — the present-but-unusable failure moved from boot
/// to the plane, where it pauses the subscription instead of stopping the process.
pub(crate) const MAX_RETENTION_DAYS: i32 = 3650;

/// ONLY an unset variable takes the compiled default. Anything PRESENT is parsed and must
/// be usable, the `NOTIFICATIONS_RETENTION_DAYS` convention
/// (`modules/notifications/src/projection.rs`): `GROUPS_RETENTION_DAYS=${RETENTION_DAYS}`
/// with the outer variable unset expands to empty, and defaulting it would prune stale
/// invites/requests two thirds early with nobody told.
pub(crate) fn retention_days_from_env() -> anyhow::Result<i32> {
    let raw = match std::env::var(RETENTION_ENV) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(DEFAULT_RETENTION_DAYS),
        Err(std::env::VarError::NotUnicode(v)) => anyhow::bail!(
            "groups: {RETENTION_ENV} is not valid unicode ({v:?}); unset it for the default \
             {DEFAULT_RETENTION_DAYS}"
        ),
    };
    let trimmed = raw.trim();
    let days: i32 = trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "groups: {RETENTION_ENV} must be a whole number of days (got {raw:?}); unset it \
             for the default {DEFAULT_RETENTION_DAYS}"
        )
    })?;
    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
        anyhow::bail!(
            "groups: {RETENTION_ENV} must be between 1 and {MAX_RETENTION_DAYS} (got {days}); \
             unset it for the default {DEFAULT_RETENTION_DAYS}"
        );
    }
    Ok(days)
}

/// Enumerated POSITIVELY and spelled out HERE rather than reused from `store`'s
/// `PENDING_ROWS`: that const answers "which rows does the pending list show", and a state
/// added to it later must not thereby become a state this statement destroys. The
/// `memberships_pending_idx` partial index in `SCHEMA_DDL` carries the SAME predicate — the
/// planner only uses a partial index it can prove this `WHERE` implies, and the batching in
/// [`PRUNE_BATCH`] assumes it does.
///
/// Every row is returned so the sweep can emit its terminal event; the third column is the
/// batch's highest `created_at` (equal on every row), the next batch's floor.
const STALE_BATCH_SQL: &str = "WITH stale AS ( \
       SELECT ctid FROM groups.memberships \
        WHERE state IN ('invited','requested') \
          AND created_at < now() - make_interval(days => $1) \
          AND created_at >= $3::timestamptz \
        ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED \
     ), del AS ( \
       DELETE FROM groups.memberships m USING stale \
        WHERE m.ctid = stale.ctid \
      RETURNING m.group_id::text AS gid, m.player_id::text AS pid, m.created_at AS ca \
     ) \
     SELECT gid, pid, (max(ca) OVER ())::text FROM del";

/// Just the `name` of a `scheduler.fired` payload: the prune subscribes by the contract's
/// topic const without importing the producer's payload type (audit's zero-coupling shape).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

pub(crate) struct PruneHandler {
    pub(crate) retention_days: i32,
    pub(crate) bus: Arc<Bus>,
}

impl TxHandler for PruneHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(BusError::from)?;
            if fired.name != PRUNE_SCHEDULE_NAME {
                return Ok(());
            }
            debug_assert!(
                self.retention_days > 0,
                "PruneHandler constructed with non-positive retention_days: {}",
                self.retention_days
            );
            let started = std::time::Instant::now();
            // The scan floor, carried across batches as text because the workspace's sqlx
            // has no date/time feature; `-infinity` is the first batch's "no floor".
            let mut watermark = "-infinity".to_string();
            loop {
                let rows: Vec<(String, String, String)> = sqlx::query_as(STALE_BATCH_SQL)
                    .bind(self.retention_days)
                    .bind(PRUNE_BATCH)
                    .bind(&watermark)
                    .fetch_all(&mut *conn)
                    .await
                    .map_err(BusError::transport)?;
                let deleted = rows.len() as i64;
                let high = rows.last().map(|(_, _, high)| high.clone());
                // Every removal path in this module owes the log a terminal event, and these
                // rows are live relations a player could still have acted on. Ids are the
                // DELETE's `RETURNING`, never a prior read.
                for (group_id, player_id, _) in rows {
                    self.bus
                        .emit_tx(
                            AnyTx::new(&mut *conn),
                            &groupsevents::MEMBER_LEFT,
                            &groupsevents::MemberLeft {
                                group_id,
                                player_id,
                                actor_id: String::new(),
                                reason: groupsevents::REASON_EXPIRED.to_string(),
                            },
                        )
                        .await?;
                }
                if deleted < PRUNE_BATCH {
                    return Ok(());
                }
                // `>=`, never `>`: rows sharing the batch's highest `created_at` may not all
                // have fit in this batch; the ones that did are already deleted, so
                // re-scanning the tie costs one batch, never a quadratic re-walk.
                if let Some(high) = high {
                    watermark = high;
                }
                // Checked between batches, never inside the emit loop: a deleted row's
                // terminal event may not be left unwritten, so the batch is the smallest
                // unit the budget can end on.
                if started.elapsed() >= PRUNE_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "groups: retention sweep hit its budget — resuming next fire"
                    );
                    return Ok(());
                }
            }
        })
    }
}

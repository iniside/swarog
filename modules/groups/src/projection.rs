use std::sync::Arc;

use bus::{AnyTx, Bus, Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use groupsapi::{ROLE_ADMIN, STATE_MEMBER};
use sqlx::PgConnection;

/// The retention sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. `Genesis` because the sweep must run on a boot that has never
/// subscribed before, the same reasoning as `notifications.prune-on-scheduler.v1`.
pub(crate) const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "groups.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// `Genesis`, not `AfterRegistration`: a lost deletion is a permanent retention leak with
/// no repair path, while a redelivery into an already-tombstoned module is a no-op. The id
/// and the start position are an IMMUTABLE contract (`spec_hash`).
pub(crate) const PLAYER_DELETED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "groups.player-deleted.v1",
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

/// Plants the purge tombstone on the HANDED delivery connection, so it commits with the
/// subscription checkpoint, and does no other work: nothing bounds a player's memberships,
/// while `ASYNCEVENTS_HANDLER_TIMEOUT` bounds this handler at 10s, so deleting them here
/// could make no progress and burn failures toward the plane's pause threshold. The deleting
/// (and its `group.member_left` emits, and the group locks they need) belongs to the
/// scheduled sweep, which resumes across fires; this row is that sweep's work item and,
/// once swept, the permanent record that this player is gone — redelivery is
/// `ON CONFLICT DO NOTHING`, never a reset of `swept_at`.
pub(crate) fn on_player_deleted<'a>(
    svc: Arc<crate::Service>,
    mut delivery: Delivery<'a>,
    e: accountsevents::PlayerDeleted,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        let conn = delivery.tx.downcast::<PgConnection>()?;
        svc.store
            .plant_purged_player_tx(conn, &e.player_id, &e.handle)
            .await
            .map_err(BusError::transport)
    })
}

/// The purge sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. Separate from both [`PRUNE_SUB`] and [`PLAYER_DELETED_SUB`] so a
/// paused sweep never stalls the tombstone or the retention prune, and vice versa. `Genesis`
/// because the sweep must run on a boot that has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "groups.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// ONE schedule for every module holding a `purged_players` table: the purge-latency promise
/// is a maximum across all of them, so a per-module name would let an operator accelerate
/// some and leave the rest at a day.
pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Tombstones claimed per selection statement. It bounds PLAYERS; [`GROUPS_PER_FIRE`] is
/// what bounds the work each one costs.
pub(crate) const PURGE_BATCH: i64 = 256;

/// Groups this fire may touch, and therefore the number of `pg_advisory_xact_lock`s the
/// delivery transaction can be holding at once: the lock is TRANSACTION-scoped and the
/// delivery transaction ends only after the whole fire, so nothing inside a fire ever gives
/// one back. This cluster sizes its shared lock table at `max_locks_per_transaction` x
/// (`max_connections` + `max_prepared_transactions`) = 64 x 150 = 9600 slots, so a sweep
/// holding 256 advisory locks plus its handful of relation locks takes under 3% of the
/// table; the plane delivers one subscription at a time per process, so the concurrent
/// count is a small multiple of that, not 150 of them.
///
/// Unlike the retention sweep this IS a per-fire cap, and it costs throughput: a backlog
/// above 256 groups per day drains over several days. A player whose groups outrun one fire
/// keeps an unstamped tombstone and resumes on what is left — the deletes already done are
/// committed, so every fire makes progress.
pub(crate) const GROUPS_PER_FIRE: usize = 256;

/// The sweep's whole wall-clock budget, spent only at a PLAYER boundary — after that
/// player's groups and its `swept_at` stamp, never between a membership delete and the
/// `group.member_left` that ends it. A module may not read the plane's env, so it is a bare
/// constant; the operator constraint that comes with it is that
/// `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s (default 10s, and nothing in the tree
/// sets it), below which the plane terminates the delivery backend instead and 20 such fires
/// pause the subscription.
pub(crate) const SWEEP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The scan floor carried across batches, load-bearing rather than an optimisation: every
/// batch runs inside the ONE still-open delivery transaction, where the tombstones this
/// transaction already stamped are dead-but-unprunable row versions a `swept_at IS NULL`
/// re-scan still walks, so a floorless loop goes quadratic. It buys that only while the batch
/// query's `ORDER BY` names the TABLE's `player_id`: unqualified it binds to the
/// `player_id::text` output column, which the primary key can serve as neither the ordering
/// nor an index condition, leaving a seq scan plus a sort that spills to disk inside that
/// transaction. `>=` rather than `>` costs one already-stamped row per batch and keeps the
/// floor value itself reachable.
const PURGE_FLOOR: &str = "00000000-0000-0000-0000-000000000000";

/// What one player's groups did within this fire.
enum PlayerOutcome {
    /// Every group is gone; the tombstone may be stamped.
    Purged,
    /// [`GROUPS_PER_FIRE`] is exhausted; the fire ends with this player unstamped.
    CapReached,
    /// One group failed and was rolled back to its savepoint; this player is skipped and
    /// the fire continues with the next one.
    Skipped,
}

impl crate::Service {
    /// Drains the tombstones planted on `player.deleted`: every purged player's memberships
    /// are deleted, and a group left with no admin (or no member) is DISSOLVED rather than
    /// succeeded — deleted along with every remaining row, one `group.member_left` per row.
    /// Every delete, every emit, every `swept_at` stamp and the subscription checkpoint
    /// commit together on the handed delivery tx.
    ///
    /// The sweep never answers `Conflict` the way `leave` does for a sole admin: a `Conflict`
    /// out of a durable handler is an `Err`, which backs the subscription off and pauses it
    /// after 20 failures, stranding every later player behind the cursor. It RESOLVES the
    /// last-admin invariant instead of reporting it.
    pub(crate) async fn sweep_purged_players(
        &self,
        conn: &mut PgConnection,
    ) -> Result<(), BusError> {
        let started = std::time::Instant::now();
        let mut slots = GROUPS_PER_FIRE;
        let mut watermark = PURGE_FLOOR.to_string();
        loop {
            let due: Vec<(String,)> = sqlx::query_as(
                "SELECT player_id::text FROM groups.purged_players \
                  WHERE swept_at IS NULL AND player_id >= $1::uuid \
                  ORDER BY groups.purged_players.player_id \
                  LIMIT $2 FOR UPDATE SKIP LOCKED",
            )
            .bind(&watermark)
            .bind(PURGE_BATCH)
            .fetch_all(&mut *conn)
            .await
            .map_err(BusError::transport)?;
            for (player_id,) in &due {
                match self.purge_player_groups(conn, player_id, &mut slots).await? {
                    PlayerOutcome::Purged => {}
                    PlayerOutcome::CapReached => {
                        tracing::warn!(
                            player_id,
                            groups_per_fire = GROUPS_PER_FIRE,
                            "groups: purge sweep hit its per-fire group cap — resuming next fire"
                        );
                        return Ok(());
                    }
                    PlayerOutcome::Skipped => continue,
                }
                sqlx::query(
                    "UPDATE groups.purged_players SET swept_at = now() \
                      WHERE player_id = $1::uuid",
                )
                .bind(player_id)
                .execute(&mut *conn)
                .await
                .map_err(BusError::transport)?;
                if started.elapsed() >= SWEEP_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "groups: purge sweep hit its budget — resuming next fire"
                    );
                    return Ok(());
                }
            }
            if (due.len() as i64) < PURGE_BATCH {
                return Ok(());
            }
            if let Some((last,)) = due.last() {
                watermark.clone_from(last);
            }
        }
    }

    /// One player's groups, each under its own SAVEPOINT. The savepoint is what lets a
    /// failing group cost only its own player: rolling back to it releases the advisory lock
    /// that subtransaction took and leaves the delivery transaction usable, so the fire
    /// carries on with the next tombstone instead of returning an `Err` that discards every
    /// player already swept and walks the subscription toward a pause.
    ///
    /// An `Err` here is therefore never a group's own failure — only the savepoint statements
    /// themselves failing, which means the connection is gone.
    async fn purge_player_groups(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        slots: &mut usize,
    ) -> Result<PlayerOutcome, BusError> {
        loop {
            if *slots == 0 {
                return Ok(PlayerOutcome::CapReached);
            }
            let groups = self
                .store
                .player_groups_tx(&mut *conn, player_id, *slots as i64)
                .await
                .map_err(BusError::transport)?;
            if groups.is_empty() {
                return Ok(PlayerOutcome::Purged);
            }
            for group_id in groups {
                sqlx::query("SAVEPOINT purge_group")
                    .execute(&mut *conn)
                    .await
                    .map_err(BusError::transport)?;
                match self.purge_one_group(&mut *conn, &group_id, player_id).await {
                    Ok(()) => {
                        sqlx::query("RELEASE SAVEPOINT purge_group")
                            .execute(&mut *conn)
                            .await
                            .map_err(BusError::transport)?;
                        *slots -= 1;
                    }
                    Err(e) => {
                        sqlx::query("ROLLBACK TO SAVEPOINT purge_group")
                            .execute(&mut *conn)
                            .await
                            .map_err(BusError::transport)?;
                        tracing::warn!(
                            player_id,
                            group_id,
                            error = %e,
                            "groups: purge sweep skipped a player whose group failed — its \
                             tombstone stays unswept"
                        );
                        return Ok(PlayerOutcome::Skipped);
                    }
                }
            }
        }
    }

    /// The purged player's row in ONE group, under that group's advisory lock — the same lock
    /// `join`/`leave`/`respond`/`decide` take before the read they decide from, so the counts
    /// this dissolution decides from cannot change under it.
    ///
    /// Dissolution is D7: a group that just lost its last admin, or its last member, is
    /// deleted along with every row still pointing at it. `groups.groups.creator_id` keeps
    /// naming the deleted player and is left alone — it is a plain id column with no live
    /// group behind it once this runs.
    async fn purge_one_group(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
    ) -> Result<(), BusError> {
        self.store
            .lock_group_tx(&mut *conn, group_id)
            .await
            .map_err(BusError::transport)?;
        let counts = self
            .store
            .counts_tx(&mut *conn, group_id, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(BusError::transport)?;
        let deleted = self
            .store
            .delete_membership_tx(&mut *conn, group_id, player_id)
            .await
            .map_err(BusError::transport)?;
        // The row can be gone already: `player_groups_tx` read it before this lock was taken.
        let Some((state, role, group_id, player_id)) = deleted else {
            return Ok(());
        };
        self.bus
            .emit_tx(
                AnyTx::new(&mut *conn),
                &groupsevents::MEMBER_LEFT,
                &groupsevents::MemberLeft {
                    group_id: group_id.clone(),
                    player_id: player_id.clone(),
                    actor_id: player_id.clone(),
                    reason: groupsevents::REASON_ACCOUNT_DELETED.to_string(),
                },
            )
            .await?;
        let dissolves = state == STATE_MEMBER
            && (counts.members == 1 || (role == ROLE_ADMIN && counts.admins == 1));
        if !dissolves {
            return Ok(());
        }
        let swept = self
            .store
            .delete_group_tx(&mut *conn, &group_id)
            .await
            .map_err(BusError::transport)?;
        for swept_id in swept {
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *conn),
                    &groupsevents::MEMBER_LEFT,
                    &groupsevents::MemberLeft {
                        group_id: group_id.clone(),
                        player_id: swept_id,
                        // No party ended these relations: the deleted player never acted on
                        // them, the group did.
                        actor_id: String::new(),
                        reason: groupsevents::REASON_GROUP_DISSOLVED.to_string(),
                    },
                )
                .await?;
        }
        Ok(())
    }
}

pub(crate) struct PurgeSweepHandler {
    pub(crate) svc: Arc<crate::Service>,
}

impl TxHandler for PurgeSweepHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(BusError::from)?;
            if fired.name != PURGE_SCHEDULE_NAME {
                return Ok(());
            }
            self.svc.sweep_purged_players(conn).await
        })
    }
}

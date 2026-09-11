use std::sync::Arc;

use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::service::Service;

/// The tombstone's consumer-owned subscription id — a durable contract; renaming it abandons
/// the checkpoint. `Genesis` because a lost deletion is a permanent retention leak with no
/// repair path, while a redelivery into an already-tombstoned module is a no-op.
pub(crate) const PLAYER_DELETED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "storage.player-deleted.v1",
    start: bus::StartPosition::Genesis,
};

/// The sweep's own subscription id, separate from [`PLAYER_DELETED_SUB`] so a paused sweep
/// never stalls the tombstone, and vice versa. `Genesis` because the sweep must run on a boot
/// that has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "storage.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// ONE schedule for every module holding a `purged_players` table: the purge-latency promise
/// is a maximum across all of them, so a per-module name would let an operator accelerate
/// some and leave the rest at a day.
pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Owners one fire may sweep, and therefore the transaction-scoped advisory locks the
/// delivery transaction accumulates: a lock taken in a transaction is released only when that
/// transaction ends, so each owner swept keeps that owner's `put` blocked until the fire
/// commits. 64 is `groups::projection::GROUPS_PER_FIRE`'s number, where the binding constraint
/// is the backend's cached subxid array (`PGPROC_MAX_CACHED_SUBXIDS` = 64) rather than the
/// 9600-slot shared lock table; this sweep opens no subtransaction, so what the number bounds
/// here is those locks and the owners waiting behind them.
///
/// It is equally the claim statement's `LIMIT`: ONE constant decides how many tombstones a
/// fire claims and how many it may work, so the two cannot disagree. A backlog above 64
/// players per day drains over several days, and every fire commits what it swept, so each
/// one makes progress.
pub(crate) const STORAGE_OWNERS_PER_FIRE: i64 = 64;

/// The sweep's whole wall-clock budget. Exhausting it ends the fire with the players swept so
/// far KEPT — they commit with the subscription checkpoint and the next fire resumes from
/// there. A module may not read the plane's env, so it is a bare constant; the operator
/// constraint that comes with it is that `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s
/// (default 10s, and nothing in the tree sets it), below which the plane terminates the
/// delivery backend instead and 20 such fires pause the subscription.
pub(crate) const SWEEP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Just the `name` of a `scheduler.fired` payload: the sweep subscribes by the contract's
/// topic const without importing the producer's payload type.
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

impl Service {
    /// Drains the tombstones planted on `player.deleted`: a purged owner's objects, in every
    /// collection, deleted under that owner's advisory lock. Every delete, every `swept_at`
    /// stamp and the subscription checkpoint commit together on the handed delivery tx.
    ///
    /// The lock is what makes the drain final. Without it a `put` that read no tombstone
    /// before the plant committed could land its row AFTER this statement passed the owner,
    /// and nothing would ever collect it: this module has no time-based pruning, so an object
    /// outlives its owner forever. `Service::create` takes the same lock and re-reads the
    /// tombstone under it, so one of the two orders always loses.
    ///
    /// Lock order is identical on both sides — advisory lock, then row locks — so a `put` and
    /// this sweep cannot deadlock; the sweep waits for a live `put`, never the reverse.
    pub(crate) async fn sweep_purged_players(
        &self,
        conn: &mut PgConnection,
    ) -> Result<(), BusError> {
        let started = std::time::Instant::now();
        // `ORDER BY` names the TABLE's `player_id`: unqualified it binds to the
        // `player_id::text` output column, which `purged_players_pending_idx` can serve as
        // neither the ordering nor an index condition, leaving a seq scan plus a sort that
        // spills to disk inside the delivery transaction.
        let due: Vec<(String,)> = sqlx::query_as(
            "SELECT player_id::text FROM storage.purged_players \
              WHERE swept_at IS NULL \
              ORDER BY storage.purged_players.player_id \
              LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(STORAGE_OWNERS_PER_FIRE)
        .fetch_all(&mut *conn)
        .await
        .map_err(BusError::transport)?;
        let mut swept = 0usize;
        for (player_id,) in &due {
            self.store
                .lock_owner_tx(&mut *conn, player_id)
                .await
                .map_err(BusError::transport)?;
            self.store
                .delete_owner_objects_tx(&mut *conn, player_id)
                .await
                .map_err(BusError::transport)?;
            sqlx::query(
                "UPDATE storage.purged_players SET swept_at = now() WHERE player_id = $1::uuid",
            )
            .bind(player_id)
            .execute(&mut *conn)
            .await
            .map_err(BusError::transport)?;
            swept += 1;
            // The budget is spent only at a player boundary — after the delete AND the stamp,
            // never between them, or the fire would commit a swept owner it never stamped and
            // sweep it again. One owner's delete is a single statement bounded by
            // `MAX_OBJECTS_PER_OWNER`, so the coarse granularity cannot strand a fire.
            if started.elapsed() >= SWEEP_BUDGET {
                tracing::warn!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    swept,
                    "storage: purge sweep hit its budget — resuming next fire"
                );
                return Ok(());
            }
        }
        Ok(())
    }
}

pub(crate) struct PurgeSweepHandler {
    pub(crate) svc: Arc<Service>,
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

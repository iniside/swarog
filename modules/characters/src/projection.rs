use std::sync::Arc;

use bus::{AnyTx, Delivery, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::service::Service;

/// The purge sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. Separate from `characters.player-deleted.v1` so a paused sweep
/// never stalls the tombstone, and vice versa. `Genesis` because the sweep must run on a boot
/// that has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "characters.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// ONE schedule for every module holding a `purged_players` table: the purge-latency promise
/// is a maximum across all of them, so a per-module name would let an operator accelerate
/// some and leave the rest at a day.
pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Tombstones claimed per selection statement. It bounds PLAYERS, not characters — one
/// player's rows go in a single statement — and the sweep LOOPS batches until a short one, so
/// a backlog above one batch per day still drains.
pub(crate) const PURGE_BATCH: i64 = 256;

/// The sweep's whole wall-clock budget. Exhausting it ends the fire with the players swept so
/// far KEPT — they commit with the subscription checkpoint and the next fire resumes from
/// there. A module may not read the plane's env, so it is a bare constant; the operator
/// constraint that comes with it is that `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s
/// (default 10s, and nothing in the tree sets it), below which the plane terminates the
/// delivery backend instead and 20 such fires pause the subscription.
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

/// Just the `name` of a `scheduler.fired` payload: the sweep subscribes by the contract's
/// topic const without importing the producer's payload type.
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

impl Service {
    /// Drains the tombstones planted on `player.deleted`: every purged player's characters are
    /// deleted and one `character.deleted` is emitted per row. Every delete, every emit, every
    /// `swept_at` stamp and the subscription checkpoint commit together on the handed delivery
    /// tx.
    ///
    /// The emit is the CASCADE, not a courtesy: `character.deleted` is the only writer of
    /// `inventory.wiped_characters`, itself the only guard against a late `character.created`
    /// resurrecting a deleted character's holdings. It carries the `RETURNING` values, never a
    /// prior read, so a redelivery — which deletes nothing — emits nothing.
    ///
    /// `list_by_player` is deliberately not used: it truncates at `LIST_HARD_LIMIT`, which
    /// would leave a player's surplus characters behind permanently.
    pub(crate) async fn sweep_purged_players(
        &self,
        conn: &mut PgConnection,
    ) -> Result<(), bus::Error> {
        let started = std::time::Instant::now();
        let mut swept = 0usize;
        let mut watermark = PURGE_FLOOR.to_string();
        loop {
            let due: Vec<(String,)> = sqlx::query_as(
                "SELECT player_id::text FROM characters.purged_players \
                  WHERE swept_at IS NULL AND player_id >= $1::uuid \
                  ORDER BY characters.purged_players.player_id \
                  LIMIT $2 FOR UPDATE SKIP LOCKED",
            )
            .bind(&watermark)
            .bind(PURGE_BATCH)
            .fetch_all(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
            for (player_id,) in &due {
                let removed: Vec<(String, String)> = sqlx::query_as(
                    "DELETE FROM characters.characters WHERE player_id = $1::uuid \
                     RETURNING id::text, player_id::text",
                )
                .bind(player_id)
                .fetch_all(&mut *conn)
                .await
                .map_err(bus::Error::transport)?;
                for (character_id, canonical_player_id) in removed {
                    let evt = charactersevents::Deleted {
                        character_id,
                        player_id: canonical_player_id,
                    };
                    self.bus
                        .emit_tx(AnyTx::new(&mut *conn), &charactersevents::DELETED, &evt)
                        .await
                        .map_err(bus::Error::transport)?;
                }
                sqlx::query(
                    "UPDATE characters.purged_players SET swept_at = now() \
                      WHERE player_id = $1::uuid",
                )
                .bind(player_id)
                .execute(&mut *conn)
                .await
                .map_err(bus::Error::transport)?;
                swept += 1;
                // The budget is spent only at a player boundary — after the emits and the
                // stamp, never between a row's delete and the `character.deleted` that
                // cascades it. Resumption is therefore at PLAYER granularity: one player
                // whose single `DELETE … RETURNING` outruns `ASYNCEVENTS_HANDLER_TIMEOUT`
                // fails identically every retry and pauses this subscription.
                if started.elapsed() >= SWEEP_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        swept,
                        "characters: purge sweep hit its budget — resuming next fire"
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
}

pub(crate) struct PurgeSweepHandler {
    pub(crate) svc: Arc<Service>,
}

impl TxHandler for PurgeSweepHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), bus::Error>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(bus::Error::from)?;
            if fired.name != PURGE_SCHEDULE_NAME {
                return Ok(());
            }
            self.svc.sweep_purged_players(conn).await
        })
    }
}

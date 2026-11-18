use std::sync::Arc;

use bus::{AnyTx, Delivery, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::Service;

/// The purge sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. Separate from `friends.player-deleted.v1` so a paused sweep never
/// stalls the tombstone, and vice versa. `Genesis` because the sweep must run on a boot that
/// has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "friends.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// ONE schedule for every module holding a `purged_players` table: the purge-latency promise
/// is a maximum across all of them, so a per-module name would let an operator accelerate
/// some and leave the rest slower.
pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Players one fire may sweep, and therefore the transaction-scoped advisory locks the
/// delivery transaction accumulates: each player swept keeps requests naming that player
/// blocked until the fire commits. It is equally the claim statement's `LIMIT`, so one
/// constant decides both; a backlog above 64 players per fire drains over several fires.
pub(crate) const FRIENDS_PLAYERS_PER_FIRE: i64 = 64;

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
    /// Drains the tombstones planted on `player.deleted`: every purged player's edges are
    /// deleted and one `friend.removed` is emitted per row. Every delete, every emit, every
    /// `swept_at` stamp and the subscription checkpoint commit together on the handed
    /// delivery tx.
    ///
    /// The other party's handle is EMPTY, the value `party()` already produces for a
    /// directory miss: resolving it would mean a cross-process `accountsapi::Directory` call
    /// while holding the delivery transaction open, which this module's event contract rules
    /// out. `actor_handle` is the tombstone's, carried from the `player.deleted` payload.
    ///
    /// The emitted rows are the `DELETE … RETURNING` values, never a prior read, so a
    /// redelivery — which deletes nothing — emits nothing.
    pub(crate) async fn sweep_purged_players(
        &self,
        conn: &mut PgConnection,
    ) -> Result<(), bus::Error> {
        let started = std::time::Instant::now();
        // `ORDER BY` names the TABLE's `player_id`: unqualified it binds to the
        // `player_id::text` output column, which `purged_players_pending_idx` can serve as
        // neither the ordering nor an index condition, leaving a seq scan plus a sort inside
        // the delivery tx.
        let due: Vec<(String, String)> = sqlx::query_as(
            "SELECT player_id::text, handle FROM friends.purged_players \
              WHERE swept_at IS NULL \
              ORDER BY friends.purged_players.player_id \
              LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(FRIENDS_PLAYERS_PER_FIRE)
        .fetch_all(&mut *conn)
        .await
        .map_err(bus::Error::transport)?;
        for (swept, (player_id, handle)) in due.iter().enumerate() {
            self.store
                .lock_player_tx(&mut *conn, player_id)
                .await
                .map_err(bus::Error::transport)?;
            let removed = self
                .store
                .purge_edges_tx(&mut *conn, player_id)
                .await
                .map_err(bus::Error::transport)?;
            for (edge_id, other_id) in removed {
                let evt = friendsevents::Removed {
                    edge_id,
                    actor_id: player_id.clone(),
                    actor_handle: handle.clone(),
                    other_id,
                    other_handle: String::new(),
                    reason: friendsevents::REASON_ACCOUNT_DELETED.to_string(),
                };
                self.bus
                    .emit_tx(AnyTx::new(&mut *conn), &friendsevents::REMOVED, &evt)
                    .await
                    .map_err(bus::Error::transport)?;
            }
            sqlx::query(
                "UPDATE friends.purged_players SET swept_at = now() \
                  WHERE player_id = $1::uuid",
            )
            .bind(player_id)
            .execute(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
            // The budget is spent only at a player boundary — after the emits and the stamp,
            // never between a row's delete and the `friend.removed` that ends it. Resumption
            // is therefore at PLAYER granularity: one player whose single `DELETE … RETURNING`
            // outruns `ASYNCEVENTS_HANDLER_TIMEOUT` fails identically every retry and pauses
            // this subscription.
            if started.elapsed() >= SWEEP_BUDGET {
                tracing::warn!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    swept = swept + 1,
                    "friends: purge sweep hit its budget — resuming next fire"
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

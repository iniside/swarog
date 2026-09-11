use std::sync::Arc;

use bus::{Delivery, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;
use walletapi::{Movement, MAX_MOVEMENT_AMOUNT};

use crate::{Outcome, Service};

/// The starter grant is OFF unless configured: an empty currency and a zero amount are the
/// compiled defaults, so a wallet nobody configured grants nothing. Enabling it is a
/// `config` write (`wallet/starter_currency`, `wallet/starter_amount`), never an env var or
/// a code change.
pub(crate) const STARTER_CURRENCY: &str = "";
pub(crate) const STARTER_AMOUNT: i64 = 0;

pub(crate) const REASON: &str = "starter-grant";

impl Service {
    /// Reads the starter knobs straight off the injected `config` reader — no wallet-owned
    /// second cache: the reader is itself a replica-local cache kept fresh by the app-owned
    /// broadcast invalidation plane, so another one would only add a staleness window.
    fn starter_spec(&self) -> (String, i64) {
        let cfg = self
            .config
            .get()
            .expect("wallet.init must resolve config before use");
        (
            cfg.get_string("wallet", "starter_currency", STARTER_CURRENCY),
            cfg.get_int("wallet", "starter_amount", STARTER_AMOUNT),
        )
    }

    /// Credits a brand-new player the configured starter amount. `conn` is the plane's
    /// HANDED delivery transaction (never the pool), so the balance, the ledger row, the
    /// `wallet.changed` append and the subscription checkpoint commit as one unit.
    ///
    /// **Every data-quality verdict returns `Ok(())`.** An `Err` here backs the delivering
    /// subscription off and, after 20 consecutive failures, PAUSES it for every subsequent
    /// player — a fat-fingered config knob or an unseeded catalog must never cost that,
    /// because the fault is a property of the config, not of the event.
    ///
    /// The three pre-checks are therefore ORDERING-CRITICAL, not defensive:
    /// [`Service::apply_on`] validates the movement itself, so anything `validate_movement`
    /// would reject arrives as an `Err`; and letting the currency FK or the `$n::uuid` cast
    /// fire would abort the delivery transaction, after which even an `Ok` fails the plane's
    /// checkpoint `UPDATE` with 25P02. With the clamp below, the uuid check (which also bounds
    /// the derived key at 47 of `MAX_IDEMPOTENCY_KEY_BYTES`), a catalog whose own
    /// `currencies_code_len_check` caps a code at 32 octets, and fixed `reason` / key shapes,
    /// every one of those branches is unreachable BY CONSTRUCTION — which is why no
    /// `validate_movement` call is repeated here.
    ///
    /// One `Err` path survives that argument and is genuinely REACHABLE, because this is the
    /// first module to `emit_tx` INSIDE a delivery transaction: [`Service::apply_on`] appends
    /// `wallet.changed` on this same handed connection, and the plane's `enqueue_tx` runs
    /// `ensure_history_contract` there — which FAILS LOUDLY when a stored `history_contracts`
    /// row records a different policy than this process's code declares — before
    /// `append_event`. Both are `Err`s the savepoint recovers, so the posture above holds; the
    /// consequence to know is that a `wallet.changed` history-policy skew across a
    /// mixed-version fleet degrades THIS subscription (backoff, then pause) rather than
    /// failing one wire call.
    ///
    /// `player_id` is the one input wallet does not mint, so its shape is checked HERE rather
    /// than trusted from `accounts`: the wire paths must keep answering 400 for a malformed
    /// id, but on this path a 400 is an `Err` is a fault, and one producer's bad payload would
    /// cost every later player their grant.
    pub(crate) async fn grant_starter(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), bus::Error> {
        let (currency, amount) = self.starter_spec();
        if currency.is_empty() || amount <= 0 {
            return Ok(());
        }
        if amount > MAX_MOVEMENT_AMOUNT {
            tracing::warn!(
                amount,
                max = MAX_MOVEMENT_AMOUNT,
                "wallet: configured starter_amount out of range — granting nothing"
            );
            return Ok(());
        }
        if !crate::is_uuid_text(player_id) {
            tracing::warn!(
                player_id,
                "wallet: starter-grant event carried a player_id that is not a uuid — granting \
                 nothing"
            );
            return Ok(());
        }
        if !self
            .store
            .currency_exists_tx(&mut *conn, &currency)
            .await
            .map_err(bus::Error::transport)?
        {
            tracing::warn!(
                %currency,
                "wallet: configured starter_currency is not in the catalog — granting nothing"
            );
            return Ok(());
        }

        let movement = Movement {
            idempotency_key: format!("starter:{player_id}"),
            player_id: player_id.to_string(),
            currency,
            amount,
            reason: REASON.to_string(),
        };
        match self
            .apply_on(conn, &movement, 1)
            .await
            .map_err(bus::Error::transport)?
        {
            Outcome::Applied(_) | Outcome::Duplicate(_) => Ok(()),
            Outcome::Conflict => {
                tracing::warn!(
                    player_id,
                    key = %movement.idempotency_key,
                    "wallet: starter-grant key already records a different movement — granting nothing"
                );
                Ok(())
            }
        }
    }
}

/// The purge sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. Separate from `wallet.player-deleted.v1` so a paused sweep never
/// stalls the tombstone, and vice versa. `Genesis` because the sweep must run on a boot that
/// has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "wallet.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// ONE schedule for every module holding a `purged_players` table: the purge-latency promise
/// is a maximum across all of them, so a per-module name would let an operator accelerate
/// some and leave the rest at a day.
pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Tombstones claimed per selection statement. It bounds PLAYERS, not ledger rows — a
/// player's ledger is unbounded and its rewrite is one statement — and the sweep LOOPS
/// batches until a short one, so a backlog above one batch per day still drains.
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
    /// Drains the tombstones planted on `player.deleted`: every purged player's balances are
    /// deleted and their ledger rows re-owned to `Store::PURGE_SENTINEL`. Every write, every
    /// `swept_at` stamp and the subscription checkpoint commit together on the handed
    /// delivery tx.
    pub(crate) async fn sweep_purged_players(
        &self,
        conn: &mut PgConnection,
    ) -> Result<(), bus::Error> {
        let started = std::time::Instant::now();
        let mut watermark = PURGE_FLOOR.to_string();
        loop {
            let due: Vec<(String,)> = sqlx::query_as(
                "SELECT player_id::text FROM wallet.purged_players \
                  WHERE swept_at IS NULL AND player_id >= $1::uuid \
                  ORDER BY wallet.purged_players.player_id \
                  LIMIT $2 FOR UPDATE SKIP LOCKED",
            )
            .bind(&watermark)
            .bind(PURGE_BATCH)
            .fetch_all(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
            for (player_id,) in &due {
                self.store
                    .purge_player_tx(&mut *conn, player_id)
                    .await
                    .map_err(bus::Error::transport)?;
                sqlx::query(
                    "UPDATE wallet.purged_players SET swept_at = now() WHERE player_id = $1::uuid",
                )
                .bind(player_id)
                .execute(&mut *conn)
                .await
                .map_err(bus::Error::transport)?;
                // The budget is spent only at a player boundary — after the stamp, never
                // between a player's writes and its stamp. All six sweeps place it here.
                if started.elapsed() >= SWEEP_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "wallet: purge sweep hit its budget — resuming next fire"
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

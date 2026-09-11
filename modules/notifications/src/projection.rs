use std::sync::Arc;

use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::service::{is_uuid_text, NewNotification, Service};

pub(crate) const KIND_WALLET_CREDIT: &str = "wallet.credit";
pub(crate) const KIND_ACCOUNT_PROMOTED: &str = "account.promoted";
pub(crate) const KIND_FRIEND_REQUESTED: &str = "friend.requested";
pub(crate) const KIND_FRIEND_ACCEPTED: &str = "friend.accepted";

pub(crate) const WALLET_CHANGED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.wallet-changed.v1",
    // `Genesis` would replay every retained `wallet.changed` into every existing player's
    // inbox on the first boot of this module.
    start: bus::StartPosition::AfterRegistration,
};

pub(crate) const PLAYER_PROMOTED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.player-promoted.v1",
    start: bus::StartPosition::AfterRegistration,
};

/// `Genesis` would replay every retained friendship into every existing inbox on this
/// module's first boot — the reason `WALLET_CHANGED_SUB` already gives.
pub(crate) const FRIEND_REQUESTED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.friend-requested.v1",
    start: bus::StartPosition::AfterRegistration,
};

/// As [`FRIEND_REQUESTED_SUB`].
pub(crate) const FRIEND_ACCEPTED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.friend-accepted.v1",
    start: bus::StartPosition::AfterRegistration,
};

/// `Genesis`, not the `AfterRegistration` the content subscriptions take: a lost deletion
/// is a permanent retention leak with no repair path, and a redelivery into an
/// already-tombstoned module is a no-op — there is no inbox row to duplicate. The id and
/// the start position are an IMMUTABLE contract (`spec_hash`).
pub(crate) const PLAYER_DELETED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.player-deleted.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::NOTIFICATIONS_PRUNE;

pub(crate) const DEFAULT_RETENTION_DAYS: i32 = 30;

pub(crate) const RETENTION_ENV: &str = "NOTIFICATIONS_RETENTION_DAYS";

/// Rows per statement. The sweep LOOPS batches until a short one — a per-fire CAP would
/// leave retention permanently behind any inflow above one batch per day, since this
/// schedule fires once every 86400s — and each batch resumes from the previous batch's
/// highest `created_at`. That watermark is load-bearing, not an optimisation: every batch
/// runs inside the ONE still-open delivery transaction, where the tuples this transaction
/// already deleted are neither killable nor prunable from the index, so a watermark-less
/// scan re-walks all `256 x (k-1)` of them and the loop goes quadratic.
pub(crate) const PRUNE_BATCH: i64 = 256;

/// A sweep's whole wall-clock budget — one constant for BOTH scheduled sweeps in this
/// module, since the bound it respects is the plane's per-handler timeout, not either
/// sweep's subject matter. Exhausting it ends the fire with the batches so far
/// KEPT — they commit with the checkpoint, and the next fire resumes from there.
///
/// It is a bare constant because a module may not read the plane's env and `bus::Delivery`
/// carries no deadline to derive one from; the operator constraint that comes with it is
/// that `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s (its default is 10s, and nothing
/// in the tree sets it). Below that the sweep is killed rather than budgeted: the plane
/// `pg_terminate_backend`s the delivery backend, so the transaction dies with it and EVERY
/// batch of that fire is lost — no savepoint rollback is involved — and 20 such fires
/// (`PAUSE_AFTER`, i.e. 20 days on a daily schedule) pause the subscription.
pub(crate) const SWEEP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Ten years: far past any inbox policy, and far inside the timestamp range `make_interval`
/// can subtract from `now()`. Without the ceiling an out-of-range value passes startup and
/// then raises `22008` on EVERY delivery — the present-but-unusable failure moved from boot
/// to the plane, where it pauses the subscription instead of stopping the process.
pub(crate) const MAX_RETENTION_DAYS: i32 = 3650;

/// ONLY an unset variable takes the compiled default. Anything PRESENT is parsed and must
/// be usable, the `ASYNCEVENTS_HANDLER_TIMEOUT` convention (`core/asyncevents/src/worker.rs`,
/// where `parse_duration` rejects an empty value): `NOTIFICATIONS_RETENTION_DAYS=${RETENTION_DAYS}`
/// with the outer variable unset expands to empty, and defaulting it would prune a 90-day
/// inbox two thirds early with nobody told.
pub(crate) fn retention_days_from_env() -> anyhow::Result<i32> {
    let raw = match std::env::var(RETENTION_ENV) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(DEFAULT_RETENTION_DAYS),
        Err(std::env::VarError::NotUnicode(v)) => anyhow::bail!(
            "notifications: {RETENTION_ENV} is not valid unicode ({v:?}); unset it for the \
             default {DEFAULT_RETENTION_DAYS}"
        ),
    };
    let trimmed = raw.trim();
    let days: i32 = trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "notifications: {RETENTION_ENV} must be a whole number of days (got {raw:?}); \
             unset it for the default {DEFAULT_RETENTION_DAYS}"
        )
    })?;
    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
        anyhow::bail!(
            "notifications: {RETENTION_ENV} must be between 1 and {MAX_RETENTION_DAYS} (got \
             {days}); unset it for the default {DEFAULT_RETENTION_DAYS}"
        );
    }
    Ok(days)
}

/// The delivery-path insert: it runs on the plane's HANDED transaction, so the row and the
/// subscription checkpoint commit together.
///
/// A `Status::Invalid` verdict is ONE message's data quality and answers `Ok(())`: an `Err`
/// backs the subscription off and eventually PAUSES it, taking every player's inbox offline
/// over one bad payload. Anything else is infrastructure and propagates so the plane retries.
async fn deliver_or_skip(
    svc: &Service,
    conn: &mut PgConnection,
    n: &NewNotification<'_>,
) -> Result<(), BusError> {
    if !deliverable_player_id(n.player_id) {
        return Ok(());
    }
    match svc.deliver_on(conn, n).await {
        Ok(_) => Ok(()),
        Err(e) if e.status == opsapi::Status::Invalid => {
            tracing::warn!(
                kind = n.kind,
                reason = %e.msg,
                "notifications: durable event rejected on data quality — no inbox row"
            );
            Ok(())
        }
        Err(e) => Err(BusError::transport(e)),
    }
}

/// The DURABLE path's id policy, stricter than the column's tolerant `$n::uuid` cast: only
/// the canonical 36-character hyphenated spelling (hex digits either case) is delivered,
/// because a 22P02 aborts the plane's
/// delivery transaction and the checkpoint `UPDATE` that follows an `Ok(())` would then fail
/// with 25P02 — a skip here has to cost no statement at all. Producers mint canonical ids, so
/// a payload that fails this is a producer bug, logged and dropped rather than replayed
/// forever. The operator path keeps the tolerant cast (see `SCHEMA_DDL`'s prose) and answers
/// a bad id 400 through `Service::deliver_on`'s own 22P02 arm, which is unreachable from here.
fn deliverable_player_id(player_id: &str) -> bool {
    if is_uuid_text(player_id) {
        return true;
    }
    tracing::warn!(
        player_id,
        "notifications: durable event carried a player_id that is not a uuid — no inbox row"
    );
    false
}

pub(crate) fn wallet_credit_body(e: &walletevents::Changed) -> String {
    format!(
        "{} {} was added to your wallet ({}). New balance: {}.",
        e.delta, e.currency, e.reason, e.balance_after
    )
}

pub(crate) fn promoted_body(e: &accountsevents::PlayerPromoted) -> String {
    format!(
        "Your guest account is now a permanent {} account. Your progress is saved.",
        e.to_provider
    )
}

pub(crate) fn friend_requested_body(e: &friendsevents::Requested) -> String {
    format!("{} sent you a friend request.", e.requester_handle)
}

pub(crate) fn friend_accepted_body(e: &friendsevents::Accepted) -> String {
    format!("{} accepted your friend request.", e.addressee_handle)
}

/// One inbox row per CREDIT. A debit moves the player's money away from them, which the
/// wallet page already shows; only an incoming amount is news.
pub(crate) fn on_wallet_changed<'a>(
    svc: Arc<Service>,
    mut delivery: Delivery<'a>,
    e: walletevents::Changed,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        if e.delta <= 0 {
            return Ok(());
        }
        let event_id = delivery.event_id;
        let conn = delivery.tx.downcast::<PgConnection>()?;
        deliver_or_skip(
            &svc,
            conn,
            &NewNotification {
                player_id: &e.player_id,
                kind: KIND_WALLET_CREDIT,
                title: "Currency received",
                body: &wallet_credit_body(&e),
                source_event_id: event_id,
            },
        )
        .await
    })
}

pub(crate) fn on_player_promoted<'a>(
    svc: Arc<Service>,
    mut delivery: Delivery<'a>,
    e: accountsevents::PlayerPromoted,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        let event_id = delivery.event_id;
        let conn = delivery.tx.downcast::<PgConnection>()?;
        deliver_or_skip(
            &svc,
            conn,
            &NewNotification {
                player_id: &e.player_id,
                kind: KIND_ACCOUNT_PROMOTED,
                title: "Welcome — your account is permanent",
                body: &promoted_body(&e),
                source_event_id: event_id,
            },
        )
        .await
    })
}

/// Recipient is the ADDRESSEE — the party who received the ask and can act on it.
pub(crate) fn on_friend_requested<'a>(
    svc: Arc<Service>,
    mut delivery: Delivery<'a>,
    e: friendsevents::Requested,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        let event_id = delivery.event_id;
        let conn = delivery.tx.downcast::<PgConnection>()?;
        deliver_or_skip(
            &svc,
            conn,
            &NewNotification {
                player_id: &e.addressee_id,
                kind: KIND_FRIEND_REQUESTED,
                title: "New friend request",
                body: &friend_requested_body(&e),
                source_event_id: event_id,
            },
        )
        .await
    })
}

/// Recipient is the original REQUESTER — the party who waited for an answer. The
/// auto-accept branch (`friends`' reverse-pending path) preserves these roles from the
/// original request, so this addressing is correct there too.
pub(crate) fn on_friend_accepted<'a>(
    svc: Arc<Service>,
    mut delivery: Delivery<'a>,
    e: friendsevents::Accepted,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        let event_id = delivery.event_id;
        let conn = delivery.tx.downcast::<PgConnection>()?;
        deliver_or_skip(
            &svc,
            conn,
            &NewNotification {
                player_id: &e.requester_id,
                kind: KIND_FRIEND_ACCEPTED,
                title: "Friend request accepted",
                body: &friend_accepted_body(&e),
                source_event_id: event_id,
            },
        )
        .await
    })
}

/// Just the `name` of a `scheduler.fired` payload: the prune subscribes by the contract's
/// topic const without importing the producer's payload type (audit's zero-coupling shape).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

/// Plants the purge tombstone on the HANDED delivery connection, so it commits with the
/// subscription checkpoint, and does no other work: nothing bounds a player's inbox, while
/// `ASYNCEVENTS_HANDLER_TIMEOUT` bounds this handler at 10s, so deleting it here could make
/// no progress and burn failures toward the plane's pause threshold. The deleting belongs to
/// the scheduled sweep, which resumes across fires; this row is that sweep's work item and,
/// once swept, the permanent record that this player is gone — redelivery is
/// `ON CONFLICT DO NOTHING`, never a reset of `swept_at`.
pub(crate) fn on_player_deleted<'a>(
    svc: Arc<Service>,
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

pub(crate) struct PruneHandler {
    pub(crate) retention_days: i32,
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
                let (deleted, high): (i64, Option<String>) = sqlx::query_as(
                    "WITH stale AS ( \
                       SELECT ctid FROM notifications.messages \
                        WHERE created_at < now() - make_interval(days => $1) \
                          AND created_at >= $3::timestamptz \
                        ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED \
                     ), del AS ( \
                       DELETE FROM notifications.messages m USING stale \
                        WHERE m.ctid = stale.ctid RETURNING m.created_at \
                     ) \
                     SELECT count(*)::bigint, max(created_at)::text FROM del",
                )
                .bind(self.retention_days)
                .bind(PRUNE_BATCH)
                .bind(&watermark)
                .fetch_one(&mut *conn)
                .await
                .map_err(BusError::transport)?;
                if deleted < PRUNE_BATCH {
                    return Ok(());
                }
                // `>=`, never `>`: rows sharing the batch's highest `created_at` may still be
                // pending, and they are already deleted, so re-scanning them costs one batch.
                if let Some(high) = high {
                    watermark = high;
                }
                if started.elapsed() >= SWEEP_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "notifications: retention sweep hit its budget — resuming next fire"
                    );
                    return Ok(());
                }
            }
        })
    }
}

/// The purge sweep's consumer-owned subscription id. Separate from [`PRUNE_SUB`] although
/// both ride `scheduler.fired`: two checkpoints, so a paused retention prune never stalls a
/// deletion and a paused deletion never stalls retention. `Genesis` because the sweep must
/// run on a boot that has never subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Tombstones claimed per selection statement. Unlike [`PRUNE_BATCH`] this bounds PLAYERS,
/// not rows — the per-player delete is one statement whose size nothing here caps — and the
/// sweep LOOPS batches until a short one, so a backlog above one batch per day still drains.
pub(crate) const PURGE_BATCH: i64 = 256;

/// The scan floor carried across batches, load-bearing rather than an optimisation: every
/// batch runs inside the ONE still-open delivery transaction, where the tombstones this
/// transaction has already stamped are dead-but-unprunable row versions that a
/// `swept_at IS NULL` re-scan still walks — so a floorless scan re-walks `256 x (k-1)` of
/// them and the loop goes quadratic. It buys that only while the batch query's `ORDER BY`
/// names the TABLE's `player_id`: unqualified it resolves to the `player_id::text` output
/// column, which the primary key can supply as neither the ordering nor an index condition,
/// leaving a seq scan plus a sort that spills to disk inside that transaction. Ordering and
/// filtering on the same `uuid` column also drops an unstated dependency on canonical uuid
/// text collating in uuid order. The comparison is `>=` rather than the `>` the unique key
/// would allow: it costs one already-stamped row per batch (`swept_at IS NULL` filters it
/// out) and in exchange keeps the floor value itself reachable.
const PURGE_FLOOR: &str = "00000000-0000-0000-0000-000000000000";

/// Drains the tombstones planted by [`on_player_deleted`]: the inbox rows of players whose
/// account is gone. Runs on the handed delivery tx, so every delete, every `swept_at` stamp
/// and the subscription checkpoint commit together.
pub(crate) struct PurgeSweepHandler;

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
            let started = std::time::Instant::now();
            let mut watermark = PURGE_FLOOR.to_string();
            loop {
                let due: Vec<(String,)> = sqlx::query_as(
                    "SELECT player_id::text FROM notifications.purged_players \
                      WHERE swept_at IS NULL AND player_id >= $1::uuid \
                      ORDER BY notifications.purged_players.player_id \
                      LIMIT $2 FOR UPDATE SKIP LOCKED",
                )
                .bind(&watermark)
                .bind(PURGE_BATCH)
                .fetch_all(&mut *conn)
                .await
                .map_err(BusError::transport)?;
                for (player_id,) in &due {
                    sqlx::query("DELETE FROM notifications.messages WHERE player_id = $1::uuid")
                        .bind(player_id)
                        .execute(&mut *conn)
                        .await
                        .map_err(BusError::transport)?;
                    sqlx::query(
                        "UPDATE notifications.purged_players SET swept_at = now() \
                          WHERE player_id = $1::uuid",
                    )
                    .bind(player_id)
                    .execute(&mut *conn)
                    .await
                    .map_err(BusError::transport)?;
                    // The budget is spent only at a player boundary — after the stamp, never
                    // between a player's delete and its stamp. The sweeps that emit a
                    // terminal event per deleted row depend on that placement; this one keeps
                    // it so all six read the same.
                    if started.elapsed() >= SWEEP_BUDGET {
                        tracing::warn!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "notifications: purge sweep hit its budget — resuming next fire"
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
        })
    }
}

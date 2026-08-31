use std::sync::Arc;

use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::service::{is_uuid_text, NewNotification, Service};

pub(crate) const KIND_WALLET_CREDIT: &str = "wallet.credit";
pub(crate) const KIND_ACCOUNT_PROMOTED: &str = "account.promoted";

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

pub(crate) const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "notifications.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::NOTIFICATIONS_PRUNE;

pub(crate) const DEFAULT_RETENTION_DAYS: i32 = 30;

pub(crate) const RETENTION_ENV: &str = "NOTIFICATIONS_RETENTION_DAYS";

/// Rows per statement. The sweep LOOPS these until a short batch, so the batching bounds
/// each statement without capping the fire: a cap would leave retention permanently behind
/// any inflow above one batch per day, since this schedule fires once every 86400s.
pub(crate) const PRUNE_BATCH: i64 = 256;

/// The whole sweep's wall-clock budget, comfortably under the default 10s
/// `ASYNCEVENTS_HANDLER_TIMEOUT`. Exhausting it ends the fire with the batches so far
/// KEPT (they commit with the checkpoint); a handler that instead ran into the timeout
/// would have its whole delivery rolled back to the plane's savepoint and make no
/// progress at all, on every retry, until the subscription paused.
pub(crate) const PRUNE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

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
        Err(_) => return Ok(DEFAULT_RETENTION_DAYS),
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
/// the canonical hyphenated spelling is delivered, because a 22P02 aborts the plane's
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

/// Just the `name` of a `scheduler.fired` payload: the prune subscribes by the contract's
/// topic const without importing the producer's payload type (audit's zero-coupling shape).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
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
            loop {
                let deleted = sqlx::query(
                    "WITH stale AS ( \
                       SELECT ctid FROM notifications.messages \
                        WHERE created_at < now() - make_interval(days => $1) \
                        ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED \
                     ) \
                     DELETE FROM notifications.messages m USING stale WHERE m.ctid = stale.ctid",
                )
                .bind(self.retention_days)
                .bind(PRUNE_BATCH)
                .execute(&mut *conn)
                .await
                .map_err(BusError::transport)?
                .rows_affected();
                if deleted < PRUNE_BATCH as u64 {
                    return Ok(());
                }
                if started.elapsed() >= PRUNE_BUDGET {
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

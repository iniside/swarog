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

/// One fire deletes at most this many rows, so the sweep cannot outgrow
/// `ASYNCEVENTS_HANDLER_TIMEOUT` on a large inbox: a timed-out delete makes no partial
/// progress, so an unbounded one would time out identically on every retry until the
/// subscription pauses and retention stops running altogether.
pub(crate) const PRUNE_BATCH: i64 = 256;

/// A PRESENT but unusable value fails startup rather than silently falling back — the
/// `ASYNCEVENTS_HANDLER_TIMEOUT` convention. An operator writing `90d` means 90 days, and
/// booting at the compiled 30 would prune inboxes a third of the way in with nobody told.
/// Only unset (or empty) is the default.
pub(crate) fn retention_days_from_env() -> anyhow::Result<i32> {
    let raw = match std::env::var(RETENTION_ENV) {
        Ok(v) => v,
        Err(_) => return Ok(DEFAULT_RETENTION_DAYS),
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DEFAULT_RETENTION_DAYS);
    }
    let days: i32 = raw.parse().map_err(|_| {
        anyhow::anyhow!(
            "notifications: {RETENTION_ENV} must be a whole number of days (got {raw:?}); \
             unset it for the default {DEFAULT_RETENTION_DAYS}"
        )
    })?;
    if days <= 0 {
        anyhow::bail!(
            "notifications: {RETENTION_ENV} must be > 0 (got {days}) — a non-positive \
             retention would delete every inbox; unset it for the default {DEFAULT_RETENTION_DAYS}"
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

/// A `player_id` the `$n::uuid` cast would reject must be caught BEFORE the insert runs: a
/// 22P02 aborts the plane's delivery transaction, and the checkpoint `UPDATE` that follows an
/// `Ok(())` would then fail with 25P02 — the rejection has to cost no statement at all. So
/// `Service::deliver_on`'s own 22P02 arm is unreachable from here.
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
            sqlx::query(
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
            .map_err(BusError::transport)?;
            Ok(())
        })
    }
}

use std::sync::{Arc, OnceLock};

use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use prometheus::IntCounter;
use sqlx::PgConnection;

use crate::service::{NewMail, Service};
use crate::store::Enqueued;

/// The consumer-owned subscription id — a durable contract; renaming it abandons the
/// checkpoint. `Genesis` is safe because the topic is born with this subscription (no
/// retained history can predate it) and it covers a request appended between the
/// contract's registration and the first subscribe.
pub(crate) const SEND_REQUESTED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "mail.send-requested.v1",
    start: bus::StartPosition::Genesis,
};

/// Requests refused on data quality. Every rejection answers `Ok(())` and advances the
/// checkpoint, so no readiness check ever reflects them — this counter is the only place a
/// producer emitting unusable requests becomes visible.
pub(crate) fn enqueue_rejected() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "mail_enqueue_rejected_total",
            "Durable mail.send_requested events refused on data quality (no outbox row).",
        )
        .expect("valid mail enqueue_rejected counter");
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

/// Requests whose `idempotency_key` already held a DIFFERENT message: a producer bug, and
/// a message that was never enqueued.
pub(crate) fn enqueue_conflicts() -> &'static IntCounter {
    static C: OnceLock<IntCounter> = OnceLock::new();
    C.get_or_init(|| {
        let c = IntCounter::new(
            "mail_enqueue_conflicts_total",
            "Durable mail.send_requested events whose idempotency_key already held a \
             different message (no outbox row).",
        )
        .expect("valid mail enqueue_conflicts counter");
        let _ = metrics::register(Box::new(c.clone()));
        c
    })
}

/// Both refusals answer `Ok(())`: an `Err` backs the subscription off and eventually
/// PAUSES it, taking the whole outbound channel down over one bad payload or one
/// producer's reused key. Only infrastructure propagates.
pub(crate) async fn enqueue_or_skip(
    svc: &Service,
    conn: &mut PgConnection,
    m: &NewMail<'_>,
) -> Result<(), BusError> {
    match svc.enqueue_on(conn, m).await {
        Ok(Enqueued::Inserted(id)) => {
            tracing::debug!(mail_id = %id, kind = m.kind, "mail: request enqueued");
            Ok(())
        }
        Ok(Enqueued::Duplicate) => Ok(()),
        Ok(Enqueued::Conflict) => {
            enqueue_conflicts().inc();
            tracing::warn!(
                kind = m.kind,
                "mail: idempotency_key already holds a different message — request NOT \
                 enqueued"
            );
            Ok(())
        }
        Err(e) if e.status == opsapi::Status::Invalid => {
            enqueue_rejected().inc();
            tracing::warn!(
                kind = m.kind,
                reason = %e.msg,
                "mail: durable request rejected on data quality — no outbox row"
            );
            Ok(())
        }
        Err(e) => Err(BusError::transport(e)),
    }
}

pub(crate) fn on_send_requested<'a>(
    svc: Arc<Service>,
    mut delivery: Delivery<'a>,
    e: mailevents::SendRequested,
) -> BoxFuture<'a, Result<(), BusError>> {
    Box::pin(async move {
        let m = NewMail {
            idempotency_key: &e.idempotency_key,
            recipient: &e.to,
            subject: &e.subject,
            body: &e.body,
            kind: &e.kind,
        };
        let conn = delivery.tx.downcast::<PgConnection>()?;
        enqueue_or_skip(&svc, conn, &m).await
    })
}

/// The retention sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. `Genesis` because the sweep must run on a boot that has never
/// subscribed before, the same reasoning as `notifications.prune-on-scheduler.v1`.
pub(crate) const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "mail.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::MAIL_PRUNE;

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
///
/// It is a bare constant because a module may not read the plane's env and `bus::Delivery`
/// carries no deadline to derive one from; the operator constraint that comes with it is
/// that `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s (its default is 10s, and nothing
/// in the tree sets it). Below that the sweep is killed rather than budgeted: the plane
/// `pg_terminate_backend`s the delivery backend, so the transaction dies with it and EVERY
/// batch of that fire is lost — no savepoint rollback is involved — and 20 such fires
/// (`PAUSE_AFTER`, i.e. 20 days on a daily schedule) pause the subscription.
pub(crate) const PRUNE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

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
            // The scan floor, carried across batches as text because the workspace's sqlx
            // has no date/time feature; `-infinity` is the first batch's "no floor".
            let mut watermark = "-infinity".to_string();
            loop {
                let (deleted, high): (i64, Option<String>) = sqlx::query_as(
                    "WITH stale AS ( \
                       SELECT ctid FROM mail.outbox \
                        WHERE state IN ('sent','cancelled') \
                          AND created_at < now() - make_interval(days => $1) \
                          AND created_at >= $3::timestamptz \
                        ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED \
                     ), del AS ( \
                       DELETE FROM mail.outbox m USING stale \
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
                // `>=`, never `>`: rows sharing the batch's highest `created_at` may not all
                // have fit in this batch; the ones that did are already deleted, so
                // re-scanning the tie costs one batch, never a quadratic re-walk.
                if let Some(high) = high {
                    watermark = high;
                }
                if started.elapsed() >= PRUNE_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "mail: retention sweep hit its budget — resuming next fire"
                    );
                    return Ok(());
                }
            }
        })
    }
}

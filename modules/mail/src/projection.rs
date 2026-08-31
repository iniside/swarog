use std::sync::{Arc, OnceLock};

use bus::{Delivery, Error as BusError};
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

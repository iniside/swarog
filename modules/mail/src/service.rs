use opsapi::Error;
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres};

use crate::address::parse_address;
use crate::store::{
    cap_from_db_error, Enqueued, OutboxListRow, OutboxStats, Store, BODY, IDEMPOTENCY_KEY, KIND,
    SUBJECT,
};

/// One request to enqueue, borrowed from whichever caller raised it — a durable payload or
/// an operator form.
pub(crate) struct NewMail<'a> {
    pub(crate) idempotency_key: &'a str,
    pub(crate) recipient: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) body: &'a str,
    pub(crate) kind: &'a str,
}

/// THE input policy, enforced INSIDE the enqueue authority so no caller can route around
/// it, and run BEFORE any statement of the caller's transaction.
///
/// The ordering is what keeps a bad payload from stalling the plane. A rejection raised
/// AFTER a failed statement leaves the delivery transaction aborted, so the plane's
/// checkpoint `UPDATE` — which runs on the handler's `Ok(())` arm, BEFORE anything records
/// a failure (`core/asyncevents/src/worker.rs`) — fails with 25P02 and propagates through
/// `?`. Nothing then records a failure, so the subscription takes no backoff and never
/// pauses: it re-delivers the same event on every pass forever, making no progress, while
/// `/readyz` stays green because the passes themselves keep completing.
///
/// The byte caps are the column CHECKs' Rust twins (`store::COLUMN_CAPS`); without them an
/// over-long field is a 23514 nothing maps, which reaches the durable handler as
/// infrastructure trouble and pauses the subscription.
pub(crate) fn validate_new(m: &NewMail<'_>) -> Result<(), Error> {
    if m.idempotency_key.trim().is_empty() {
        return Err(Error::invalid("mail: idempotency_key is required"));
    }
    IDEMPOTENCY_KEY.check(m.idempotency_key)?;
    parse_address(m.recipient)
        .map_err(|reason| Error::invalid(format!("mail: recipient {reason}")))?;
    SUBJECT.check(m.subject)?;
    // A subject is a message HEADER too, so it carries the recipient's injection rule.
    if m.subject.chars().any(char::is_control) {
        return Err(Error::invalid(
            "mail: subject must not contain control characters",
        ));
    }
    BODY.check(m.body)?;
    if m.kind.trim().is_empty() {
        return Err(Error::invalid("mail: kind is required"));
    }
    KIND.check(m.kind)?;
    Ok(())
}

pub struct Service {
    pub(crate) store: Store,
    /// The operator page's own connection source. The durable ingress never touches it —
    /// that path runs on the delivery transaction it is handed — so this pool serves only
    /// the pool-owned reads and writes of the admin surface.
    pool: PgPool,
}

impl Service {
    pub fn new(pool: PgPool) -> Service {
        Service { store: Store, pool }
    }

    /// A pool checkout under the drain's own bound. Unbounded here would be worse than a
    /// slow page: the LOCAL render runs inside `block_in_place`, so a checkout that never
    /// completes pins a runtime worker thread for as long as the operator's request lives.
    async fn conn(&self) -> Result<PoolConnection<Postgres>, Error> {
        tokio::time::timeout(crate::worker::ACQUIRE_DEADLINE, self.pool.acquire())
            .await
            .map_err(|_| {
                Error::internal(format!(
                    "mail: pool checkout timed out after {}s",
                    crate::worker::ACQUIRE_DEADLINE.as_secs()
                ))
            })?
            .map_err(crate::internal)
    }

    /// The single enqueue authority. It runs on a CALLER-OWNED connection and never begins,
    /// commits or rolls back, so the durable handler's row and its subscription checkpoint
    /// commit together.
    ///
    /// The two error classes are DISTINGUISHED by status and a durable caller must treat
    /// them differently: `Status::Invalid` is one message's data quality — refused before
    /// any statement ran — and the handler answers `Ok(())`; anything else is
    /// infrastructure and must propagate so the plane retries.
    pub(crate) async fn enqueue_on(
        &self,
        conn: &mut PgConnection,
        m: &NewMail<'_>,
    ) -> Result<Enqueued, Error> {
        validate_new(m)?;
        self.store.enqueue_tx(conn, m).await.map_err(|e| {
            if let Some(cap) = cap_from_db_error(&e) {
                tracing::error!(
                    constraint = cap.constraint,
                    max_bytes = cap.max_bytes,
                    "mail: the {} column CHECK rejected a value validate_new should have \
                     refused first",
                    cap.what
                );
            }
            crate::internal(e)
        })
    }
}

/// The operator surface's data access. Every method runs on a pool connection and returns
/// [`Error`]: `Status::Internal` is infrastructure, anything else is the operator's input —
/// the split `crate::admin` maps onto its own verdicts.
impl Service {
    pub(crate) async fn outbox_stats(&self) -> Result<OutboxStats, Error> {
        let mut conn = self.conn().await?;
        self.store
            .stats_tx(&mut conn)
            .await
            .map_err(crate::internal)
    }

    pub(crate) async fn recent_outbox(
        &self,
        state: Option<&str>,
        limit: i64,
    ) -> Result<Vec<OutboxListRow>, Error> {
        let mut conn = self.conn().await?;
        self.store
            .recent_tx(&mut conn, state, limit)
            .await
            .map_err(crate::internal)
    }

    /// Rows moved — `0` means the row was not parked when the statement ran, which the
    /// caller reports as a stale form rather than as success.
    pub(crate) async fn requeue_parked(&self, id: &str) -> Result<u64, Error> {
        let mut conn = self.conn().await?;
        self.store
            .requeue_parked_tx(&mut conn, id)
            .await
            .map_err(crate::internal)
    }

    pub(crate) async fn requeue_all_parked(&self, limit: i64) -> Result<u64, Error> {
        let mut conn = self.conn().await?;
        self.store
            .requeue_all_parked_tx(&mut conn, limit)
            .await
            .map_err(crate::internal)
    }

    pub(crate) async fn cancel_pending(&self, id: &str) -> Result<u64, Error> {
        let mut conn = self.conn().await?;
        self.store
            .cancel_pending_tx(&mut conn, id)
            .await
            .map_err(crate::internal)
    }

    /// An operator's test message through the SAME [`Service::enqueue_on`] authority, run
    /// on a pool connection: the admin form cannot acquire an input rule — or an
    /// idempotency semantic — the durable ingress does not have.
    pub(crate) async fn enqueue_from_admin(&self, m: &NewMail<'_>) -> Result<Enqueued, Error> {
        let mut conn = self.conn().await?;
        self.enqueue_on(&mut conn, m).await
    }
}

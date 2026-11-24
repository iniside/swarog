use std::time::Duration;

use opsapi::Error;
use sqlx::{PgConnection, PgPool};

use crate::worker::bounded_tx;

use crate::address::parse_address;
use crate::store::{
    cap_from_db_error, Enqueued, OutboxListRow, OutboxListing, OutboxStats, Store, BODY,
    IDEMPOTENCY_KEY, KIND, SUBJECT,
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

/// What one operator-page statement may take. Deliberately short: the page's reads are one
/// statement of counts and one 26-row listing, so anything longer is a wedged statement.
const ADMIN_BUDGET: Duration = Duration::from_secs(5);

/// The operator test-send key space: this prefix followed by the portal's `request_id`.
/// `mail.send_requested.idempotency_key` is a free-form producer-chosen string, so a
/// producer COULD emit this prefix; [`Service::enqueue_from_admin`] therefore refuses any
/// key outside the shape rather than trusting the space to be reserved.
pub(crate) const TEST_KEY_PREFIX: &str = "admin-send-test-";

/// Whether `key` has the operator test-send shape: [`TEST_KEY_PREFIX`], then a non-empty
/// run of printable ASCII, within the column's byte cap. The rule lives beside the enqueue
/// authority that enforces it, not beside the provider that builds the key.
pub(crate) fn is_test_key(key: &str) -> bool {
    match key.strip_prefix(TEST_KEY_PREFIX) {
        Some(request_id) => {
            !request_id.is_empty()
                && key.len() <= IDEMPOTENCY_KEY.max_bytes
                && request_id.bytes().all(|b| b.is_ascii_graphic())
        }
        None => false,
    }
}

/// The operator surface's data access: each call is one bounded transaction on the pool.
///
/// [`bounded_tx`] is the module's ONE checkout-and-statement bound, the same helper the
/// drain uses.
impl Service {
    pub(crate) async fn outbox_stats(&self) -> Result<OutboxStats, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let stats = self
            .store
            .stats_tx(&mut tx)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(stats)
    }

    /// The counts and one listing page, in one bounded transaction.
    pub(crate) async fn outbox_page(
        &self,
        listing: &OutboxListing<'_>,
    ) -> Result<(OutboxStats, Vec<OutboxListRow>), Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let stats = self
            .store
            .stats_tx(&mut tx)
            .await
            .map_err(crate::internal)?;
        let rows = self
            .store
            .list_tx(&mut tx, listing)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok((stats, rows))
    }

    pub(crate) async fn outbox_summary(&self, id: &str) -> Result<Option<(String, String)>, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let summary = self
            .store
            .summary_tx(&mut tx, id)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(summary)
    }

    /// Rows moved — `0` means the row was not parked when the statement ran, which the
    /// caller reports as nothing done rather than as success.
    pub(crate) async fn requeue_parked(&self, id: &str) -> Result<u64, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let moved = self
            .store
            .requeue_parked_tx(&mut tx, id)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(moved)
    }

    /// `(moved, still_parked)` — both read in the one transaction, so the operator is
    /// never shown a pair from two different moments.
    #[expect(
        dead_code,
        reason = "G11 (a): requeue-all has no mockup element; kept as the mass-park recovery path, \
                  docs/plans/2026-11-17-step11-mail-checklist.md:375"
    )]
    pub(crate) async fn requeue_all_parked(&self, limit: i64) -> Result<(i64, i64), Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let counts = self
            .store
            .requeue_all_parked_tx(&mut tx, limit)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(counts)
    }

    pub(crate) async fn cancel_pending(&self, id: &str) -> Result<u64, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let cancelled = self
            .store
            .cancel_pending_tx(&mut tx, id)
            .await
            .map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(cancelled)
    }

    /// Rows made due now — `0` means the row was not retrying (moved, or never failed).
    pub(crate) async fn retry_now(&self, id: &str) -> Result<u64, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET).await.map_err(crate::internal)?;
        let moved = self.store.retry_now_tx(&mut tx, id).await.map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(moved)
    }

    /// Rows parked — `0` means the row was not retrying (moved, resumed, or never failed).
    pub(crate) async fn park(&self, id: &str) -> Result<u64, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET).await.map_err(crate::internal)?;
        let parked = self.store.park_tx(&mut tx, id).await.map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(parked)
    }

    /// Rows cancelled — `0` means the row was not parked when the statement ran.
    pub(crate) async fn cancel_parked(&self, id: &str) -> Result<u64, Error> {
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET).await.map_err(crate::internal)?;
        let cancelled = self.store.cancel_parked_tx(&mut tx, id).await.map_err(crate::internal)?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(cancelled)
    }

    /// An operator's test message through the SAME [`Service::enqueue_on`] authority, run
    /// on a pool transaction: the admin form cannot acquire an input rule — or an
    /// idempotency semantic — the durable ingress does not have.
    ///
    /// The key SHAPE is checked HERE, before the checkout: this is the only entry point
    /// that writes an operator key, so a caller that invented one is refused by the
    /// authority rather than by whichever caller remembered to look.
    pub(crate) async fn enqueue_from_admin(&self, m: &NewMail<'_>) -> Result<Enqueued, Error> {
        if !is_test_key(m.idempotency_key) {
            return Err(Error::invalid(
                "mail: an operator send must carry an admin-send-test- key within the key cap",
            ));
        }
        let mut tx = bounded_tx(&self.pool, ADMIN_BUDGET)
            .await
            .map_err(crate::internal)?;
        let enqueued = self.enqueue_on(&mut tx, m).await?;
        tx.commit().await.map_err(crate::internal)?;
        Ok(enqueued)
    }
}

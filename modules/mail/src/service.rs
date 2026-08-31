use opsapi::Error;
use sqlx::PgConnection;

use crate::address::parse_address;
use crate::store::{cap_from_db_error, Enqueued, Store, BODY, IDEMPOTENCY_KEY, KIND, SUBJECT};

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
}

impl Default for Service {
    fn default() -> Self {
        Service::new()
    }
}

impl Service {
    pub fn new() -> Service {
        Service { store: Store }
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

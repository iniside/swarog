use opsapi::Error;
use sqlx::{PgConnection, PgPool};

use crate::store::{
    cap_from_db_error, Enqueued, Store, BODY, IDEMPOTENCY_KEY, KIND, RECIPIENT, SUBJECT,
};

/// One request to enqueue, borrowed from whichever caller raised it (a durable payload, or
/// from Step 6 an operator form).
pub(crate) struct NewMail<'a> {
    pub(crate) idempotency_key: &'a str,
    pub(crate) recipient: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) body: &'a str,
    pub(crate) kind: &'a str,
}

/// THE input policy, enforced INSIDE the enqueue authority so no caller can route around
/// it, and run BEFORE any statement of the caller's transaction: a rejection raised after
/// a failed statement would leave the delivery transaction aborted, and the plane's
/// checkpoint `UPDATE` on the `Ok(())` that follows would then fail with 25P02 — aborting
/// the whole worker pass and starving every other subscription on the plane.
///
/// The byte caps are the column CHECKs' Rust twins ([`crate::store::COLUMN_CAPS`]);
/// without them an over-long field is a 23514 nothing maps, which reaches the durable
/// handler as infrastructure trouble and pauses the subscription.
pub(crate) fn validate_new(m: &NewMail<'_>) -> Result<(), Error> {
    if m.idempotency_key.trim().is_empty() {
        return Err(Error::invalid("mail: idempotency_key is required"));
    }
    IDEMPOTENCY_KEY.check(m.idempotency_key)?;
    if m.recipient.trim().is_empty() {
        return Err(Error::invalid("mail: recipient is required"));
    }
    RECIPIENT.check(m.recipient)?;
    // CR/LF (and every other control character) in a recipient or a subject is header
    // injection: both go into the message HEADER, where a newline starts a header the
    // sender never wrote. This is the only gate between a producer's payload and a relay.
    if has_control(m.recipient) {
        return Err(Error::invalid(
            "mail: recipient must not contain control characters",
        ));
    }
    SUBJECT.check(m.subject)?;
    if has_control(m.subject) {
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

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

pub struct Service {
    pub(crate) store: Store,
}

impl Service {
    pub fn new(pool: PgPool) -> Service {
        Service {
            store: Store { pool },
        }
    }

    /// The single enqueue authority. It runs on a CALLER-OWNED connection and never
    /// begins, commits or rolls back, so the durable handler's row and its subscription
    /// checkpoint commit together.
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

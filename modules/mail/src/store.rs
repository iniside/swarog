use sqlx::PgConnection;

use mailevents::{
    MAX_ADDRESS_BYTES, MAX_BODY_BYTES, MAX_IDEMPOTENCY_KEY_BYTES, MAX_KIND_BYTES,
    MAX_SUBJECT_BYTES,
};
use opsapi::Error;

use crate::service::NewMail;

/// The delivered state — one of the four `mail_outbox_state_check` names.
pub(crate) const STATE_SENT: &str = "sent";

/// One text column's byte ceiling and the named column CHECK that backstops it. ONE table
/// feeds the Rust pre-check and the 23514 lookup, so the two cannot word one limit two
/// ways, and the pairing is checkable against `SCHEMA_DDL` instead of resting on two lists
/// kept in step by hand.
pub(crate) struct ColumnCap {
    pub(crate) what: &'static str,
    pub(crate) max_bytes: usize,
    pub(crate) constraint: &'static str,
}

pub(crate) const RECIPIENT: ColumnCap = ColumnCap {
    what: "recipient",
    max_bytes: MAX_ADDRESS_BYTES,
    constraint: "mail_outbox_recipient_len_check",
};
pub(crate) const SUBJECT: ColumnCap = ColumnCap {
    what: "subject",
    max_bytes: MAX_SUBJECT_BYTES,
    constraint: "mail_outbox_subject_len_check",
};
pub(crate) const BODY: ColumnCap = ColumnCap {
    what: "body",
    max_bytes: MAX_BODY_BYTES,
    constraint: "mail_outbox_body_len_check",
};
pub(crate) const KIND: ColumnCap = ColumnCap {
    what: "kind",
    max_bytes: MAX_KIND_BYTES,
    constraint: "mail_outbox_kind_len_check",
};
pub(crate) const IDEMPOTENCY_KEY: ColumnCap = ColumnCap {
    what: "idempotency_key",
    max_bytes: MAX_IDEMPOTENCY_KEY_BYTES,
    constraint: "mail_outbox_key_len_check",
};

pub(crate) const COLUMN_CAPS: &[&ColumnCap] =
    &[&RECIPIENT, &SUBJECT, &BODY, &KIND, &IDEMPOTENCY_KEY];

impl ColumnCap {
    /// `Status::Invalid` — one message's data quality, which the durable handler answers
    /// `Ok(())` to rather than pausing its subscription.
    pub(crate) fn check(&self, value: &str) -> Result<(), Error> {
        if value.len() > self.max_bytes {
            return Err(Error::invalid(format!(
                "mail: {} exceeds the {}-byte cap",
                self.what, self.max_bytes
            )));
        }
        Ok(())
    }
}

/// The `_len_check` a failed statement names, if any: the class fail-safe firing means a
/// Rust cap did not refuse the value first. Resolving it only NAMES the broken pairing —
/// the error stays infrastructure, because a CHECK has already aborted the caller's
/// transaction.
pub(crate) fn cap_from_db_error(e: &sqlx::Error) -> Option<&'static ColumnCap> {
    let db = e.as_database_error()?;
    if db.code().as_deref() != Some("23514") {
        return None;
    }
    let constraint = db.constraint()?;
    COLUMN_CAPS
        .iter()
        .copied()
        .find(|cap| cap.constraint == constraint)
}

/// What the unique `idempotency_key` did with one enqueue. `Conflict` exists because a
/// bare `ON CONFLICT DO NOTHING` would discard a corrected message and report success.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Enqueued {
    Inserted(String),
    Duplicate,
    Conflict,
}

pub(crate) struct ExistingMail {
    pub(crate) recipient: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) kind: String,
    pub(crate) state: String,
}

/// Whether `body` still holds what the request asked to send, and so takes part in the
/// duplicate-vs-conflict comparison.
///
/// This is a REQUIREMENT ON EVERY WRITER, not a description of one: `sent` is the only
/// state whose `body` may be blanked. Blanking it in a second state without extending this
/// predicate makes a durable replay of that request compare `"" != body`, answer
/// `Conflict`, and drop the message while telling the operator their own row was a
/// producer bug.
fn body_is_comparable(state: &str) -> bool {
    state != STATE_SENT
}

/// Identical message under an existing key is a replay; anything else is a key reused for
/// a different message. Zero I/O, so the verdict is provable without a DB.
pub(crate) fn classify_existing(m: &NewMail<'_>, existing: &ExistingMail) -> Enqueued {
    let same = m.recipient == existing.recipient
        && m.subject == existing.subject
        && m.kind == existing.kind
        && (!body_is_comparable(&existing.state) || m.body == existing.body);
    if same {
        Enqueued::Duplicate
    } else {
        Enqueued::Conflict
    }
}

/// A correlation token for an `idempotency_key`. The key is producer-chosen and may embed
/// a token, so it never reaches a log line verbatim. `DefaultHasher` is seeded with fixed
/// keys, so the token is stable across processes and restarts.
fn key_digest(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Every write takes `&mut PgConnection`, never a pool, so one implementation serves any
/// caller-owned transaction — the event plane's handed delivery transaction and a
/// pool-owned one alike.
pub(crate) struct Store;

impl Store {
    /// `RETURNING id` on a `DO NOTHING` insert is the claim signal — no row means another
    /// message already owns this key — and the re-read runs on the SAME connection, so it
    /// sees the caller's own uncommitted rows (a request replayed twice inside one
    /// transaction) as well as committed ones.
    pub(crate) async fn enqueue_tx(
        &self,
        conn: &mut PgConnection,
        m: &NewMail<'_>,
    ) -> Result<Enqueued, sqlx::Error> {
        let inserted: Option<(String,)> = sqlx::query_as(
            "INSERT INTO mail.outbox (idempotency_key, recipient, subject, body, kind) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (idempotency_key) DO NOTHING \
             RETURNING id::text",
        )
        .bind(m.idempotency_key)
        .bind(m.recipient)
        .bind(m.subject)
        .bind(m.body)
        .bind(m.kind)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((id,)) = inserted {
            return Ok(Enqueued::Inserted(id));
        }
        match self.existing_tx(conn, m.idempotency_key).await? {
            Some(existing) => Ok(classify_existing(m, &existing)),
            // The winning row was pruned between the conflicting insert and this read —
            // an explicit arm rather than an `unwrap`, so a changed isolation level or a
            // new delete path is visible.
            None => {
                tracing::warn!(
                    key_digest = key_digest(m.idempotency_key),
                    "mail: the row holding this key vanished between the insert and the \
                     re-read — treating the request as already delivered"
                );
                Ok(Enqueued::Duplicate)
            }
        }
    }

    async fn existing_tx(
        &self,
        conn: &mut PgConnection,
        idempotency_key: &str,
    ) -> Result<Option<ExistingMail>, sqlx::Error> {
        let row: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT recipient, subject, body, kind, state FROM mail.outbox \
              WHERE idempotency_key = $1",
        )
        .bind(idempotency_key)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(
            row.map(|(recipient, subject, body, kind, state)| ExistingMail {
                recipient,
                subject,
                body,
                kind,
                state,
            }),
        )
    }
}

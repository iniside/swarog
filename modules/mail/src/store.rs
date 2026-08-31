use sqlx::{PgConnection, PgPool};

use mailevents::{
    MAX_ADDRESS_BYTES, MAX_BODY_BYTES, MAX_IDEMPOTENCY_KEY_BYTES, MAX_KIND_BYTES,
    MAX_SUBJECT_BYTES,
};
use opsapi::Error;

use crate::service::NewMail;

/// The delivered state, spelled once — one of the four `mail_outbox_state_check` names.
pub(crate) const STATE_SENT: &str = "sent";

/// One text column: the byte ceiling checked in Rust BEFORE the statement, and the named
/// column CHECK that backstops it. ONE table feeds both, so the Rust verdict and the DB's
/// cannot word one limit two ways, and the pairing is mechanically checkable against
/// `SCHEMA_DDL` rather than resting on two lists staying in step by hand.
///
/// The direction that matters here is the delivery transaction: an unmapped 23514 raised
/// inside it is an infrastructure `Err`, which pauses the whole subscription over one
/// over-long field.
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
    /// The over-cap verdict. `Status::Invalid` — one message's data quality, which the
    /// durable handler answers `Ok(())` to rather than pausing its subscription.
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

/// The `_len_check` a failed statement names, if any. The class fail-safe firing means a
/// Rust cap did NOT refuse the value first — the two-sided coupling is broken. It stays an
/// infrastructure error either way: a CHECK aborts the caller's transaction, so answering
/// the delivery arm `Ok(())` would then 25P02 the checkpoint `UPDATE`; resolving it only
/// names the cap in the log, where an operator staring at a paused subscription can see it.
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

/// What the unique `idempotency_key` did with one enqueue.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Enqueued {
    /// A new outbox row, carrying its id.
    Inserted(String),
    /// This key already holds an IDENTICAL message — the replay the key exists to absorb.
    Duplicate,
    /// This key already holds a DIFFERENT message: a producer bug. The posted message was
    /// NOT written, and saying so is the point — a bare `ON CONFLICT DO NOTHING` would
    /// discard a corrected message and report success.
    Conflict,
}

/// The row a duplicate key collides with, as the discrimination reads it.
pub(crate) struct ExistingMail {
    pub(crate) recipient: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) kind: String,
    pub(crate) state: String,
}

/// The states in which `body` still holds what the request asked to send, and so takes
/// part in the duplicate-vs-conflict comparison. `sent` is absent: a delivered row's body
/// is the one field the outbox is free to drop (it is rendered plaintext, often a
/// one-time secret), so comparing it would answer an operator re-drive of an
/// already-delivered request with a producer-bug verdict.
fn body_is_comparable(state: &str) -> bool {
    state != STATE_SENT
}

/// The PURE discrimination: identical message under an existing key is a replay, anything
/// else is a key reused for a different message. Zero I/O so it is provable without a DB.
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

/// Every write takes `&mut PgConnection`, never the pool, so one implementation serves the
/// event plane's HANDED delivery transaction and (from Step 6) an operator's pool-owned
/// transaction alike; reads use the pool.
pub(crate) struct Store {
    /// Held for the pool-owned paths (the drain's claim, the operator page); the enqueue
    /// authority deliberately runs on a caller-owned connection instead.
    #[allow(dead_code)]
    pub(crate) pool: PgPool,
}

impl Store {
    /// The enqueue authority: claim the key, then discriminate what happened.
    ///
    /// `RETURNING id` on a `DO NOTHING` insert is the claim signal — no row means another
    /// message already owns this key — and the re-read runs on the SAME connection, so
    /// under the delivery transaction it sees that transaction's own uncommitted rows
    /// (a request replayed twice inside one pass) as well as committed ones.
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
            // The winning row was pruned between the conflicting insert and this read
            // (retention deletes only long-delivered rows). Nothing is written and nothing
            // is lost that was not already delivered — an explicit arm rather than an
            // `unwrap`, so a changed isolation level or a new delete path is visible.
            None => {
                tracing::warn!(
                    idempotency_key = m.idempotency_key,
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

use sqlx::PgConnection;

use mailevents::{
    MAX_ADDRESS_BYTES, MAX_BODY_BYTES, MAX_IDEMPOTENCY_KEY_BYTES, MAX_KIND_BYTES,
    MAX_SUBJECT_BYTES,
};
use opsapi::Error;

use crate::service::NewMail;

/// The delivered state — one of the four `mail_outbox_state_check` names.
pub(crate) const STATE_SENT: &str = "sent";

/// Ceiling on the operator-visible `last_error`. A relay's response or an error chain is
/// unbounded; the column is not indexed and nobody reads past the first line.
pub(crate) const LAST_ERROR_MAX_BYTES: usize = 1_000;

/// Truncates on a CHAR boundary — slicing a `str` mid-UTF-8 panics, and a relay's
/// response is arbitrary bytes.
pub(crate) fn truncate_error(value: &str) -> String {
    if value.len() <= LAST_ERROR_MAX_BYTES {
        return value.to_string();
    }
    let mut end = LAST_ERROR_MAX_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

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

/// One outbox row a drain pass owns for the length of one send attempt.
pub(crate) struct Claimed {
    pub(crate) id: String,
    pub(crate) recipient: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) kind: String,
    /// The attempt count the claim WROTE. Every status write CASes on it, so a second
    /// claim of the same row (the lease expired while this attempt was in flight) makes
    /// the loser's write match zero rows instead of overwriting the winner's.
    pub(crate) attempts: i32,
}

/// The row write one send attempt implies. Computed by [`crate::worker::disposition`]
/// with zero I/O; [`Store::finish_tx`] is the only thing that turns it into SQL.
#[derive(Debug, PartialEq)]
pub(crate) enum Disposition {
    /// Delivered. `body` is BLANKED — a rendered body is a verification link or a reset
    /// token, and a `sent` row otherwise archives it for the whole retention window.
    /// `sent` is the ONLY state that may blank it: `store::body_is_comparable` drops
    /// `body` from the duplicate-vs-conflict comparison for exactly this state, so
    /// blanking any other would make a durable replay of that request compare `"" != body`
    /// and answer `Conflict` — dropping the message and reporting the operator's own row
    /// as a producer bug.
    Sent,
    /// Permanently undeliverable, or out of attempts. The body SURVIVES: the message
    /// still has to be sent once an operator fixes what parked it.
    Parked { last_error: String },
    /// Transient. Stays `pending`, due again after the backoff.
    Retry { last_error: String, backoff_secs: f64 },
}

/// The parked count and how overdue the earliest due row is, refreshed once per drain
/// pass. Both read through a partial index — see [`Store::gauges_tx`].
pub(crate) struct OutboxGauges {
    pub(crate) parked: i64,
    pub(crate) oldest_pending_overdue_secs: f64,
}

impl Store {
    /// Claims ONE due row, burning an attempt and pushing `next_attempt_at` out by
    /// `lease_secs`.
    ///
    /// There is deliberately no `sending` state: the claim is a COMMITTED update, so a
    /// process that dies mid-send leaves the row due again when the lease expires, at the
    /// cost of one burnt attempt — fail-closed toward parking rather than toward an
    /// unbounded resend loop. `FOR UPDATE SKIP LOCKED` makes replicas a consumer group by
    /// construction. ONE row per call so an exhausted pass budget never strands rows whose
    /// attempt it already burnt.
    pub(crate) async fn claim_due_tx(
        &self,
        conn: &mut PgConnection,
        lease_secs: f64,
    ) -> Result<Option<Claimed>, sqlx::Error> {
        let row: Option<(String, String, String, String, String, i32)> = sqlx::query_as(
            "WITH due AS ( \
                 SELECT id FROM mail.outbox \
                  WHERE state = 'pending' AND next_attempt_at <= now() \
                  ORDER BY next_attempt_at \
                  LIMIT 1 \
                  FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE mail.outbox m \
                SET attempts = m.attempts + 1, \
                    next_attempt_at = now() + make_interval(secs => $1), \
                    updated_at = now() \
               FROM due \
              WHERE m.id = due.id \
             RETURNING m.id::text, m.recipient, m.subject, m.body, m.kind, m.attempts",
        )
        .bind(lease_secs)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(
            row.map(|(id, recipient, subject, body, kind, attempts)| Claimed {
                id,
                recipient,
                subject,
                body,
                kind,
                attempts,
            }),
        )
    }

    /// Writes one attempt's outcome, CAS-guarded on `(id, state = 'pending', attempts)`.
    ///
    /// BOTH legs are load-bearing. The `state` leg is what stops an operator `cancel` that
    /// landed during the SMTP dialogue from being silently overwritten back to `sent`; the
    /// `attempts` leg is the ABA guard against a second claim of the same row (the lease
    /// expired mid-attempt) — `core/asyncevents/src/worker.rs`'s `record_failure` uses the
    /// same pairing for the same reason. Returns the rows matched: `0` is a CAS MISS, a
    /// named outcome the caller counts, never an error.
    pub(crate) async fn finish_tx(
        &self,
        conn: &mut PgConnection,
        row: &Claimed,
        disposition: &Disposition,
        provider: &str,
    ) -> Result<u64, sqlx::Error> {
        let query = match disposition {
            Disposition::Sent => sqlx::query(
                "UPDATE mail.outbox \
                    SET state = 'sent', sent_at = now(), provider = $3, last_error = NULL, \
                        body = '', updated_at = now() \
                  WHERE id = $1::uuid AND state = 'pending' AND attempts = $2",
            )
            .bind(&row.id)
            .bind(row.attempts)
            .bind(provider),
            Disposition::Parked { last_error } => sqlx::query(
                "UPDATE mail.outbox \
                    SET state = 'parked', provider = $3, last_error = $4, updated_at = now() \
                  WHERE id = $1::uuid AND state = 'pending' AND attempts = $2",
            )
            .bind(&row.id)
            .bind(row.attempts)
            .bind(provider)
            .bind(last_error),
            Disposition::Retry {
                last_error,
                backoff_secs,
            } => sqlx::query(
                "UPDATE mail.outbox \
                    SET next_attempt_at = now() + make_interval(secs => $3), provider = $4, \
                        last_error = $5, updated_at = now() \
                  WHERE id = $1::uuid AND state = 'pending' AND attempts = $2",
            )
            .bind(&row.id)
            .bind(row.attempts)
            .bind(backoff_secs)
            .bind(provider)
            .bind(last_error),
        };
        Ok(query.execute(&mut *conn).await?.rows_affected())
    }

    /// The two outbox gauges in ONE round-trip. `min(next_attempt_at)` (not
    /// `min(created_at)`) is what `mail_outbox_due_idx` covers, so this stays an index
    /// scan; the parked count rides `mail_outbox_parked_idx`. The age is clamped at zero
    /// because the earliest pending row may be leased or backed off into the future —
    /// what the gauge reports is how overdue the outbox's head is, and `0` means nothing
    /// is due.
    pub(crate) async fn gauges_tx(
        &self,
        conn: &mut PgConnection,
    ) -> Result<OutboxGauges, sqlx::Error> {
        let (parked, overdue): (i64, f64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM mail.outbox WHERE state = 'parked'), \
               (SELECT COALESCE(GREATEST(0, EXTRACT(EPOCH FROM (now() - min(next_attempt_at)))), 0)::float8 \
                  FROM mail.outbox WHERE state = 'pending')",
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok(OutboxGauges {
            parked,
            oldest_pending_overdue_secs: overdue,
        })
    }
}

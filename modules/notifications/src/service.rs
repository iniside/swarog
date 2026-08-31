use async_trait::async_trait;
use base64::Engine;
use notificationsapi::{
    Notification, Page, Player, DEFAULT_PAGE_LIMIT, MAX_BODY_BYTES, MAX_CURSOR_BYTES,
    MAX_KIND_BYTES, MAX_PAGE_LIMIT, MAX_TITLE_BYTES,
};
use opsapi::{Error, Identity};
use sqlx::{PgConnection, PgPool};

use crate::{internal, Store};

/// The ONE answer for a row that is absent AND for a row owned by somebody else. A 403
/// would confirm the id exists, which is an enumeration oracle over another player's inbox,
/// so ownership is a predicate in the statement and never a comparison afterwards.
pub(crate) const NOT_FOUND: &str = "notification not found";

pub(crate) const MALFORMED_CURSOR: &str = "cursor is malformed";

pub(crate) const MALFORMED_PLAYER_ID: &str = "player_id is not a valid uuid";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// `N` stands for one ascii digit; every other byte must match literally. The shape is the
/// one `store::COLS` renders.
const CURSOR_TIME_SHAPE: &str = "NNNN-NN-NNTNN:NN:NN.NNNNNNZ";

fn field(s: &str, from: usize, to: usize) -> u32 {
    s[from..to].parse().unwrap_or_default()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        _ => 0,
    }
}

/// The CODEC is the sole authority on what `list` will let reach `$2::timestamptz`, so the
/// calendar is checked HERE and the statement has no timestamp arm to map: a digit-SHAPED
/// but impossible instant (`2026-13-45`, `2026-02-30`, hour 25, year 0000) is `Status::Invalid`
/// like any other malformed cursor, where letting it through would raise 22008 in Postgres
/// and answer 500 — contradicting the contract's own promise on `Player::list`.
///
/// Accepting a value does NOT mean this module minted it (an attacker can encode any real
/// instant); it means the keyset is well-formed, which is all a keyset predicate needs.
fn is_cursor_time(s: &str) -> bool {
    let shaped = s.len() == CURSOR_TIME_SHAPE.len()
        && s.bytes()
            .zip(CURSOR_TIME_SHAPE.bytes())
            .all(|(c, p)| if p == b'N' { c.is_ascii_digit() } else { c == p });
    if !shaped {
        return false;
    }
    let (year, month, day) = (field(s, 0, 4), field(s, 5, 7), field(s, 8, 10));
    let (hour, minute, second) = (field(s, 11, 13), field(s, 14, 16), field(s, 17, 19));
    year >= 1
        && (1..=12).contains(&month)
        && day >= 1
        && day <= days_in_month(year, month)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

/// True iff `s` is the canonical hyphenated 36-character layout, hex digits case-insensitive.
pub(crate) fn is_uuid_text(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The dedup column's byte ceiling, enforced for BOTH write paths. It is a module const,
/// not a contract one: no wire field carries a dedup key — the durable half is minted by
/// the event plane and the operator half by `admin::mint_idempotency_key`. The value is
/// wallet's (`MAX_IDEMPOTENCY_KEY_BYTES`), far above the two real shapes (a 36-char
/// `event_id`, a 48-char operator key) and far below the ~2704-byte btree tuple limit that
/// `notifications_source_event_idx` would otherwise raise 54000 on.
pub(crate) const MAX_DEDUP_KEY_BYTES: usize = 128;

/// The prefix every operator dedup key carries. A durable `event_id` is
/// `gen_random_uuid()::text` (`asyncevents.events.event_id`'s DEFAULT, and
/// `asyncevents.append_event` takes no caller-supplied id), so it is hex and hyphens and can
/// never contain these letters — which is what keeps the two key spaces in the one shared
/// column disjoint.
pub(crate) const OPERATOR_DEDUP_PREFIX: &str = "admin-send-mail-";

/// The random half of an operator dedup key, in hex characters.
pub(crate) const OPERATOR_DEDUP_HEX: usize = 32;

pub(crate) const MALFORMED_DEDUP_KEY: &str = "dedup key is not an operator key";

/// True iff `s` is EXACTLY the shape `admin::mint_idempotency_key` mints. Checked at the
/// operator entry point rather than at the form, so no caller of the pub authority can
/// address a row in the durable half of the dedup column.
pub(crate) fn is_operator_dedup_key(s: &str) -> bool {
    match s.strip_prefix(OPERATOR_DEDUP_PREFIX) {
        Some(hex) => hex.len() == OPERATOR_DEDUP_HEX && hex.bytes().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

pub(crate) fn dedup_key_within_cap(key: &str) -> bool {
    key.len() <= MAX_DEDUP_KEY_BYTES
}

pub(crate) fn title_within_cap(title: &str) -> bool {
    title.len() <= MAX_TITLE_BYTES
}

pub(crate) fn body_within_cap(body: &str) -> bool {
    body.len() <= MAX_BODY_BYTES
}

pub(crate) fn kind_within_cap(kind: &str) -> bool {
    kind.len() <= MAX_KIND_BYTES
}

pub(crate) fn cursor_within_cap(cursor: &str) -> bool {
    cursor.len() <= MAX_CURSOR_BYTES
}

/// The opaque page token: base64url-no-pad over `"{created_at}|{id}"`, the keyset the next
/// page resumes from. Opaque so the shape stays this module's business, not a client's.
pub(crate) fn encode_cursor(created_at: &str, id: &str) -> String {
    B64.encode(format!("{created_at}|{id}"))
}

/// `Ok(None)` is the empty cursor — the first page. Everything else is either a keyset this
/// module minted or `Status::Invalid` (400): a cursor that fails the cap, the base64, the
/// separator or either half's shape is REJECTED, never silently reset to page 1, because a
/// silent reset makes a paging bug look like a working inbox that repeats its newest page.
///
/// Pure: no I/O, no `self`, so the caps and the reject arms are testable without a database.
pub(crate) fn decode_cursor(cursor: &str) -> Result<Option<(String, String)>, Error> {
    if cursor.is_empty() {
        return Ok(None);
    }
    if !cursor_within_cap(cursor) {
        return Err(Error::invalid(format!(
            "cursor exceeds {MAX_CURSOR_BYTES} bytes"
        )));
    }
    let raw = B64
        .decode(cursor)
        .map_err(|_| Error::invalid(MALFORMED_CURSOR))?;
    let text = String::from_utf8(raw).map_err(|_| Error::invalid(MALFORMED_CURSOR))?;
    let (created_at, id) = text
        .split_once('|')
        .ok_or_else(|| Error::invalid(MALFORMED_CURSOR))?;
    if !is_cursor_time(created_at) || !is_uuid_text(id) {
        return Err(Error::invalid(MALFORMED_CURSOR));
    }
    Ok(Some((created_at.to_string(), id.to_string())))
}

/// The contract has no `Option`, so `0` carries "unspecified" and a value above the ceiling
/// is CLAMPED rather than refused — a client asking for too much gets a page, not an error.
/// A negative limit is the one rejection: it is not an over-ask but a malformed request.
pub(crate) fn resolve_limit(limit: i64) -> Result<i64, Error> {
    if limit < 0 {
        return Err(Error::invalid("limit must not be negative"));
    }
    if limit == 0 {
        return Ok(DEFAULT_PAGE_LIMIT);
    }
    Ok(limit.min(MAX_PAGE_LIMIT))
}

/// One inbox row about to be written. `source_event_id` is the row's DEDUP identity, which
/// is the durable `event_id` for the fan-in and the admin form's render-time
/// `admin-send-mail-<hex>` key for operator mail. It is REQUIRED — [`validate_new`] refuses
/// an empty one, because a keyless row deduplicates nothing. The two key spaces are kept
/// disjoint by that prefix, which
/// [`Service::send_operator_mail`] requires of every operator send, so no caller of the
/// operator entry can pre-empt a real event's row.
pub(crate) struct NewNotification<'a> {
    pub(crate) player_id: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) title: &'a str,
    pub(crate) body: &'a str,
    pub(crate) source_event_id: &'a str,
}

/// What the dedup index did with one operator send.
pub(crate) enum Sent {
    /// Appended.
    Appended,
    /// This key already produced an IDENTICAL message — the double-submit the key exists for.
    Duplicate,
    /// This key already produced a DIFFERENT message: the posted form is stale and its
    /// message was NOT written.
    KeyReused,
}

/// THE input policy, enforced INSIDE the insert authority so no caller — operator form or
/// durable handler — can route around it. The byte caps mirror the table's column CHECKs:
/// without them a 23514 that nothing maps reaches the operator as a 500.
pub(crate) fn validate_new(n: &NewNotification<'_>) -> Result<(), Error> {
    if n.player_id.trim().is_empty() {
        return Err(Error::invalid("player_id is required"));
    }
    if n.kind.is_empty() {
        return Err(Error::invalid("kind is required"));
    }
    if !kind_within_cap(n.kind) {
        return Err(Error::invalid(format!(
            "kind exceeds {MAX_KIND_BYTES} bytes"
        )));
    }
    if !title_within_cap(n.title) {
        return Err(Error::invalid(format!(
            "title exceeds {MAX_TITLE_BYTES} bytes"
        )));
    }
    if !body_within_cap(n.body) {
        return Err(Error::invalid(format!(
            "body exceeds {MAX_BODY_BYTES} bytes"
        )));
    }
    // A keyless row is stored as NULL, which the PARTIAL unique index skips — so it would
    // dedup nothing and an operator re-drive would append a second copy into a player's
    // inbox. Refused here rather than left to each caller to remember.
    if n.source_event_id.trim().is_empty() {
        return Err(Error::invalid("dedup key is required"));
    }
    // The one cap with no column CHECK under it: `source_event_id` is a btree INDEX key, so
    // an over-long value is 54000 (an unmappable 500), not a 23514 this module could word.
    if !dedup_key_within_cap(n.source_event_id) {
        return Err(Error::invalid(format!(
            "dedup key exceeds {MAX_DEDUP_KEY_BYTES} bytes"
        )));
    }
    Ok(())
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

    /// The single insert authority: it runs on a CALLER-OWNED connection and never begins,
    /// commits or rolls back, so the durable fan-in's handed delivery transaction commits
    /// the row and its checkpoint together while operator mail runs the identical policy on
    /// a pool connection.
    ///
    /// `false` is the dedup index answering "this event already produced a row" — a normal
    /// outcome, not an error, because a handler that returned `Err` here would back off and
    /// pause the whole subscription.
    ///
    /// The two error classes are DISTINGUISHED by status and a durable caller must treat them
    /// differently: `Status::Invalid` is a data-quality rejection of ONE message (a cap, an
    /// empty field, a `player_id` the DB cannot parse) and a handler must answer `Ok(())` to
    /// it, since pausing every player's inbox over one bad payload is worse than skipping it;
    /// anything else is infrastructure and must propagate so the plane retries.
    pub(crate) async fn deliver_on(
        &self,
        conn: &mut PgConnection,
        n: &NewNotification<'_>,
    ) -> Result<bool, Error> {
        validate_new(n)?;
        let id = self
            .store
            .insert_tx(conn, n.player_id, n.kind, n.title, n.body, n.source_event_id)
            .await
            .map_err(|e| {
                if crate::is_invalid_uuid(&e) {
                    Error::invalid(MALFORMED_PLAYER_ID)
                } else {
                    internal(e)
                }
            })?;
        Ok(id.is_some())
    }

    /// Operator mail: the SAME [`Service::deliver_on`] policy, run on a pool connection, so
    /// the admin form cannot acquire an input rule the durable fan-in does not have.
    ///
    /// The key's shape is enforced HERE, not at the form: this is the only operator entry to
    /// the insert authority (neither it nor [`Service::deliver_on`] leaves the crate —
    /// `Service` escapes only as `dyn Player`), so refusing anything but an operator key is
    /// what stops any caller from claiming a row in the durable half of the shared dedup
    /// column and silently suppressing that event's notification.
    ///
    /// A key that already holds a row is NOT reported as sent on its own: `ON CONFLICT DO
    /// NOTHING` collapses "the same form submitted twice" and "an edited form resubmitted
    /// under its old key" into one outcome, and only the first may answer success — the
    /// second discarded an operator's correction.
    pub(crate) async fn send_operator_mail(&self, n: &NewNotification<'_>) -> Result<Sent, Error> {
        if !is_operator_dedup_key(n.source_event_id) {
            return Err(Error::invalid(MALFORMED_DEDUP_KEY));
        }
        let mut conn = self.store.pool.acquire().await.map_err(internal)?;
        if self.deliver_on(&mut conn, n).await? {
            return Ok(Sent::Appended);
        }
        // Two cases reach here: the row under this key carries a DIFFERENT message (an
        // edited form resubmitted under its old key), or it vanished between the conflicting
        // insert and this read. Neither may read as sent. Once a delete or a prune has
        // COMMITTED the key holds nothing, so a resubmit re-inserts and never gets here.
        let same = self
            .store
            .matches_tx(
                &mut conn,
                n.player_id,
                n.kind,
                n.title,
                n.body,
                n.source_event_id,
            )
            .await
            .map_err(internal)?;
        Ok(if same { Sent::Duplicate } else { Sent::KeyReused })
    }
}

#[async_trait]
impl Player for Service {
    /// The player id comes from `identity` (gateway-verified), NEVER from a body field — so
    /// a client cannot page another player's inbox.
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        let limit = resolve_limit(limit)?;
        let after = decode_cursor(&cursor)?;
        let after = after.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));

        // One row past the page: its presence IS `next_cursor`, so a full last page never
        // hands out a cursor that would answer empty.
        let mut items: Vec<Notification> = self
            .store
            .page_by_player(player_id, after, limit + 1)
            .await
            .map_err(internal)?;
        let has_more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next_cursor = match items.last() {
            Some(last) if has_more => encode_cursor(&last.created_at, &last.id),
            _ => String::new(),
        };
        Ok(Page { items, next_cursor })
    }

    async fn mark_read(&self, identity: Identity, notification_id: String) -> Result<(), Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        let mut conn = self.store.pool.acquire().await.map_err(internal)?;
        let stamped = self
            .store
            .mark_read_owned_tx(&mut conn, &notification_id, &player_id)
            .await
            .map_err(internal)?;
        if !stamped {
            return Err(Error::not_found(NOT_FOUND));
        }
        Ok(())
    }

    async fn delete(&self, identity: Identity, notification_id: String) -> Result<(), Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        let mut conn = self.store.pool.acquire().await.map_err(internal)?;
        let removed = self
            .store
            .delete_owned_tx(&mut conn, &notification_id, &player_id)
            .await
            .map_err(internal)?;
        if !removed {
            return Err(Error::not_found(NOT_FOUND));
        }
        Ok(())
    }
}

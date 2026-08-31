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

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// `N` stands for one ascii digit; every other byte must match literally. The shape is the
/// one `store::COLS` renders, so a cursor this rejects is one this module never minted.
const CURSOR_TIME_SHAPE: &str = "NNNN-NN-NNTNN:NN:NN.NNNNNNZ";

fn is_cursor_time(s: &str) -> bool {
    s.len() == CURSOR_TIME_SHAPE.len()
        && s.bytes()
            .zip(CURSOR_TIME_SHAPE.bytes())
            .all(|(c, p)| if p == b'N' { c.is_ascii_digit() } else { c == p })
}

/// True iff `$n::uuid` parses `s`, in the canonical hyphenated spelling `RETURNING id::text`
/// produces. Deliberately narrower than `uuid_in` (no braces, no bare 32 digits): the codec
/// only ever has to accept what it encoded.
fn is_uuid_text(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
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

/// One inbox row about to be written. `source_event_id` is the durable `event_id` that
/// produced it, or EMPTY for operator mail — see [`Store::insert_tx`] for what that changes.
pub struct NewNotification<'a> {
    pub player_id: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub source_event_id: &'a str,
}

/// THE input policy, enforced INSIDE the insert authority so no caller — operator form or
/// durable handler — can route around it. The byte caps mirror the `notifications_*_len`
/// column CHECKs: without them a 23514 that nothing maps reaches the operator as a 500.
fn validate_new(n: &NewNotification<'_>) -> Result<(), Error> {
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
    pub async fn deliver_on(
        &self,
        conn: &mut PgConnection,
        n: &NewNotification<'_>,
    ) -> Result<bool, Error> {
        validate_new(n)?;
        let id = self
            .store
            .insert_tx(conn, n.player_id, n.kind, n.title, n.body, n.source_event_id)
            .await
            .map_err(internal)?;
        Ok(id.is_some())
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

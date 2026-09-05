use std::sync::{Arc, OnceLock};

use accountsapi::{Directory, PlayerSummary, MAX_HANDLE_BYTES};
use async_trait::async_trait;
use base64::Engine;
use bus::{AnyTx, Bus};
use friendsapi::{
    Friend, Page, Player, DEFAULT_PAGE_LIMIT, DIRECTION_INCOMING, DIRECTION_OUTGOING,
    MAX_CURSOR_BYTES, MAX_PAGE_LIMIT, MAX_PENDING_OUTSTANDING, STATE_ACCEPTED, STATE_PENDING,
};
use friendsevents::{REASON_DECLINED, REASON_UNFRIENDED, REASON_WITHDRAWN};
use opsapi::{Error, Identity};
use sqlx::{PgPool, Postgres, Transaction};

use crate::internal;
use crate::store::{is_invalid_uuid, EdgeRow, Store};

/// The ONE answer for an edge that does not exist, one the caller is not party to, and one
/// whose state no longer admits the operation: a 403 would confirm the id names a real
/// relation.
pub(crate) const NOT_FOUND: &str = "friend relation not found";

pub(crate) const NO_SUCH_PLAYER: &str = "no player with that handle";

pub(crate) const MALFORMED_CURSOR: &str = "cursor is malformed";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// `N` is one ascii digit; every other byte matches literally. The shape `CREATED_TEXT`
/// renders.
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

/// The codec is the sole authority on what reaches `$2::timestamptz`, so the calendar is
/// checked HERE: a digit-SHAPED but impossible instant (`2026-13-45`, hour 25, year 0000)
/// would otherwise raise 22008 and answer 500, contradicting `Player::list`'s promise of a
/// 400 for a malformed cursor.
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

pub(crate) fn is_uuid_text(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

pub(crate) fn cursor_within_cap(cursor: &str) -> bool {
    cursor.len() <= MAX_CURSOR_BYTES
}

/// base64url-no-pad over `"{created_at}|{edge_id}"` — opaque so the keyset stays this
/// module's business.
pub(crate) fn encode_cursor(created_at: &str, edge_id: &str) -> String {
    B64.encode(format!("{created_at}|{edge_id}"))
}

/// `Ok(None)` is the empty cursor — the first page. Anything malformed is REJECTED, never
/// silently reset to page 1: a silent reset makes a paging bug look like a working list that
/// repeats its newest page. The cap is checked BEFORE the decode, as the contract states.
///
/// Pure: no I/O, no `self`, so every reject arm is testable without a database.
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
    let (created_at, edge_id) = text
        .split_once('|')
        .ok_or_else(|| Error::invalid(MALFORMED_CURSOR))?;
    if !is_cursor_time(created_at) || !is_uuid_text(edge_id) {
        return Err(Error::invalid(MALFORMED_CURSOR));
    }
    Ok(Some((created_at.to_string(), edge_id.to_string())))
}

/// `0` carries "unspecified" (the contract has no `Option`) and an over-ask is CLAMPED, not
/// refused. A negative limit is the one rejection: not an over-ask but a malformed request.
pub(crate) fn resolve_limit(limit: i64) -> Result<i64, Error> {
    if limit < 0 {
        return Err(Error::invalid("limit must not be negative"));
    }
    if limit == 0 {
        return Ok(DEFAULT_PAGE_LIMIT);
    }
    Ok(limit.min(MAX_PAGE_LIMIT))
}

/// Every directory failure is `Status::Unavailable` (503), never a page of blank names: the
/// only inputs this module passes — a handle already checked against [`MAX_HANDLE_BYTES`]
/// and at most `MAX_PAGE_LIMIT` canonical ids — cannot be the malformed request the
/// capability rejects, so an `Err` is the directory being unreachable. A MISS never reaches
/// here: it is an absent summary, which keeps its row with empty name and handle.
fn directory_unavailable(e: Error) -> Error {
    Error::unavailable(format!("player directory unavailable: {}", e.msg))
}

/// A `22P02` on the pair-addressed write path can only be the caller's own identity — the
/// target's id came from the directory — so it is a 400, not the 500 the raw SQLSTATE would
/// produce. The pair CHECK's `23514` has no arm because it is unreachable: a self-pair is
/// refused against the accounts-minted `identity.player_id()` before any statement runs.
fn pair_write_error(e: sqlx::Error) -> Error {
    if is_invalid_uuid(&e) {
        Error::invalid("player identity is not a valid uuid")
    } else {
        internal(e)
    }
}

/// The one place the table's `state` vocabulary meets the contract's.
fn wire_state(db_state: &str) -> Result<&'static str, Error> {
    match db_state {
        STATE_PENDING => Ok(STATE_PENDING),
        STATE_ACCEPTED => Ok(STATE_ACCEPTED),
        other => Err(internal(format!("unknown relation state {other:?}"))),
    }
}

/// The ONE join between the directory's ids and the database's. Both spaces are canonical
/// lowercase today, so the case-insensitive compare changes no outcome — it exists so the
/// two call sites cannot drift into disagreeing about what "the same player" means.
fn summary_of<'a>(summaries: &'a [PlayerSummary], id: &str) -> Option<&'a PlayerSummary> {
    summaries
        .iter()
        .find(|s| s.player_id.eq_ignore_ascii_case(id))
}

/// One party of a relation as an event payload needs it: the id plus the handle SNAPSHOT
/// taken at emit time. A player the directory does not return keeps an empty handle.
struct Party {
    id: String,
    handle: String,
}

fn party(summaries: &[PlayerSummary], id: &str) -> Party {
    match summary_of(summaries, id) {
        Some(s) => Party {
            id: s.player_id.clone(),
            handle: s.handle.clone(),
        },
        None => Party {
            id: id.to_string(),
            handle: String::new(),
        },
    }
}

/// CALLER-RELATIVE, and therefore not a `From<EdgeRow>`: `direction` says who asked
/// *relative to the caller*.
fn friend_of(
    edge_id: String,
    state: &str,
    requester_is_caller: bool,
    other_id: &str,
    other: Option<&PlayerSummary>,
) -> Friend {
    Friend {
        player_id: other.map(|s| s.player_id.clone()).unwrap_or_else(|| other_id.to_string()),
        display_name: other.map(|s| s.display_name.clone()).unwrap_or_default(),
        handle: other.map(|s| s.handle.clone()).unwrap_or_default(),
        online_until: other.map(|s| s.online_until.clone()).unwrap_or_default(),
        edge_id,
        state: state.to_string(),
        direction: if requester_is_caller {
            DIRECTION_OUTGOING
        } else {
            DIRECTION_INCOMING
        }
        .to_string(),
    }
}

pub struct Service {
    pub(crate) store: Store,
    pub(crate) bus: Arc<Bus>,
    /// Resolved in `init` (phase 2); in a split process a `remote::Stub` swaps an
    /// edge-backed client under the same key.
    pub(crate) directory: OnceLock<Arc<dyn Directory>>,
}

impl Service {
    pub fn new(pool: PgPool, bus: Arc<Bus>) -> Service {
        Service {
            store: Store { pool },
            bus,
            directory: OnceLock::new(),
        }
    }

    fn directory(&self) -> &Arc<dyn Directory> {
        self.directory
            .get()
            .expect("friends.init must resolve the accounts directory before any op")
    }

    fn caller(identity: &Identity) -> Result<String, Error> {
        identity
            .player_id()
            .map(str::to_string)
            .ok_or_else(|| Error::invalid("missing player identity"))
    }

    /// ONE batched call, made BEFORE any transaction opens: an RPC issued while holding one
    /// would pin the connection and the row's locks across the network, so one accounts blip
    /// would stall every writer.
    async fn parties(&self, me: &str, other_id: &str) -> Result<(Party, Party), Error> {
        let summaries = self
            .directory()
            .players_by_id(vec![me.to_string(), other_id.to_string()])
            .await
            .map_err(directory_unavailable)?;
        Ok((party(&summaries, me), party(&summaries, other_id)))
    }

    /// The cap is counted AFTER the insert, inside its transaction, so a caller already
    /// holding the maximum can still repeat an EXISTING request and be answered with it —
    /// only a relation this call actually created can be refused.
    async fn commit_new_request(
        &self,
        mut tx: Transaction<'_, Postgres>,
        edge_id: String,
        requester_id: String,
        requester_handle: String,
        target: &PlayerSummary,
    ) -> Result<Friend, Error> {
        let outstanding = self
            .store
            .count_authored_pending_tx(&mut tx, &requester_id, STATE_PENDING)
            .await
            .map_err(internal)?;
        if outstanding > MAX_PENDING_OUTSTANDING {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(format!(
                "at most {MAX_PENDING_OUTSTANDING} outstanding friend requests"
            )));
        }
        let evt = friendsevents::Requested {
            edge_id: edge_id.clone(),
            requester_id,
            requester_handle,
            addressee_id: target.player_id.clone(),
            addressee_handle: target.handle.clone(),
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &friendsevents::REQUESTED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(friend_of(
            edge_id,
            STATE_PENDING,
            true,
            &target.player_id,
            Some(target),
        ))
    }

    async fn page(
        &self,
        identity: Identity,
        cursor: String,
        limit: i64,
        state: &str,
    ) -> Result<Page, Error> {
        let me = Service::caller(&identity)?;
        let limit = resolve_limit(limit)?;
        let after = decode_cursor(&cursor)?;
        let after = after.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));

        // One row past the page: its presence IS `next_cursor`, so a full last page never
        // hands out a cursor that would answer empty.
        let mut rows: Vec<EdgeRow> = self
            .store
            .page(&me, state, after, limit + 1)
            .await
            .map_err(internal)?;
        let has_more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let next_cursor = match rows.last() {
            Some(last) if has_more => encode_cursor(&last.created_at, &last.edge_id),
            _ => String::new(),
        };

        let mut ids: Vec<String> = Vec::with_capacity(rows.len());
        for row in &rows {
            if !ids.iter().any(|id| id == &row.other_id) {
                ids.push(row.other_id.clone());
            }
        }
        let summaries = if ids.is_empty() {
            Vec::new()
        } else {
            self.directory()
                .players_by_id(ids)
                .await
                .map_err(directory_unavailable)?
        };

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let other = summary_of(&summaries, &row.other_id);
            items.push(friend_of(
                row.edge_id,
                wire_state(&row.state)?,
                row.requester_is_caller,
                &row.other_id,
                other,
            ));
        }
        Ok(Page { items, next_cursor })
    }
}

#[async_trait]
impl Player for Service {
    /// Branched by statement, never by a duplicate-key exception: a conflict on the pair
    /// index has three distinct causes, and turning it blindly into an acceptance would let
    /// a player accept their OWN request with a second POST. The repeat branch emits
    /// NOTHING.
    async fn request(&self, identity: Identity, target_handle: String) -> Result<Friend, Error> {
        let me = Service::caller(&identity)?;
        // `accountsapi`'s cap is the one authority for a handle's size.
        if target_handle.len() > MAX_HANDLE_BYTES {
            return Err(Error::invalid(format!(
                "handle exceeds {MAX_HANDLE_BYTES} bytes"
            )));
        }

        let target = self
            .directory()
            .find_by_handle(target_handle)
            .await
            .map_err(directory_unavailable)?
            .ok_or_else(|| Error::not_found(NO_SUCH_PLAYER))?;
        // Compared against the ACCOUNTS-MINTED identity, not the directory's answer for the
        // caller: a miss on the caller must not open a path to a self-pair, which the pair
        // CHECK would raise as an unmapped 23514.
        if me.eq_ignore_ascii_case(&target.player_id) {
            return Err(Error::invalid("cannot send a friend request to yourself"));
        }
        let me_summary = party(
            &self
                .directory()
                .players_by_id(vec![me.clone()])
                .await
                .map_err(directory_unavailable)?,
            &me,
        );

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_requester_tx(&mut tx, &me)
            .await
            .map_err(internal)?;

        if let Some((edge_id, requester_id)) = self
            .store
            .insert_pending_tx(&mut tx, &me, &target.player_id, STATE_PENDING)
            .await
            .map_err(pair_write_error)?
        {
            return self
                .commit_new_request(tx, edge_id, requester_id, me_summary.handle, &target)
                .await;
        }

        if let Some((edge_id, addressee_id)) = self
            .store
            .accept_crossing_tx(&mut tx, &me, &target.player_id, STATE_PENDING, STATE_ACCEPTED)
            .await
            .map_err(pair_write_error)?
        {
            // The roles are the ORIGINAL ones: the target authored the request this call
            // answers, so the caller is the addressee even though it called `request`.
            let evt = friendsevents::Accepted {
                edge_id: edge_id.clone(),
                requester_id: target.player_id.clone(),
                requester_handle: target.handle.clone(),
                addressee_id,
                addressee_handle: me_summary.handle,
            };
            self.bus
                .emit_tx(AnyTx::new(&mut *tx), &friendsevents::ACCEPTED, &evt)
                .await
                .map_err(internal)?;
            tx.commit().await.map_err(internal)?;
            return Ok(friend_of(
                edge_id,
                STATE_ACCEPTED,
                false,
                &target.player_id,
                Some(&target),
            ));
        }

        if let Some(row) = self
            .store
            .find_pair_tx(&mut tx, &me, &target.player_id)
            .await
            .map_err(pair_write_error)?
        {
            tx.rollback().await.map_err(internal)?;
            return Ok(friend_of(
                row.edge_id,
                wire_state(&row.state)?,
                row.requester_is_caller,
                &target.player_id,
                Some(&target),
            ));
        }

        // The pair's row was removed between the conflicting insert and this read, so the
        // request creates the relation after all: ONE retry, no loop. A second miss means a
        // third party re-created the pair inside that window; it names no outcome the
        // contract enumerates for `request` — least of all the cap's 409 — so it stays the
        // unclassified failure it is.
        match self
            .store
            .insert_pending_tx(&mut tx, &me, &target.player_id, STATE_PENDING)
            .await
            .map_err(pair_write_error)?
        {
            Some((edge_id, requester_id)) => {
                self.commit_new_request(tx, edge_id, requester_id, me_summary.handle, &target)
                    .await
            }
            None => {
                tx.rollback().await.map_err(internal)?;
                Err(internal("friend relation neither created nor resolvable"))
            }
        }
    }

    async fn accept(&self, identity: Identity, edge_id: String) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let view = self
            .store
            .view_edge(&edge_id, &me)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))?;
        let (mine, theirs) = self.parties(&me, &view.other_id).await?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let accepted = self
            .store
            .accept_tx(&mut tx, &edge_id, &me, STATE_PENDING, STATE_ACCEPTED)
            .await
            .map_err(internal)?;
        let (edge_id, other_id) = match accepted {
            Some(pair) => pair,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        // The predicate admits only the party that did NOT author the request, so the other
        // party is the requester and the caller is the addressee.
        let evt = friendsevents::Accepted {
            edge_id,
            requester_id: other_id,
            requester_handle: theirs.handle,
            addressee_id: mine.id,
            addressee_handle: mine.handle,
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &friendsevents::ACCEPTED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn decline(&self, identity: Identity, edge_id: String) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let view = self
            .store
            .view_edge(&edge_id, &me)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))?;
        let (mine, theirs) = self.parties(&me, &view.other_id).await?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let declined = self
            .store
            .decline_tx(&mut tx, &edge_id, &me, STATE_PENDING)
            .await
            .map_err(internal)?;
        let (edge_id, other_id) = match declined {
            Some(pair) => pair,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        let evt = friendsevents::Removed {
            edge_id,
            actor_id: mine.id,
            actor_handle: mine.handle,
            other_id,
            other_handle: theirs.handle,
            reason: REASON_DECLINED.to_string(),
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &friendsevents::REMOVED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn remove(&self, identity: Identity, edge_id: String) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let view = self
            .store
            .view_edge(&edge_id, &me)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))?;
        let (mine, theirs) = self.parties(&me, &view.other_id).await?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let removed = self
            .store
            .delete_tx(&mut tx, &edge_id, &me)
            .await
            .map_err(internal)?;
        let (edge_id, other_id, state, requester_is_caller) = match removed {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        // Decided from the DELETED row: ending a friendship and cancelling an offer say
        // opposite things about the graph.
        let reason = if wire_state(&state)? == STATE_ACCEPTED {
            REASON_UNFRIENDED
        } else if requester_is_caller {
            REASON_WITHDRAWN
        } else {
            REASON_DECLINED
        };
        let evt = friendsevents::Removed {
            edge_id,
            actor_id: mine.id,
            actor_handle: mine.handle,
            other_id,
            other_handle: theirs.handle,
            reason: reason.to_string(),
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &friendsevents::REMOVED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    /// The player id comes from `identity` (gateway-verified), NEVER from a body field.
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error> {
        self.page(identity, cursor, limit, STATE_ACCEPTED).await
    }

    async fn pending(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error> {
        self.page(identity, cursor, limit, STATE_PENDING).await
    }
}

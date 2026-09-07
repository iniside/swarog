use std::sync::{Arc, OnceLock};

use accountsapi::{Directory, PlayerSummary, MAX_HANDLE_BYTES};
use async_trait::async_trait;
use base64::Engine;
use bus::{AnyTx, Bus};
use groupsapi::{
    GroupPage, GroupSummary, MemberPage, MemberSummary, Membership, Player, DEFAULT_PAGE_LIMIT,
    JOIN_INVITE,
    JOIN_OPEN, JOIN_REQUEST, MAX_CURSOR_BYTES, MAX_MEMBERS, MAX_NAME_BYTES, MAX_PAGE_LIMIT,
    ROLE_ADMIN, ROLE_MEMBER, STATE_INVITED, STATE_MEMBER, STATE_REQUESTED,
};
use groupsevents::{REASON_DECLINED, REASON_KICKED, REASON_LEFT};
use opsapi::{Error, Identity};
use sqlx::PgPool;

use crate::internal;
use crate::store::{is_invalid_uuid, MemberRow, Store, MEMBER_ROWS, PENDING_ROWS};

/// The ONE answer for a group that does not exist, one the caller holds no visible row
/// in, one the caller is not an admin of, and a subject row whose state no longer admits
/// the operation: a 403 would confirm the id names a real group.
pub(crate) const NOT_FOUND: &str = "group not found";

pub(crate) const NO_SUCH_PLAYER: &str = "no player with that handle";

pub(crate) const MALFORMED_CURSOR: &str = "cursor is malformed";

/// `decision` on `respond`/`decide`. Not `groupsapi` consts: the contract states the two
/// literals in prose, and adding exported consts is a contract change.
pub(crate) const DECISION_ACCEPT: &str = "accept";
pub(crate) const DECISION_REJECT: &str = "reject";

/// [`Store::visible_role`]'s "any member will do" — the empty required role.
const ANY_ROLE: &str = "";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// `N` is one ascii digit; every other byte matches literally. The shape the store's
/// `created_text` renders.
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
/// checked HERE: a digit-SHAPED but impossible instant (`2026-02-30`, hour 25, year 0000)
/// would otherwise raise 22008 and answer 500, contradicting the contract's promise of a
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

/// base64url-no-pad over `"{created_at}|{id}"` — opaque so the keyset stays this module's
/// business. The id half is the group id for `list_mine` and the player id for
/// `members`/`pending`; both are uuids, so one codec serves both keysets.
pub(crate) fn encode_cursor(created_at: &str, id: &str) -> String {
    B64.encode(format!("{created_at}|{id}"))
}

/// `Ok(None)` is the empty cursor — the first page. Anything malformed is REJECTED, never
/// silently reset to page 1: a silent reset makes a paging bug look like a working list
/// that repeats its newest page. The cap is checked BEFORE the decode, as the contract
/// states.
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
    let (created_at, id) = text
        .split_once('|')
        .ok_or_else(|| Error::invalid(MALFORMED_CURSOR))?;
    if !is_cursor_time(created_at) || !is_uuid_text(id) {
        return Err(Error::invalid(MALFORMED_CURSOR));
    }
    Ok(Some((created_at.to_string(), id.to_string())))
}

/// `0` carries "unspecified" (the contract has no `Option`) and an over-ask is CLAMPED,
/// not refused. A negative limit is the one rejection: not an over-ask but a malformed
/// request.
pub(crate) fn resolve_limit(limit: i64) -> Result<i64, Error> {
    if limit < 0 {
        return Err(Error::invalid("limit must not be negative"));
    }
    if limit == 0 {
        return Ok(DEFAULT_PAGE_LIMIT);
    }
    Ok(limit.min(MAX_PAGE_LIMIT))
}

/// Checked in the SERVICE, before the statement: left to `groups_join_policy_check` this
/// is a `23514`, which has no mapping and would surface as a 500.
pub(crate) fn validate_policy(policy: &str) -> Result<&'static str, Error> {
    match policy {
        JOIN_OPEN => Ok(JOIN_OPEN),
        JOIN_REQUEST => Ok(JOIN_REQUEST),
        JOIN_INVITE => Ok(JOIN_INVITE),
        _ => Err(Error::invalid(format!(
            "join_policy must be one of {JOIN_OPEN}, {JOIN_REQUEST}, {JOIN_INVITE}"
        ))),
    }
}

/// Checked in the SERVICE for the same reason as [`validate_policy`]: an unrecognised
/// decision has no statement to reach at all, so without this it would silently behave
/// like a reject.
pub(crate) fn validate_decision(decision: &str) -> Result<&'static str, Error> {
    match decision {
        DECISION_ACCEPT => Ok(DECISION_ACCEPT),
        DECISION_REJECT => Ok(DECISION_REJECT),
        _ => Err(Error::invalid(format!(
            "decision must be {DECISION_ACCEPT} or {DECISION_REJECT}"
        ))),
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::invalid(format!(
            "name exceeds {MAX_NAME_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Every directory failure is `Status::Unavailable` (503), never a page of blank handles:
/// the only inputs this module passes — a handle already checked against
/// [`MAX_HANDLE_BYTES`] and at most `MAX_PAGE_LIMIT` canonical ids — cannot be the
/// malformed request the capability rejects, so an `Err` is the directory being
/// unreachable. A MISS never reaches here: it is an absent summary, which keeps its row
/// with an empty handle.
fn directory_unavailable(e: Error) -> Error {
    Error::unavailable(format!("player directory unavailable: {}", e.msg))
}

/// A `22P02` on a statement that binds the caller's own identity can only be that
/// identity — every group id reaching one has already been resolved by a statement that
/// binds it alone — so it is a 400, not the 500 the raw SQLSTATE would produce.
fn write_error(e: sqlx::Error) -> Error {
    if is_invalid_uuid(&e) {
        Error::invalid("player identity is not a valid uuid")
    } else {
        internal(e)
    }
}

/// The one place the table's `state` vocabulary meets the contract's.
fn wire_state(db_state: &str) -> Result<&'static str, Error> {
    match db_state {
        STATE_MEMBER => Ok(STATE_MEMBER),
        STATE_INVITED => Ok(STATE_INVITED),
        STATE_REQUESTED => Ok(STATE_REQUESTED),
        other => Err(internal(format!("unknown membership state {other:?}"))),
    }
}

/// The ONE join between the directory's ids and the database's. Both spaces are canonical
/// lowercase today, so the case-insensitive compare changes no outcome — it exists so the
/// call sites cannot drift into disagreeing about what "the same player" means.
fn summary_of<'a>(summaries: &'a [PlayerSummary], id: &str) -> Option<&'a PlayerSummary> {
    summaries
        .iter()
        .find(|s| s.player_id.eq_ignore_ascii_case(id))
}

fn member_of(row: MemberRow, summaries: &[PlayerSummary]) -> Result<MemberSummary, Error> {
    let handle = summary_of(summaries, &row.player_id)
        .map(|s| s.handle.clone())
        .unwrap_or_default();
    Ok(MemberSummary {
        player_id: row.player_id,
        handle,
        state: wire_state(&row.state)?.to_string(),
        role: row.role,
        joined_at: row.created_at,
    })
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
            .expect("groups.init must resolve the accounts directory before any op")
    }

    fn caller(identity: &Identity) -> Result<String, Error> {
        identity
            .player_id()
            .map(str::to_string)
            .ok_or_else(|| Error::invalid("missing player identity"))
    }

    /// The visibility gate for the two paged reads. `required_role` empty admits any
    /// member. Absent group, foreign group, pending-only row and insufficient role all
    /// leave by this one door.
    async fn require_role(
        &self,
        group_id: &str,
        me: &str,
        required_role: &str,
    ) -> Result<String, Error> {
        self.store
            .visible_role(group_id, me, STATE_MEMBER, required_role)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))
    }

    /// ONE batched call per page, made after the rows are read and OUTSIDE any
    /// transaction: an RPC issued while holding one would pin the connection and the
    /// rows' locks across the network, so one accounts blip would stall every writer.
    async fn hydrate(&self, rows: &[MemberRow]) -> Result<Vec<PlayerSummary>, Error> {
        let mut ids: Vec<String> = Vec::with_capacity(rows.len());
        for row in rows {
            if !ids.iter().any(|id| id == &row.player_id) {
                ids.push(row.player_id.clone());
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.directory()
            .players_by_id(ids)
            .await
            .map_err(directory_unavailable)
    }

    /// The caller's own handle, for the summary `join` answers with.
    async fn my_handle(&self, me: &str) -> Result<String, Error> {
        let summaries = self
            .directory()
            .players_by_id(vec![me.to_string()])
            .await
            .map_err(directory_unavailable)?;
        Ok(summary_of(&summaries, me)
            .map(|s| s.handle.clone())
            .unwrap_or_default())
    }

    async fn page_group(
        &self,
        identity: Identity,
        group_id: String,
        cursor: String,
        limit: i64,
        required_role: &str,
        state_clause: &str,
    ) -> Result<MemberPage, Error> {
        let me = Service::caller(&identity)?;
        let limit = resolve_limit(limit)?;
        let after = decode_cursor(&cursor)?;
        self.require_role(&group_id, &me, required_role).await?;

        let after = after.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));
        // One row past the page: its presence IS `next_cursor`, so a full last page never
        // hands out a cursor that would answer empty.
        let mut rows = self
            .store
            .page_group(&group_id, state_clause, after, limit + 1)
            .await
            .map_err(internal)?;
        let has_more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let next_cursor = match rows.last() {
            Some(last) if has_more => encode_cursor(&last.created_at, &last.player_id),
            _ => String::new(),
        };

        let summaries = self.hydrate(&rows).await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(member_of(row, &summaries)?);
        }
        Ok(MemberPage { items, next_cursor })
    }
}

#[async_trait]
impl Player for Service {
    async fn create(
        &self,
        identity: Identity,
        name: String,
        join_policy: String,
    ) -> Result<GroupSummary, Error> {
        let me = Service::caller(&identity)?;
        validate_name(&name)?;
        let policy = validate_policy(&join_policy)?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let (group_id, creator_id, created_at) = self
            .store
            .insert_group_tx(&mut tx, &name, policy, &me)
            .await
            .map_err(write_error)?;
        // The creator's own row rides the same transaction: a group whose only admin never
        // joined is a group nobody can administer.
        let joined = self
            .store
            .insert_membership_tx(&mut tx, &group_id, &creator_id, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(write_error)?;
        if joined.is_none() {
            tx.rollback().await.map_err(internal)?;
            return Err(internal("freshly created group already had a member row"));
        }
        self.bus
            .emit_tx(
                AnyTx::new(&mut *tx),
                &groupsevents::CREATED,
                &groupsevents::Created {
                    group_id: group_id.clone(),
                    name: name.clone(),
                    creator_id: creator_id.clone(),
                    join_policy: policy.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.bus
            .emit_tx(
                AnyTx::new(&mut *tx),
                &groupsevents::MEMBER_JOINED,
                &groupsevents::MemberJoined {
                    group_id: group_id.clone(),
                    player_id: creator_id,
                    role: ROLE_ADMIN.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;

        Ok(GroupSummary {
            id: group_id,
            name,
            join_policy: policy.to_string(),
            created_at,
            my_state: STATE_MEMBER.to_string(),
            my_role: ROLE_ADMIN.to_string(),
        })
    }

    /// The player id comes from `identity` (gateway-verified), NEVER from a body field.
    async fn list_mine(
        &self,
        identity: Identity,
        cursor: String,
        limit: i64,
    ) -> Result<GroupPage, Error> {
        let me = Service::caller(&identity)?;
        let limit = resolve_limit(limit)?;
        let after = decode_cursor(&cursor)?;
        let after = after.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));

        let mut rows = self
            .store
            .page_mine(&me, after, limit + 1)
            .await
            .map_err(internal)?;
        let has_more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        // Keyed on the MEMBERSHIP timestamp the statement ordered on, not the group's
        // `created_at` the summary carries: a cursor built from the displayed field would
        // skip rows the moment the two disagree.
        let next_cursor = match rows.last() {
            Some(last) if has_more => encode_cursor(&last.sort_at, &last.group_id),
            _ => String::new(),
        };

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(GroupSummary {
                id: row.group_id,
                name: row.name,
                join_policy: row.join_policy,
                created_at: row.created_at,
                my_state: wire_state(&row.my_state)?.to_string(),
                my_role: row.my_role,
            });
        }
        Ok(GroupPage { items, next_cursor })
    }

    async fn members(
        &self,
        identity: Identity,
        group_id: String,
        cursor: String,
        limit: i64,
    ) -> Result<MemberPage, Error> {
        self.page_group(identity, group_id, cursor, limit, ANY_ROLE, MEMBER_ROWS)
            .await
    }

    async fn pending(
        &self,
        identity: Identity,
        group_id: String,
        cursor: String,
        limit: i64,
    ) -> Result<MemberPage, Error> {
        self.page_group(identity, group_id, cursor, limit, ROLE_ADMIN, PENDING_ROWS)
            .await
    }

    async fn join(&self, identity: Identity, group_id: String) -> Result<MemberSummary, Error> {
        let me = Service::caller(&identity)?;
        // Resolved BEFORE the transaction opens, for the reason in [`Service::hydrate`].
        let handle = self.my_handle(&me).await?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_group_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?;
        let (policy, group_id) = match self
            .store
            .join_policy_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?
        {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        let (state, role) = match policy.as_str() {
            JOIN_OPEN => (STATE_MEMBER, ROLE_MEMBER),
            JOIN_REQUEST => (STATE_REQUESTED, ""),
            JOIN_INVITE => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::conflict("this group admits members by invitation only"));
            }
            other => {
                tx.rollback().await.map_err(internal)?;
                return Err(internal(format!("unknown join policy {other:?}")));
            }
        };

        // Counted under the group lock, which is the whole reason the lock exists: the
        // cap covers members and pending rows alike.
        let counts = self
            .store
            .counts_tx(&mut tx, &group_id, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?;
        if counts.rows >= MAX_MEMBERS {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(format!(
                "group is full at {MAX_MEMBERS} members"
            )));
        }

        let inserted = self
            .store
            .insert_membership_tx(&mut tx, &group_id, &me, state, role)
            .await
            .map_err(write_error)?;
        let (group_id, player_id, joined_at) = match inserted {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::conflict("you already have a relation to this group"));
            }
        };
        if state == STATE_MEMBER {
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_JOINED,
                    &groupsevents::MemberJoined {
                        group_id,
                        player_id: player_id.clone(),
                        role: role.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;

        Ok(MemberSummary {
            player_id,
            handle,
            state: state.to_string(),
            role: role.to_string(),
            joined_at,
        })
    }

    async fn leave(&self, identity: Identity, group_id: String) -> Result<(), Error> {
        let me = Service::caller(&identity)?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_group_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?;
        let mine = self
            .store
            .membership_tx(&mut tx, &group_id, &me)
            .await
            .map_err(internal)?;
        let (state, role, _) = match mine {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        let counts = self
            .store
            .counts_tx(&mut tx, &group_id, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?;
        // Without this the group freezes forever: no accepts, no invites, no kicks, and
        // no way to reach the last-member teardown that deletes it.
        if state == STATE_MEMBER && role == ROLE_ADMIN && counts.admins == 1 && counts.members > 1
        {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(
                "the last admin cannot leave a group that still has members",
            ));
        }

        let deleted = self
            .store
            .delete_membership_tx(&mut tx, &group_id, &me)
            .await
            .map_err(internal)?;
        let (deleted_state, _, group_id, player_id) = match deleted {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(internal("membership vanished while the group was locked"));
            }
        };
        // The last MEMBER leaving takes the group with it, along with whatever pending
        // rows it still owned: they name a group that no longer exists, and
        // `groups.groups` has no other collector.
        let swept = if wire_state(&deleted_state)? == STATE_MEMBER && counts.members == 1 {
            self.store
                .delete_group_tx(&mut tx, &group_id)
                .await
                .map_err(internal)?
        } else {
            Vec::new()
        };
        let reason = if wire_state(&deleted_state)? == STATE_MEMBER {
            REASON_LEFT
        } else {
            REASON_DECLINED
        };
        self.bus
            .emit_tx(
                AnyTx::new(&mut *tx),
                &groupsevents::MEMBER_LEFT,
                &groupsevents::MemberLeft {
                    group_id: group_id.clone(),
                    player_id: player_id.clone(),
                    actor_id: player_id.clone(),
                    reason: reason.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        // A pending row the teardown removed ends the same way a reject ends it, so the
        // log carries a terminal event for every relation it ever announced. Bounded by
        // `MAX_MEMBERS`, and the group lock is held either way.
        for swept_id in swept {
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_LEFT,
                    &groupsevents::MemberLeft {
                        group_id: group_id.clone(),
                        player_id: swept_id,
                        actor_id: player_id.clone(),
                        reason: REASON_DECLINED.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn invite(
        &self,
        identity: Identity,
        group_id: String,
        target_handle: String,
    ) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        // `accountsapi`'s cap is the one authority for a handle's size.
        if target_handle.len() > MAX_HANDLE_BYTES {
            return Err(Error::invalid(format!(
                "handle exceeds {MAX_HANDLE_BYTES} bytes"
            )));
        }
        // Pre-checked before the directory call so a caller who may not invite cannot use
        // this op as a handle-existence probe. The authoritative check is re-made inside
        // the transaction, under the group lock.
        self.require_role(&group_id, &me, ROLE_ADMIN).await?;
        let target = self
            .directory()
            .find_by_handle(target_handle)
            .await
            .map_err(directory_unavailable)?
            .ok_or_else(|| Error::not_found(NO_SUCH_PLAYER))?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_group_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?;
        if self
            .store
            .visible_role_tx(&mut tx, &group_id, &me, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?
            .is_none()
        {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(NOT_FOUND));
        }
        let counts = self
            .store
            .counts_tx(&mut tx, &group_id, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?;
        if counts.rows >= MAX_MEMBERS {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(format!(
                "group is full at {MAX_MEMBERS} members"
            )));
        }
        let inserted = self
            .store
            .insert_membership_tx(&mut tx, &group_id, &target.player_id, STATE_INVITED, "")
            .await
            .map_err(internal)?;
        if inserted.is_none() {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(
                "that player already has a relation to this group",
            ));
        }
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn respond(
        &self,
        identity: Identity,
        group_id: String,
        decision: String,
    ) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let decision = validate_decision(&decision)?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_group_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?;
        let mine = self
            .store
            .membership_tx(&mut tx, &group_id, &me)
            .await
            .map_err(internal)?;
        // A row in any other state — and no row at all — is the same `NotFound`: only an
        // invitation is the subject's to answer.
        match mine {
            Some((state, _, _)) if state == STATE_INVITED => {}
            _ => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        }

        if decision == DECISION_ACCEPT {
            let promoted = self
                .store
                .promote_tx(
                    &mut tx,
                    &group_id,
                    &me,
                    STATE_INVITED,
                    STATE_MEMBER,
                    ROLE_MEMBER,
                )
                .await
                .map_err(internal)?;
            let (group_id, player_id) = match promoted {
                Some(row) => row,
                None => {
                    tx.rollback().await.map_err(internal)?;
                    return Err(internal("invitation vanished while the group was locked"));
                }
            };
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_JOINED,
                    &groupsevents::MemberJoined {
                        group_id,
                        player_id,
                        role: ROLE_MEMBER.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        } else {
            let deleted = self
                .store
                .delete_membership_tx(&mut tx, &group_id, &me)
                .await
                .map_err(internal)?;
            let (group_id, player_id) = match deleted {
                Some((_, _, group_id, player_id)) => (group_id, player_id),
                None => {
                    tx.rollback().await.map_err(internal)?;
                    return Err(internal("invitation vanished while the group was locked"));
                }
            };
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_LEFT,
                    &groupsevents::MemberLeft {
                        group_id,
                        player_id: player_id.clone(),
                        actor_id: player_id,
                        reason: REASON_DECLINED.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn decide(
        &self,
        identity: Identity,
        group_id: String,
        subject_id: String,
        decision: String,
    ) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let decision = validate_decision(&decision)?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_group_tx(&mut tx, &group_id)
            .await
            .map_err(internal)?;
        // Authorization first: a non-admin learns nothing about the group, not even
        // whether the subject holds a row in it. The probe also answers the actor id as
        // the DATABASE spells it — `me` carries the caller's own spelling, which is
        // uuid-equal but need not be textually canonical.
        let actor_id = match self
            .store
            .visible_role_tx(&mut tx, &group_id, &me, STATE_MEMBER, ROLE_ADMIN)
            .await
            .map_err(internal)?
        {
            Some((_, actor_id)) => actor_id,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        let subject = self
            .store
            .membership_tx(&mut tx, &group_id, &subject_id)
            .await
            .map_err(internal)?;
        let (state, _, subject_canonical) = match subject {
            Some(row) => row,
            None => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found(NOT_FOUND));
            }
        };
        // Both sides are the DATABASE's spelling of the id, never the two the CALLER
        // supplied: `subject_id` and the identity are independent texts that can be
        // uuid-EQUAL while differing byte for byte (braced, urn-prefixed, unhyphenated),
        // so comparing them directly lets an admin kick itself through `decide`. The
        // subject's row was found by `player_id = $2::uuid`, so it is the same row the
        // admin probe answered `actor_id` from.
        if subject_canonical == actor_id {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict("use leave to end your own membership"));
        }
        let state = wire_state(&state)?;

        if decision == DECISION_ACCEPT {
            // Only a request is an admin's to grant: an invitation is the SUBJECT's
            // consent to give, and a member is already in.
            if state != STATE_REQUESTED {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::conflict(match state {
                    STATE_INVITED => "an invitation is the invited player's to accept",
                    _ => "that player is already a member",
                }));
            }
            let promoted = self
                .store
                .promote_tx(
                    &mut tx,
                    &group_id,
                    &subject_id,
                    STATE_REQUESTED,
                    STATE_MEMBER,
                    ROLE_MEMBER,
                )
                .await
                .map_err(internal)?;
            let (group_id, player_id) = match promoted {
                Some(row) => row,
                None => {
                    tx.rollback().await.map_err(internal)?;
                    return Err(internal("request vanished while the group was locked"));
                }
            };
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_JOINED,
                    &groupsevents::MemberJoined {
                        group_id,
                        player_id,
                        role: ROLE_MEMBER.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        } else {
            let deleted = self
                .store
                .delete_membership_tx(&mut tx, &group_id, &subject_id)
                .await
                .map_err(internal)?;
            let (deleted_state, _, group_id, player_id) = match deleted {
                Some(row) => row,
                None => {
                    tx.rollback().await.map_err(internal)?;
                    return Err(internal("membership vanished while the group was locked"));
                }
            };
            // Decided from the DELETED row: removing a member and refusing a pending row
            // say different things about the group, and the ledger retains 30 days.
            let reason = if wire_state(&deleted_state)? == STATE_MEMBER {
                REASON_KICKED
            } else {
                REASON_DECLINED
            };
            self.bus
                .emit_tx(
                    AnyTx::new(&mut *tx),
                    &groupsevents::MEMBER_LEFT,
                    &groupsevents::MemberLeft {
                        group_id,
                        player_id,
                        actor_id,
                        reason: reason.to_string(),
                    },
                )
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(())
    }
}

/// Wire-only, server-to-server: no `Identity`, so this trait decides nothing about a
/// caller — its authorization is the internal mTLS edge it is registered on. Nothing
/// routes it from the front door: `operations()`/`route_bindings()`/`describe()` are
/// emitted for `#[http]` methods only, and the player-QUIC plane matches against that
/// same table, so `groups.roleOf` is `NotFound` on both player planes.
#[async_trait]
impl Membership for Service {
    async fn role_of(&self, group_id: String, player_id: String) -> Result<String, Error> {
        // The empty role is the ONE answer for a non-member, a pending row, a group that
        // does not exist and an id that is not a uuid — the contract's promise, and the
        // same predicate the player face's visibility gate uses.
        Ok(self
            .store
            .visible_role(&group_id, &player_id, STATE_MEMBER, ANY_ROLE)
            .await
            .map_err(internal)?
            .unwrap_or_default())
    }
}

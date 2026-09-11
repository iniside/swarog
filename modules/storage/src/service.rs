use async_trait::async_trait;
use base64::Engine;
use opsapi::{Error, Identity};
use sqlx::PgPool;
use storageapi::{
    is_legal_segment, ObjectPage, ObjectVersion, Player, StorageObject, ACL_PRIVATE,
    ACL_PUBLIC_READ, DEFAULT_PAGE_LIMIT, MAX_CURSOR_BYTES, MAX_OBJECTS_PER_OWNER,
    MAX_PAGE_LIMIT, MAX_VALUE_BYTES,
};

use crate::store::{write_error, CasOutcome, ColumnCap, Store, COLLECTION_CAP, KEY_CAP};

/// The address verdict for a caller who may not see the object, and for one writing to an
/// address that holds nothing: the same answer, or the write path becomes an oracle.
pub(crate) const NOT_FOUND: &str = "no such object";

/// What `put` answers a caller whose account is already tombstoned. `Conflict`, not the
/// address's [`NOT_FOUND`]: the request is well-formed and names a real past player — what is
/// stale is the identity it carries.
pub(crate) const PLAYER_DELETED: &str = "this account has been deleted";

/// The ONE address verdict, worded identically for `collection` and `key`. The byte half
/// is [`ColumnCap`]'s, so Rust and the column CHECK cannot word the limit two ways.
fn validate_segment(cap: &ColumnCap, segment: &str) -> Result<(), Error> {
    if !is_legal_segment(segment) {
        return Err(Error::invalid(format!(
            "{} must be a non-empty run of [A-Za-z0-9._-]",
            cap.what
        )));
    }
    cap.check(segment)
}

pub(crate) fn validate_address(collection: &str, key: &str) -> Result<(), Error> {
    validate_segment(&COLLECTION_CAP, collection)?;
    validate_segment(&KEY_CAP, key)
}

/// Checked in the SERVICE, before the statement: a malformed document left to the
/// `::jsonb` cast is a `22P02` with no mapping, which would surface as a 500 where the
/// contract promises 400.
pub(crate) fn validate_value(value: &str) -> Result<(), Error> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(Error::invalid(format!(
            "value exceeds {MAX_VALUE_BYTES} bytes"
        )));
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map_err(|_| Error::invalid("value is not valid json"))?;
    Ok(())
}

/// The empty string is "leave the acl unchanged", not an unknown token. Checked here for
/// the same reason as [`validate_value`]: left to `storage_acl_check` this is a `23514`,
/// which has no mapping and would surface as a 500.
pub(crate) fn validate_acl(acl: &str) -> Result<(), Error> {
    match acl {
        "" | ACL_PRIVATE | ACL_PUBLIC_READ => Ok(()),
        _ => Err(Error::invalid(format!(
            "acl must be {ACL_PRIVATE}, {ACL_PUBLIC_READ}, or empty to leave it unchanged"
        ))),
    }
}

/// `0` is create-or-conflict, every positive value is a CAS expectation, and a negative
/// one is the single rejection: not an expectation the store could ever hold.
pub(crate) fn validate_expected_version(expected_version: i64) -> Result<(), Error> {
    if expected_version < 0 {
        return Err(Error::invalid("expected_version must not be negative"));
    }
    Ok(())
}

/// `0` carries "unspecified" (the contract has no `Option`) and an over-ask is CLAMPED,
/// not refused. A negative limit is the one rejection.
pub(crate) fn resolve_limit(limit: i64) -> Result<i64, Error> {
    if limit < 0 {
        return Err(Error::invalid("limit must not be negative"));
    }
    if limit == 0 {
        return Ok(DEFAULT_PAGE_LIMIT);
    }
    Ok(limit.min(MAX_PAGE_LIMIT))
}

/// The cap's own rejection text, shared with `conformance.rs` so a wording change here
/// stays in lockstep with the probe instead of tripping it on a rename.
pub(crate) fn cursor_too_long_message() -> String {
    format!("cursor exceeds {MAX_CURSOR_BYTES} bytes")
}

pub(crate) fn cursor_within_cap(cursor: &str) -> Result<(), Error> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(Error::invalid(cursor_too_long_message()));
    }
    Ok(())
}

pub(crate) const MALFORMED_CURSOR: &str = "cursor is malformed";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// `N` is one ascii digit; every other byte matches literally. The shape `store`'s
/// `to_char` renders.
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

/// The codec is the sole authority on what reaches `$3::timestamptz`, so the calendar is
/// checked HERE: a digit-SHAPED but impossible instant (`2026-02-30`, hour 25, year 0000)
/// would otherwise raise 22008 and answer 500, contradicting the contract's promise of a
/// 400 for a malformed cursor.
pub(crate) fn is_cursor_time(s: &str) -> bool {
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

/// base64url-no-pad over `"{created_at}|{key}"` — opaque, so the keyset stays this module's
/// business rather than a client's.
///
/// It carries the keyset and NOTHING else. The owner is never encoded, so a cursor replayed
/// from another session has no half that could reach the `owner_id` predicate, which
/// [`Store::page_objects`] binds from the caller's identity alone.
///
/// The second half is TEXT — a caller-authored `key`, where the tree's other keyset codecs
/// carry a server-minted uuid. That is what makes the `|` separator safe to split on:
/// [`is_legal_segment`] is the one alphabet, and `|` is outside it.
pub(crate) fn encode_cursor(created_at: &str, key: &str) -> String {
    B64.encode(format!("{created_at}|{key}"))
}

/// `Ok(None)` is the empty cursor — the first page. Anything malformed is REJECTED, never
/// silently reset to page 1: a silent reset makes a paging bug look like a working listing
/// that repeats its newest page. The cap is checked BEFORE the decode, as the contract
/// states.
///
/// Accepting a cursor does not mean this module minted it — an attacker can encode any real
/// instant and any legal key — only that the keyset is well-formed, which is all a keyset
/// predicate needs, since neither half can name another owner.
///
/// Pure: no I/O, no `self`, so every reject arm is testable without a database.
pub(crate) fn decode_cursor(cursor: &str) -> Result<Option<(String, String)>, Error> {
    if cursor.is_empty() {
        return Ok(None);
    }
    cursor_within_cap(cursor)?;
    let raw = B64
        .decode(cursor)
        .map_err(|_| Error::invalid(MALFORMED_CURSOR))?;
    let text = String::from_utf8(raw).map_err(|_| Error::invalid(MALFORMED_CURSOR))?;
    let (created_at, key) = text
        .split_once('|')
        .ok_or_else(|| Error::invalid(MALFORMED_CURSOR))?;
    if !is_cursor_time(created_at) || !is_legal_segment(key) || key.len() > KEY_CAP.max_bytes {
        return Err(Error::invalid(MALFORMED_CURSOR));
    }
    Ok(Some((created_at.to_string(), key.to_string())))
}

/// The canonical hyphenated spelling, hex case-insensitive — what `accounts` mints and the
/// only shape `storage.objects`' `uuid` owner column can hold.
pub(crate) fn is_uuid_text(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// A caller id outside [`is_uuid_text`] is an identity the front door minted and this
/// module can never own rows for. It is a SERVER fault: no request field is at fault, so
/// `Invalid` would blame the caller's input; re-authenticating mints the same identity
/// again, so `Unauthorized` and `Unavailable` would both promise a retry that cannot
/// succeed.
pub(crate) const MALFORMED_IDENTITY: &str = "caller identity is not a uuid";

pub struct Service {
    pub(crate) store: Store,
}

impl Service {
    pub fn new(pool: PgPool) -> Service {
        Service {
            store: Store { pool },
        }
    }

    /// The ONE place the shape of a caller id is decided, ahead of every statement that
    /// binds it, so `put`/`get`/`delete`/`list` cannot answer one malformed identity four
    /// ways — and so a `22P02` from a statement binding only this id is store trouble
    /// rather than an outcome to swallow.
    fn caller(identity: &Identity) -> Result<String, Error> {
        let me = identity
            .player_id()
            .ok_or_else(|| Error::unauthorized("a player identity is required"))?;
        if !is_uuid_text(me) {
            return Err(Error::internal(MALFORMED_IDENTITY));
        }
        Ok(me.to_string())
    }

    /// The purge backlog for the admin page: outstanding tombstones and the oldest one's
    /// `created_at`. Public for the admin page's future call, which lives outside this module.
    pub async fn pending_purges(&self) -> Result<(i64, String), Error> {
        self.store.pending_purges().await.map_err(crate::internal)
    }

    /// The create half of `put`. The owner cap is counted and the row inserted under the
    /// owner's advisory lock in ONE transaction: counting outside it is a count-then-insert
    /// that two concurrent creates both pass.
    async fn create(
        &self,
        owner_id: &str,
        collection: &str,
        key: &str,
        value: &str,
        acl: &str,
    ) -> Result<ObjectVersion, Error> {
        let mut tx = self.store.pool.begin().await.map_err(write_error)?;
        self.store
            .lock_owner_tx(&mut tx, owner_id)
            .await
            .map_err(write_error)?;
        // Read UNDER the owner lock, which is what orders it against the purge sweep: an
        // insert admitted past a committed tombstone is an object nothing collects, because
        // the sweep has no second pass and this module never prunes by time. The CAS half of
        // `put` needs no such guard — it creates no row, and a row it can still find is one
        // the sweep is yet to delete.
        if self
            .store
            .is_player_purged_tx(&mut tx, owner_id)
            .await
            .map_err(write_error)?
        {
            tx.rollback().await.map_err(write_error)?;
            return Err(Error::conflict(PLAYER_DELETED));
        }
        let held = self
            .store
            .count_objects_tx(&mut tx, owner_id)
            .await
            .map_err(write_error)?;
        // An owner at the cap rewriting an address it already holds gets this 409 rather than
        // the address-taken one: same status, misleading remedy.
        if held >= MAX_OBJECTS_PER_OWNER {
            tx.rollback().await.map_err(write_error)?;
            return Err(Error::conflict(format!(
                "you already hold the maximum of {MAX_OBJECTS_PER_OWNER} objects"
            )));
        }
        let created = self
            .store
            .insert_object_tx(&mut tx, owner_id, collection, key, value, acl)
            .await
            .map_err(write_error)?;
        let Some(version) = created else {
            tx.rollback().await.map_err(write_error)?;
            return Err(Error::conflict(
                "an object already exists at this address; read it and write with its version",
            ));
        };
        tx.commit().await.map_err(write_error)?;
        Ok(ObjectVersion { version })
    }
}

#[async_trait]
impl Player for Service {
    async fn put(
        &self,
        identity: Identity,
        collection: String,
        key: String,
        value: String,
        expected_version: i64,
        acl: String,
    ) -> Result<ObjectVersion, Error> {
        let me = Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        validate_value(&value)?;
        validate_acl(&acl)?;
        validate_expected_version(expected_version)?;

        if expected_version == 0 {
            let acl = if acl.is_empty() { ACL_PRIVATE } else { acl.as_str() };
            return self.create(&me, &collection, &key, &value, acl).await;
        }
        let acl = if acl.is_empty() { None } else { Some(acl.as_str()) };
        let outcome = self
            .store
            .cas_object(&me, &collection, &key, &value, expected_version, acl)
            .await
            .map_err(write_error)?;
        match outcome {
            CasOutcome::Updated(version) => Ok(ObjectVersion { version }),
            CasOutcome::Stale => Err(Error::conflict(
                "expected_version does not match the stored version; re-read and retry",
            )),
            CasOutcome::Missing => Err(Error::not_found(NOT_FOUND)),
        }
    }

    async fn get(
        &self,
        identity: Identity,
        collection: String,
        key: String,
        owner_id: String,
    ) -> Result<StorageObject, Error> {
        let me = Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        // EMPTY is the only "my own" spelling resolved in Rust. An owner_id the caller
        // echoes back is settled by uuid equality inside the statement, which two
        // differently-spelled but DB-equal ids agree on and a string compare would not.
        let owner = if owner_id.is_empty() {
            me.as_str()
        } else {
            owner_id.as_str()
        };
        self.store
            .read_visible_object(owner, &collection, &key, &me)
            .await
            .map_err(crate::internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))
    }

    async fn delete(
        &self,
        identity: Identity,
        collection: String,
        key: String,
    ) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        let dropped = self
            .store
            .delete_object(&me, &collection, &key)
            .await
            .map_err(crate::internal)?;
        if !dropped {
            return Err(Error::not_found(NOT_FOUND));
        }
        Ok(())
    }

    async fn list(
        &self,
        identity: Identity,
        collection: String,
        cursor: String,
        limit: i64,
    ) -> Result<ObjectPage, Error> {
        let me = Service::caller(&identity)?;
        validate_segment(&COLLECTION_CAP, &collection)?;
        let after = decode_cursor(&cursor)?;
        let limit = resolve_limit(limit)?;
        let mut items = self
            .store
            .page_objects(
                &me,
                &collection,
                after.as_ref().map(|(at, key)| (at.as_str(), key.as_str())),
                limit + 1,
            )
            .await
            .map_err(crate::internal)?;
        // One row past the page: its presence IS `next_cursor`, so a full last page never
        // hands out a cursor that would answer empty.
        let has_more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next_cursor = match items.last() {
            Some(last) if has_more => encode_cursor(&last.created_at, &last.key),
            _ => String::new(),
        };
        Ok(ObjectPage { items, next_cursor })
    }
}

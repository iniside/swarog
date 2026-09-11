use async_trait::async_trait;
use opsapi::{Error, Identity};
use sqlx::PgPool;
use storageapi::{
    is_legal_segment, ObjectPage, ObjectVersion, Player, StorageObject, ACL_PRIVATE,
    ACL_PUBLIC_READ, DEFAULT_PAGE_LIMIT, MAX_CURSOR_BYTES, MAX_OBJECTS_PER_OWNER,
    MAX_PAGE_LIMIT, MAX_VALUE_BYTES,
};

use crate::store::{write_error, CasOutcome, ColumnCap, Store, COLLECTION_CAP, KEY_CAP};

/// What `get`, `delete` and `list` answer until Step 4 replaces their bodies with
/// statements. The validation above each one is already the contract's, so every argument
/// whose verdict does not require the store gets its real verdict.
const NOT_IMPLEMENTED: &str = "storage: not implemented";

/// The address verdict for a caller who may not see the object, and for one writing to an
/// address that holds nothing: the same answer, or the write path becomes an oracle.
pub(crate) const NOT_FOUND: &str = "no such object";

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

pub struct Service {
    pub(crate) store: Store,
}

impl Service {
    pub fn new(pool: PgPool) -> Service {
        Service {
            store: Store { pool },
        }
    }

    fn caller(identity: &Identity) -> Result<String, Error> {
        identity
            .player_id()
            .map(str::to_string)
            .ok_or_else(|| Error::unauthorized("a player identity is required"))
    }

    /// The create half of `put`. The owner cap is counted and the row inserted under the
    /// owner's advisory lock in ONE transaction: counting outside it is a
    /// count-then-insert two concurrent creates both pass.
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
        let held = self
            .store
            .count_objects_tx(&mut tx, owner_id)
            .await
            .map_err(write_error)?;
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
        _owner_id: String,
    ) -> Result<StorageObject, Error> {
        Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        Err(Error::internal(NOT_IMPLEMENTED))
    }

    async fn delete(
        &self,
        identity: Identity,
        collection: String,
        key: String,
    ) -> Result<(), Error> {
        Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        Err(Error::internal(NOT_IMPLEMENTED))
    }

    async fn list(
        &self,
        identity: Identity,
        collection: String,
        cursor: String,
        limit: i64,
    ) -> Result<ObjectPage, Error> {
        Service::caller(&identity)?;
        validate_segment(&COLLECTION_CAP, &collection)?;
        cursor_within_cap(&cursor)?;
        resolve_limit(limit)?;
        Err(Error::internal(NOT_IMPLEMENTED))
    }
}

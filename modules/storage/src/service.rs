use async_trait::async_trait;
use opsapi::{Error, Identity};
use sqlx::PgPool;
use storageapi::{
    is_legal_segment, ObjectPage, ObjectVersion, Player, StorageObject, ACL_PRIVATE,
    ACL_PUBLIC_READ, DEFAULT_PAGE_LIMIT, MAX_COLLECTION_BYTES, MAX_CURSOR_BYTES, MAX_KEY_BYTES,
    MAX_PAGE_LIMIT, MAX_VALUE_BYTES,
};

use crate::store::Store;

/// What every op answers until Steps 3 and 4 replace its body with a statement. The
/// validation above each one is already the contract's, so a caller's malformed request
/// gets its real verdict; only the storage half is absent.
const NOT_IMPLEMENTED: &str = "storage: not implemented";

/// The ONE address verdict, worded identically for `collection` and `key`. The alphabet
/// is checked before the byte cap so an illegal segment is never echoed back at length.
fn validate_segment(what: &str, segment: &str, max_bytes: usize) -> Result<(), Error> {
    if !is_legal_segment(segment) {
        return Err(Error::invalid(format!(
            "{what} must be a non-empty run of [A-Za-z0-9._-]"
        )));
    }
    if segment.len() > max_bytes {
        return Err(Error::invalid(format!(
            "{what} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn validate_address(collection: &str, key: &str) -> Result<(), Error> {
    validate_segment("collection", collection, MAX_COLLECTION_BYTES)?;
    validate_segment("key", key, MAX_KEY_BYTES)
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

pub(crate) fn cursor_within_cap(cursor: &str) -> Result<(), Error> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(Error::invalid(format!(
            "cursor exceeds {MAX_CURSOR_BYTES} bytes"
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

    fn caller(identity: &Identity) -> Result<String, Error> {
        identity
            .player_id()
            .map(str::to_string)
            .ok_or_else(|| Error::unauthorized("a player identity is required"))
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
        Service::caller(&identity)?;
        validate_address(&collection, &key)?;
        validate_value(&value)?;
        validate_acl(&acl)?;
        validate_expected_version(expected_version)?;
        Err(Error::internal(NOT_IMPLEMENTED))
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
        validate_segment("collection", &collection, MAX_COLLECTION_BYTES)?;
        cursor_within_cap(&cursor)?;
        resolve_limit(limit)?;
        Err(Error::internal(NOT_IMPLEMENTED))
    }
}

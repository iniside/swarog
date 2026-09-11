//! `storageapi` — the storage module's PURE, transport-free contract for player
//! cloud-save: opaque, caller-authored JSON documents addressed by
//! `(owner_id, collection, key)` and written under optimistic concurrency control.
//! The edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives in the
//! sibling `storagerpc`, which expands this crate's metadata-callback macro, so THIS
//! crate never depends on `edge`.
//!
//! One capability: [`Player`], the `#[http]` player face. The server never interprets a
//! stored value, so nothing here is wire-only and nothing downstream can project it.
//!
//! **Scalars here are `i64` and `String` only.** `tools/csharp-client-gen`'s type lattice
//! does not model `bool`, `u32` or `usize`, and every `#[http]`-reachable DTO field is
//! mapped through it, so any of those fails the blocking codegen-freshness stage. An
//! arbitrary JSON document is therefore a `String` carrying its serialized text, and an
//! ACL is a `String` with exported consts, never a Rust enum.
//!
//! An object the caller may not see answers [`opsapi::Status::NotFound`], never
//! `Forbidden`: a 403 would confirm the address names a real object, which is an
//! enumeration oracle. That applies to WRITES too — a non-owner's write is `NotFound`,
//! or the oracle returns through the write path.
//!
//! The address contains its owner's id, so anyone who can read a public object already
//! knew whose it was. That is a deliberately accepted oracle in the addressing scheme.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Byte cap on a `collection`, enforced in the service before the statement, with the
/// schema's `storage_collection_len_check` as the backstop.
pub const MAX_COLLECTION_BYTES: usize = 64;

/// Byte cap on a `key`, enforced in the service before the statement, with the schema's
/// `storage_key_len_check` as the backstop.
pub const MAX_KEY_BYTES: usize = 128;

/// Byte cap on the caller's `value` TEXT, enforced in the service before the statement.
///
/// The schema's `storage_value_len_check` measures NORMALIZED jsonb text, which is a
/// different byte string, so it is a backstop at a deliberately different number — never
/// a twin of this cap.
pub const MAX_VALUE_BYTES: usize = 65_536;

/// Upper bound on live objects per owner, enforced in the service.
pub const MAX_OBJECTS_PER_OWNER: i64 = 10_000;

/// Byte cap on the opaque paging cursor. A longer cursor is [`opsapi::Status::Invalid`]
/// (400) before it is decoded.
pub const MAX_CURSOR_BYTES: usize = 256;

/// Upper bound on the `limit` accepted by [`Player::list`]; a larger value is clamped to
/// this, not rejected.
pub const MAX_PAGE_LIMIT: i64 = 100;

/// The page size [`Player::list`] uses when `limit == 0`.
pub const DEFAULT_PAGE_LIMIT: i64 = 25;

/// Readable only by its owner. The default for a newly created object.
pub const ACL_PRIVATE: &str = "private";
/// Readable by any authenticated caller that can name the address. Writable only by the
/// owner — public read never widens the write predicate.
pub const ACL_PUBLIC_READ: &str = "public_read";

/// True for a `collection` or `key` segment made only of `[A-Za-z0-9._-]`, non-empty.
///
/// The ONE authority for both path bindings and the paging cursor's `|` separator: these
/// are the first caller-authored path wildcards in the tree, and the gateway router
/// matches raw, undecoded segments with an exact segment count, so a `/` in a key is
/// unaddressable over HTTP and a `%` stores whatever the client happened to encode. The
/// player-QUIC plane has no such restriction, so without this check in the SERVICE the
/// two topologies would accept different key sets.
///
/// Length is NOT checked here — that is [`MAX_COLLECTION_BYTES`]/[`MAX_KEY_BYTES`], whose
/// verdict is a distinct message.
pub fn is_legal_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// One stored object as its reader sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageObject {
    pub owner_id: String,
    pub collection: String,
    pub key: String,
    /// The caller-authored JSON document, as text.
    pub value: String,
    pub version: i64,
    /// [`ACL_PRIVATE`] or [`ACL_PUBLIC_READ`].
    pub acl: String,
    /// RFC3339.
    pub created_at: String,
    /// RFC3339.
    pub updated_at: String,
}

/// What a successful [`Player::put`] answers: the version the write COMMITTED at, which
/// is the value the caller's next `expected_version` must carry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectVersion {
    pub version: i64,
}

/// One row of [`Player::list`]. Deliberately carries no `value`: a page of up to
/// [`MAX_PAGE_LIMIT`] documents at [`MAX_VALUE_BYTES`] each is a response nobody asked
/// for. A listing names objects; [`Player::get`] fetches one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectSummary {
    pub collection: String,
    pub key: String,
    pub version: i64,
    pub acl: String,
    /// RFC3339.
    pub created_at: String,
    /// RFC3339.
    pub updated_at: String,
}

/// One page of [`Player::list`], newest first. `next_cursor` is opaque and EMPTY when the
/// page is the last one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectPage {
    pub items: Vec<ObjectSummary>,
    pub next_cursor: String,
}

/// The player-facing cloud-save capability.
///
/// `get` and `list` are `POST` because the cursor, the page size and the object's owner
/// ride the REQUEST BODY: the `#[http]` grammar sources an argument from a path wildcard
/// or the JSON body, and has no query-parameter source.
#[rpc(prefix = "storage")]
#[async_trait]
pub trait Player: Send + Sync {
    /// Writes the caller's own object under optimistic concurrency control.
    ///
    /// `expected_version` is the version the caller last read. `0` means CREATE: the
    /// object must not exist, and an existing one is [`opsapi::Status::Conflict`] — there
    /// is deliberately no unconditional-overwrite value, because a blind overwrite is
    /// exactly what a naive client would send and it defeats OCC. A NEGATIVE
    /// `expected_version` is [`opsapi::Status::Invalid`]. A positive expectation that
    /// does not match the stored version is [`opsapi::Status::Conflict`]; an address the
    /// caller does not own, or that holds no object, is [`opsapi::Status::NotFound`].
    ///
    /// `value` is the document as JSON text, capped at [`MAX_VALUE_BYTES`]. `acl` is
    /// [`ACL_PRIVATE`] or [`ACL_PUBLIC_READ`]; the EMPTY string leaves an existing
    /// object's acl unchanged (and creates a [`ACL_PRIVATE`] one), so a value-only write
    /// cannot silently un-share an object.
    ///
    /// NOT `#[retry_safe]`: a replay of a CAS write after an ambiguous network result
    /// would carry a now-stale expectation, so the client re-reads and re-CASes instead.
    #[http(verb = "POST", path = "/storage/{collection}/{key}", auth = "player",
           success = 200, path_args(collection = "collection", key = "key"))]
    async fn put(&self, identity: Identity, collection: String, key: String, value: String,
                 expected_version: i64, acl: String) -> Result<ObjectVersion, Error>;

    /// Reads one object. `owner_id` EMPTY means the caller's own; any other owner
    /// resolves only when the object is [`ACL_PUBLIC_READ`].
    ///
    /// A nonexistent object, another owner's private object and a malformed `owner_id`
    /// all answer the SAME [`opsapi::Status::NotFound`].
    #[http(verb = "POST", path = "/storage/{collection}/{key}/get", auth = "player",
           success = 200, path_args(collection = "collection", key = "key"))]
    #[retry_safe]
    async fn get(&self, identity: Identity, collection: String, key: String, owner_id: String)
        -> Result<StorageObject, Error>;

    /// Drops the caller's own object. An address holding no object the caller owns is
    /// [`opsapi::Status::NotFound`], so a replay is a 404 — the tree's uniform shape.
    ///
    /// The address is then free to be created again, and the new object starts at version
    /// 1: `version` identifies a write within ONE incarnation of an address, not across
    /// deletions.
    #[http(verb = "DELETE", path = "/storage/{collection}/{key}", auth = "player",
           success = 204, path_args(collection = "collection", key = "key"))]
    async fn delete(&self, identity: Identity, collection: String, key: String)
        -> Result<(), Error>;

    /// The caller's OWN objects in `collection`, newest first. There is no cross-owner
    /// listing: a public object is readable by whoever can name it, never discoverable.
    #[http(verb = "POST", path = "/storage/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list(&self, identity: Identity, collection: String, cursor: String, limit: i64)
        -> Result<ObjectPage, Error>;
}

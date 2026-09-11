//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production code real requests traverse — `Player::put` and `Player::list` themselves,
//! never a re-stated arithmetic check.

use opsapi::Identity;
use sqlx::PgPool;
use storageapi::{Player as _, ACL_PRIVATE};

use crate::service::cursor_too_long_message;
use crate::Service;

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that ever did reach the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

fn service(rt: &tokio::runtime::Runtime) -> Service {
    let _guard = rt.enter();
    Service::new(dead_pool())
}

/// A `len`-byte run inside the legal segment alphabet, so the only thing that varies
/// between the two probe calls is the length, never the character set.
fn legal(len: usize) -> String {
    "a".repeat(len)
}

/// A `len`-byte JSON document: a string literal padded to the exact length, so an at-cap
/// value passes `validate_value`'s parse and only its length can decide the verdict.
fn json_of(len: usize) -> String {
    format!("\"{}\"", "a".repeat(len - 2))
}

fn put_rejected(collection: String, key: String, value: String) -> bool {
    let rt = runtime();
    let svc = service(&rt);
    let outcome = rt.block_on(svc.put(
        Identity::player("conformance-storage-probe"),
        collection,
        key,
        value,
        0,
        ACL_PRIVATE.to_string(),
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// `storageapi::MAX_COLLECTION_BYTES`, executed through `Player::put` itself against a pool
/// that cannot connect. The discriminant is the STATUS: `validate_address` runs before any
/// statement, so an at-cap collection passes it, reaches the store and answers `Internal`;
/// only the cap's own rejection is `Invalid`. Delete the guard and the over-cap collection
/// reaches the dead store too, so the case goes red rather than passing on the column
/// CHECK below it.
#[doc(hidden)]
pub fn conformance_collection_rejected(len: usize) -> bool {
    put_rejected(legal(len), "k".to_string(), "{}".to_string())
}

/// `storageapi::MAX_KEY_BYTES`, on the same authority and the same discriminant:
/// `validate_address` words the collection and the key verdict identically, and both run
/// ahead of the store.
#[doc(hidden)]
pub fn conformance_key_rejected(len: usize) -> bool {
    put_rejected("c".to_string(), legal(len), "{}".to_string())
}

/// `storageapi::MAX_VALUE_BYTES`, executed through `Player::put`. `validate_value` checks
/// the length BEFORE the json parse, and the probe's document is well-formed at every
/// length, so the parse arm can never decide either call.
#[doc(hidden)]
pub fn conformance_value_rejected(len: usize) -> bool {
    put_rejected("c".to_string(), "k".to_string(), json_of(len))
}

/// `storageapi::MAX_CURSOR_BYTES`, executed through `Player::list` itself. The
/// discriminant is the MESSAGE, not the status: the cap is checked before the cursor is
/// decoded, so once a decoder exists an at-cap filler is `Invalid` too — as a MALFORMED
/// cursor, a different verdict from the cap's own. The message comes from the same
/// `cursor_too_long_message` production calls, so a wording change can't desync the probe.
#[doc(hidden)]
pub fn conformance_cursor_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = service(&rt);
    let outcome = rt.block_on(svc.list(
        Identity::player("conformance-storage-probe"),
        "c".to_string(),
        legal(len),
        0,
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid && e.msg == cursor_too_long_message())
}

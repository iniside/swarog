//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production code real requests traverse — `Player::request`/`Player::list` themselves,
//! never a re-stated arithmetic check.

use std::sync::Arc;

use accountsapi::{Directory, PlayerSummary};
use async_trait::async_trait;
use bus::Bus;
use friendsapi::Player as _;
use opsapi::{Error, Identity};
use sqlx::PgPool;

use crate::Service;

/// A `Directory` that fails every call, standing in for the capability being unreachable
/// (a down remote peer in split, a dead pool in monolith) — the one shape `directory_unavailable`
/// exists to fold into `Status::Unavailable`, whatever status the underlying failure carried.
struct FailingDirectory;

#[async_trait]
impl Directory for FailingDirectory {
    async fn players_by_id(&self, _ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error> {
        Err(Error::internal("directory offline"))
    }

    async fn find_by_handle(&self, _handle: String) -> Result<Option<PlayerSummary>, Error> {
        Err(Error::internal("directory offline"))
    }
}

fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

fn service_with_failing_directory() -> Service {
    let svc = Service::new(dead_pool(), Arc::new(Bus::new()));
    svc.directory
        .set(Arc::new(FailingDirectory) as Arc<dyn Directory>)
        .unwrap_or_else(|_| panic!("directory OnceLock set once"));
    svc
}

/// `accountsapi::MAX_HANDLE_BYTES`, executed through `Player::request` itself: the length
/// guard runs BEFORE any directory call, so an at-cap handle reaches the (failing)
/// directory and answers `Unavailable`, never `Invalid` — only an over-cap handle is
/// rejected by the cap. Discriminated on status rather than on reaching a live directory,
/// so the case stays zero-I/O.
#[doc(hidden)]
pub fn conformance_target_handle_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        service_with_failing_directory()
    };
    let outcome = rt.block_on(svc.request(Identity::player("conformance-handle-probe"), "a".repeat(len)));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// `friendsapi::MAX_CURSOR_BYTES`, executed through `Player::list` itself: `decode_cursor`
/// checks the cap BEFORE the base64 decode, so an at-cap filler still fails (as a malformed
/// cursor, not a cap violation) and only the over-cap case is the cap's own rejection —
/// discriminated by message, the same shape `notifications.list`'s cursor case uses.
#[doc(hidden)]
pub fn conformance_cursor_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        Service::new(dead_pool(), Arc::new(Bus::new()))
    };
    let outcome = rt.block_on(svc.list(
        Identity::player("conformance-cursor-probe"),
        "a".repeat(len),
        0,
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid && e.msg != crate::service::MALFORMED_CURSOR)
}

/// The directory-outage story: `Player::request` against a `Directory` that fails every
/// call, proving `directory_unavailable` folds it into `Status::Unavailable` (503) rather
/// than surfacing the underlying error or a blank success.
#[doc(hidden)]
pub async fn conformance_directory_outage() -> Result<(), Error> {
    let svc = service_with_failing_directory();
    svc.request(
        Identity::player("conformance-outage-probe"),
        "Someone#1234".to_string(),
    )
    .await
    .map(|_| ())
}

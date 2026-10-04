//! Minimal factual probes consumed by `tools/conformance`. Policy and expected outcomes live
//! in the tool; these probes only run the production code real requests traverse.

use std::sync::Arc;

use accountsapi::{Directory, PlayerSummary};
use async_trait::async_trait;
use opsapi::{Error, Identity};
use partyapi::Player as _;
use sqlx::PgPool;

use crate::Service;

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

const PROBE_PLAYER: &str = "00000000-0000-4000-8000-000000000001";

/// A pool pointed at a port nothing listens on, with a short acquire wait so a probe that
/// reaches the store fails fast.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

/// A `Directory` that fails every call: the capability unreachable.
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

fn service_with_failing_directory() -> Service {
    let svc = Service::new(
        dead_pool(),
        Arc::new(push::Push::new()),
        crate::DEFAULT_LEASE_SECS,
    );
    svc.directory
        .set(Arc::new(FailingDirectory) as Arc<dyn Directory>)
        .unwrap_or_else(|_| panic!("directory OnceLock set once"));
    svc
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

/// `accountsapi::MAX_HANDLE_BYTES`, executed through `Player::invite` itself against a pool
/// that cannot connect. The cap runs before the leadership read, so an at-cap handle reaches
/// the dead store and answers `Internal`; only the over-cap value answers `Invalid`.
#[doc(hidden)]
pub fn conformance_target_handle_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        service_with_failing_directory()
    };
    let outcome = rt.block_on(svc.invite(Identity::player(PROBE_PLAYER), "a".repeat(len)));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// The directory-outage story: `invite`'s target resolution — the step `Player::invite` takes
/// once the caller's leadership is confirmed — against a `Directory` that fails every call,
/// proving it answers `Status::Unavailable` (503). `invite` itself reads leadership from the
/// database first, so it cannot reach this step without a live store.
#[doc(hidden)]
pub async fn conformance_directory_outage() -> Result<(), Error> {
    let svc = service_with_failing_directory();
    svc.resolve_target("Probe#1234".to_string()).await.map(|_| ())
}

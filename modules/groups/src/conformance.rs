//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production code real requests traverse — `Player::create`/`list_mine`/`invite` and
//! `admin::apply_submit` themselves, never a re-stated arithmetic check.

use std::sync::Arc;

use accountsapi::{Directory, PlayerSummary};
use async_trait::async_trait;
use bus::Bus;
use groupsapi::{Player as _, JOIN_OPEN};
use opsapi::{Error, Identity};
use sqlx::PgPool;

use crate::admin::{apply_submit, Rejection, ACTION_FIELD, ACTION_PROMOTE, GROUP_FIELD, PLAYER_FIELD};
use crate::service::MALFORMED_CURSOR;
use crate::store::MemberRow;
use crate::Service;

/// The one length `service::is_uuid_text` admits — the spelling every group and player id
/// reaches the admin form in.
pub const UUID_SHAPE_BYTES: usize = 36;

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that ever did reach the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

/// A `Directory` that fails every call, standing in for the capability being unreachable
/// (a down `accounts-svc` in split, a dead pool in monolith).
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

fn service() -> Service {
    Service::new(dead_pool(), Arc::new(Bus::new()))
}

fn service_with_failing_directory() -> Service {
    let svc = service();
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

/// A `len`-byte value in the spelling the form renders and `is_uuid_text` admits: hex with
/// the four dashes at their uuid positions, so the only thing that varies between the two
/// probe calls is the length.
fn uuid_shaped(len: usize) -> String {
    (0..len)
        .map(|i| match i {
            8 | 13 | 18 | 23 => '-',
            _ => 'a',
        })
        .collect()
}

/// `groupsapi::MAX_NAME_BYTES`, executed through `Player::create` itself against a pool that
/// cannot connect. The discriminant is the STATUS: `validate_name` runs before the
/// transaction opens, so an at-cap name reaches the store and answers `Internal`, and only
/// the cap's own rejection is `Invalid`. Delete `validate_name` and the over-cap name
/// reaches the dead store too — the case goes red rather than passing on the layer below it.
#[doc(hidden)]
pub fn conformance_name_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        service()
    };
    let outcome = rt.block_on(svc.create(
        Identity::player("conformance-name-probe"),
        "a".repeat(len),
        JOIN_OPEN.to_string(),
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// `groupsapi::MAX_CURSOR_BYTES`, executed through `Player::list_mine` itself: `decode_cursor`
/// checks the cap BEFORE the base64 decode, so an at-cap filler still fails (as a malformed
/// cursor, not a cap violation) and only the over-cap case is the cap's own rejection —
/// discriminated by MESSAGE, since both arms are `Status::Invalid`.
#[doc(hidden)]
pub fn conformance_cursor_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        service()
    };
    let outcome = rt.block_on(svc.list_mine(
        Identity::player("conformance-cursor-probe"),
        "a".repeat(len),
        0,
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid && e.msg != MALFORMED_CURSOR)
}

/// `accountsapi::MAX_HANDLE_BYTES`, executed through `Player::invite` itself. The handle cap
/// runs before `require_role`, so an at-cap handle reaches the dead store and answers
/// `Internal`; only the over-cap value answers `Invalid`.
#[doc(hidden)]
pub fn conformance_target_handle_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        service_with_failing_directory()
    };
    let outcome = rt.block_on(svc.invite(
        Identity::player("conformance-handle-probe"),
        uuid_shaped(UUID_SHAPE_BYTES),
        "a".repeat(len),
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

fn submit(pairs: &[(&str, &str)]) -> Result<adminapi::SubmitOutcome, Rejection> {
    let values: adminapi::Params = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        service()
    };
    rt.block_on(apply_submit(&svc, values))
}

/// The selected group's shape, executed through `admin::apply_submit` ITSELF — the one
/// submit authority both topologies run. At the admitted length the value passes
/// `rendered_group` and reaches the store, which cannot connect, so it answers `Internal`;
/// any other length is `rendered_group`'s own `Stale` verdict, ahead of the checkout.
#[doc(hidden)]
pub fn conformance_admin_group_rejected(len: usize) -> bool {
    let group = uuid_shaped(len);
    let player = uuid_shaped(UUID_SHAPE_BYTES);
    matches!(
        submit(&[
            (ACTION_FIELD, ACTION_PROMOTE),
            (GROUP_FIELD, &group),
            (PLAYER_FIELD, &player),
        ]),
        Err(Rejection::Stale)
    )
}

/// The promoted player's shape, on the same submit authority: at the admitted length the
/// value reaches the store and answers `Internal`, any other length is `apply_submit`'s own
/// `Rejected` verdict naming the field.
#[doc(hidden)]
pub fn conformance_admin_player_rejected(len: usize) -> bool {
    let group = uuid_shaped(UUID_SHAPE_BYTES);
    let player = uuid_shaped(len);
    matches!(
        submit(&[
            (ACTION_FIELD, ACTION_PROMOTE),
            (GROUP_FIELD, &group),
            (PLAYER_FIELD, &player),
        ]),
        Err(Rejection::Rejected(_))
    )
}

/// The directory-outage story: the paged reads' `hydrate` — the ONE call the member and
/// pending pages make per page — against a `Directory` that fails every call, proving
/// `directory_unavailable` folds it into `Status::Unavailable` (503) rather than serving a
/// page of blank handles. `invite` and `join` fold through the same function but re-check
/// the caller's role in the database first, so neither is reachable without a live store.
#[doc(hidden)]
pub async fn conformance_directory_outage() -> Result<(), Error> {
    let svc = service_with_failing_directory();
    let rows = vec![MemberRow {
        player_id: uuid_shaped(UUID_SHAPE_BYTES),
        state: "member".to_string(),
        role: "admin".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
    }];
    svc.hydrate(&rows).await.map(|_| ())
}

//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production validators real writes and real requests traverse.

use controlplane::{EntityRef, InvokeCall, Provider as _};
use notificationsapi::Player as _;
use opsapi::Identity;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::admin::{NotificationsAdmin, FIELD_BODY, FIELD_TITLE, SEND};
use crate::service::{validate_new, NewNotification, Service, MALFORMED_CURSOR};

/// Re-stated by reference so `tools/conformance` names this cap instead of a second
/// literal — `service::MAX_DEDUP_KEY_BYTES` stays the only definition site. It is a module
/// const rather than a contract one because no wire field carries a dedup key.
pub const MAX_DEDUP_KEY_BYTES: usize = crate::service::MAX_DEDUP_KEY_BYTES;

/// The control plane's per-value input cap, named here so `tools/conformance` states no
/// second literal.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

const PROBE_PLAYER_ID: &str = "00000000-0000-4000-8000-000000000000";
const PROBE_KIND: &str = "conformance.probe";
const PROBE_TITLE: &str = "conformance probe";
const PROBE_BODY: &str = "conformance probe";
const PROBE_DEDUP_KEY: &str = "conformance-probe-key";

/// A record every rule of [`validate_new`] accepts, so a case that varies ONE field is
/// decided by that field's cap and nothing else.
fn base() -> NewNotification<'static> {
    NewNotification {
        player_id: PROBE_PLAYER_ID,
        kind: PROBE_KIND,
        title: PROBE_TITLE,
        body: PROBE_BODY,
        source_event_id: PROBE_DEDUP_KEY,
    }
}

/// The four write-path caps are executed at `service::validate_new` — the single input
/// policy both writers run, called as the first statement of `Service::deliver_on`.
///
/// It is the deepest level reachable WITHOUT a database: `validate_new` runs below
/// `Service::send_operator_mail`'s `pool.begin()` and `Service::deliver_on` takes an
/// already-acquired connection, so driving either entry point would decide every case on
/// the connection rather than on the cap.
fn write_rejected(n: &NewNotification<'_>) -> bool {
    validate_new(n).is_err()
}

#[doc(hidden)]
pub fn conformance_title_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewNotification {
        title: &filler,
        ..base()
    })
}

#[doc(hidden)]
pub fn conformance_body_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewNotification {
        body: &filler,
        ..base()
    })
}

#[doc(hidden)]
pub fn conformance_kind_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewNotification {
        kind: &filler,
        ..base()
    })
}

/// The dedup column's cap, on the same authority as the other three. Unlike them it has no
/// column CHECK under it — `source_event_id` is a btree index key — so `validate_new` is
/// the only level that can word a verdict for it, and no operator entry can carry a value
/// this long: `Service::send_operator_mail` admits only the fixed-length
/// `admin-send-mail-<hex>` shape.
#[doc(hidden)]
pub fn conformance_dedup_key_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewNotification {
        source_event_id: &filler,
        ..base()
    })
}

/// The paging cursor's cap, executed through `notificationsapi::Player::list` ITSELF rather
/// than through the codec below it, so deleting the op's `decode_cursor` call turns this
/// case red as surely as deleting the cap does.
///
/// Both cases are `Status::Invalid`, so the verdict — not the status — is the discriminant:
/// every cursor `service::encode_cursor` mints is far shorter than the cap, so an at-cap
/// value cannot be well-formed and answers `MALFORMED_CURSOR`, while an over-cap value is
/// refused by the cap ahead of the base64. With the cap deleted the over-cap value falls
/// through to `MALFORMED_CURSOR` too and the case fails.
///
/// Neither case reaches the store: the pool is pointed at a port nothing listens on with a
/// bounded acquire, so a `decode_cursor` call that stopped rejecting could only answer
/// `Status::Internal`, never a cap verdict.
#[doc(hidden)]
pub fn conformance_cursor_rejected(len: usize) -> bool {
    let rt = runtime();
    let svc = {
        let _guard = rt.enter();
        Service::new(dead_pool(), Arc::new(push::Push::new()))
    };
    let outcome = rt.block_on(svc.list(
        Identity::player("conformance-cursor-probe"),
        "a".repeat(len),
        0,
    ));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid && e.msg != MALFORMED_CURSOR)
}

/// The provider's own first check on every `cp.invoke`, executed through
/// `NotificationsAdmin::invoke` against a store that cannot connect. The send carries a valid
/// title, body, request id and player, so an at-cap extra value reaches the store and answers
/// `Internal`; one byte over, the cap's `Invalid` is the only refusal it can meet.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let rt = runtime();
    let admin = {
        let _guard = rt.enter();
        NotificationsAdmin::new(Arc::new(Service::new(dead_pool(), Arc::new(push::Push::new()))))
    };
    let input = BTreeMap::from([
        (FIELD_TITLE.to_string(), PROBE_TITLE.to_string()),
        (FIELD_BODY.to_string(), PROBE_BODY.to_string()),
        ("value".to_string(), "a".repeat(len)),
    ]);
    let call = InvokeCall {
        module: "notifications".to_string(),
        action: SEND.to_string(),
        entity: Some(EntityRef { kind: "player".to_string(), id: PROBE_PLAYER_ID.to_string() }),
        input,
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: Some("send:conformance-probe".to_string()),
    };
    matches!(rt.block_on(admin.invoke(call)), Err(e) if e.status == opsapi::Status::Invalid)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that ever did reach the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

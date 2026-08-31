//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production validators a durable delivery and an operator submit traverse — a fixture
//! that restated a rule instead of driving it would stay green after the rule was deleted.

use sqlx::PgPool;

use crate::admin::{
    apply_submit, Rejection, ACTION_FIELD, ACTION_REQUEUE, ACTION_SEND_TEST, IDEM_TEST_FIELD,
    MAIL_ID_FIELD, OUTBOX_ID_BYTES, TEST_TO_FIELD,
};
use crate::service::{validate_new, NewMail, TEST_KEY_HEX, TEST_KEY_PREFIX};
use crate::Service;

/// The one length `service::is_test_key` admits, DERIVED from the two parts that make the
/// shape rather than written down again: a prefix or suffix change moves this with it.
pub const TEST_KEY_BYTES: usize = TEST_KEY_PREFIX.len() + TEST_KEY_HEX;

/// The one length `admin::is_outbox_id` admits, by reference to that rule's own const.
pub const OUTBOX_ID_SHAPE_BYTES: usize = OUTBOX_ID_BYTES;

const PROBE_KEY: &str = "conformance-probe-key";
const PROBE_RECIPIENT: &str = "probe@example.com";
const PROBE_SUBJECT: &str = "conformance probe";
const PROBE_BODY: &str = "conformance probe";
const PROBE_KIND: &str = "conformance.probe";

/// A request every rule of [`validate_new`] accepts, so a case that varies ONE field is
/// decided by that field's cap and nothing else.
fn base() -> NewMail<'static> {
    NewMail {
        idempotency_key: PROBE_KEY,
        recipient: PROBE_RECIPIENT,
        subject: PROBE_SUBJECT,
        body: PROBE_BODY,
        kind: PROBE_KIND,
    }
}

/// The four column caps are executed at `service::validate_new` — the single input policy
/// the enqueue authority runs as its first statement, shared by the durable ingress and the
/// operator form.
///
/// It is the deepest level reachable WITHOUT a database: `Service::enqueue_on` takes an
/// already-open connection and `Service::enqueue_from_admin` checks out a transaction
/// BEFORE it, so driving either would decide every case on the connection rather than on
/// the cap.
///
/// `recipient` has no case here, and that is a finding rather than an omission: its
/// declared 320-byte cap cannot be separated from the parser under it. `address::
/// parse_address` delegates to `lettre::Address`, whose local part is capped at 64 bytes
/// and domain at 254, so the longest address it will parse is 319 — no input exists that
/// the 320-byte check accepts and the parser rejects, or the reverse. The cap changes the
/// operator's message, never the verdict.
fn write_rejected(m: &NewMail<'_>) -> bool {
    validate_new(m).is_err()
}

#[doc(hidden)]
pub fn conformance_idempotency_key_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewMail {
        idempotency_key: &filler,
        ..base()
    })
}

#[doc(hidden)]
pub fn conformance_subject_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewMail {
        subject: &filler,
        ..base()
    })
}

#[doc(hidden)]
pub fn conformance_body_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewMail {
        body: &filler,
        ..base()
    })
}

#[doc(hidden)]
pub fn conformance_kind_rejected(len: usize) -> bool {
    let filler = "a".repeat(len);
    write_rejected(&NewMail {
        kind: &filler,
        ..base()
    })
}

/// The operator test-send key's shape, executed through `admin::apply_submit` ITSELF — the
/// one submit authority both topologies run — rather than through the predicate below it.
///
/// The discriminant is the VERDICT, not the presence of an error: a key of the minted shape
/// passes `rendered_key` and reaches the store, which cannot connect, so it answers
/// `Internal`; a longer one is refused as a stale form ahead of any checkout. Deleting
/// `rendered_key`'s check leaves the over-length key to `enqueue_from_admin`'s own refusal,
/// which is `Rejected`, not `Stale` — so the case goes red rather than passing on the
/// layer below it.
#[doc(hidden)]
pub fn conformance_test_key_rejected(len: usize) -> bool {
    let hex = "a".repeat(len.saturating_sub(TEST_KEY_PREFIX.len()));
    let values = params(&[
        (ACTION_FIELD, ACTION_SEND_TEST),
        (TEST_TO_FIELD, PROBE_RECIPIENT),
        (IDEM_TEST_FIELD, &format!("{TEST_KEY_PREFIX}{hex}")),
    ]);
    matches!(submit(values), Err(Rejection::Stale))
}

/// The selected row id's shape, on the same submit authority and the same reasoning: at the
/// admitted length the value reaches `Service::requeue_parked` and answers `Internal` off
/// the dead pool, and any other length is `admin::is_outbox_id`'s own stated rejection.
/// Deleting that check sends the over-length value to the `$1::uuid` cast instead, where a
/// pool that cannot connect answers `Internal` — the case's ACCEPTED arm.
#[doc(hidden)]
pub fn conformance_outbox_id_rejected(len: usize) -> bool {
    let values = params(&[
        (ACTION_FIELD, ACTION_REQUEUE),
        (MAIL_ID_FIELD, &uuid_shaped(len)),
    ]);
    matches!(submit(values), Err(Rejection::Rejected(_)))
}

/// A `len`-byte value in the spelling the row selector renders: hex with the four dashes at
/// their uuid positions, so the only thing that varies between the two probe calls is the
/// length.
fn uuid_shaped(len: usize) -> String {
    (0..len)
        .map(|i| match i {
            8 | 13 | 18 | 23 => '-',
            _ => 'a',
        })
        .collect()
}

fn params(pairs: &[(&str, &str)]) -> adminapi::Params {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn submit(values: adminapi::Params) -> Result<adminapi::SubmitOutcome, Rejection> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let svc = {
        let _guard = rt.enter();
        Service::new(dead_pool())
    };
    rt.block_on(apply_submit(&svc, values))
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

//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production validators a durable delivery and an operator invoke traverse — a fixture
//! that restated a rule instead of driving it would stay green after the rule was deleted.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlplane::{InvokeCall, InvokeOutcome, Provider};
use sqlx::PgPool;

use crate::admin::{self, MailAdmin, OUTBOX_ID_BYTES};
use crate::config::{MailConfig, DEFAULT_MAX_ATTEMPTS, DEFAULT_RETENTION_DAYS, DEFAULT_SEND_TIMEOUT_MS};
use crate::service::{validate_new, NewMail, Service, TEST_KEY_PREFIX};
use crate::store::IDEMPOTENCY_KEY;

/// The longest key `service::is_test_key` admits: the outbox key column's own cap.
pub const TEST_KEY_BYTES: usize = IDEMPOTENCY_KEY.max_bytes;

/// The one length `admin::is_outbox_id` admits, by reference to that rule's own const.
pub const OUTBOX_ID_SHAPE_BYTES: usize = OUTBOX_ID_BYTES;

/// The byte cap the provider applies to every `cp.invoke` input value.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

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

/// The five column caps are executed at `service::validate_new` — the single input policy
/// the enqueue authority runs as its first statement, shared by the durable ingress and
/// `Service::enqueue_from_admin`.
///
/// It is the deepest level reachable WITHOUT a database: `Service::enqueue_on` takes an
/// already-open connection and `Service::enqueue_from_admin` checks out a transaction
/// BEFORE it, so driving either would decide every case on the connection rather than on
/// the cap.
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

/// The recipient's cap, on the same authority. Both probe lengths are addresses
/// `lettre::Address` accepts, so the cap is the only thing that can decide between them:
/// the parser's limits (local part 64 bytes, domain 254) are applied to the PUNYCODE form,
/// while `address::parse_address` counts raw UTF-8, and a repeated non-ASCII label
/// compresses far enough that a 321-byte address is still routable. An ASCII-only fixture
/// could not separate the two — the longest ASCII address that parses is 319 bytes, so
/// every over-cap ASCII value is refused by the parser whether the cap exists or not.
///
/// This is the field that most needs the case: it is the only mail input that is both
/// producer- and operator-authored, it becomes an SMTP envelope, and its column CHECK is
/// the class fail-safe — an over-long value that reached the INSERT would be an unmapped
/// 23514, which pauses the ingress subscription.
#[doc(hidden)]
pub fn conformance_recipient_rejected(len: usize) -> bool {
    write_rejected(&NewMail {
        recipient: &address_of_len(len),
        ..base()
    })
}

/// A `len`-byte address whose only variable part is the ASCII local part, so the two probe
/// lengths differ by one byte and by nothing else.
fn address_of_len(len: usize) -> String {
    let label = "\u{4e00}".repeat(30);
    let domain = format!("{label}.{label}.{label}.com");
    let local = "a".repeat(len.saturating_sub(domain.len() + 1));
    format!("{local}@{domain}")
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

/// The provider's own first check on every `cp.invoke`, before it reads any input.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let input = BTreeMap::from([("value".to_string(), "a".repeat(len))]);
    !admin::input_within_cap(&input)
}

/// The test-send key ceiling, executed through `MailAdmin::invoke` itself against a store
/// that cannot connect. The discriminant is the VERDICT: a key within the cap passes
/// `is_test_key`, reaches the checkout and answers `Internal`; a longer one is refused as
/// `Invalid` before it. Deleting the length leg sends the long key to the checkout too, so
/// the case goes red rather than passing on the column CHECK below it.
#[doc(hidden)]
pub fn conformance_test_key_rejected(len: usize) -> bool {
    let request_id = "a".repeat(len.saturating_sub(TEST_KEY_PREFIX.len()));
    let input = BTreeMap::from([("to".to_string(), PROBE_RECIPIENT.to_string())]);
    refused(call("test-send", input, None, Some(request_id)))
}

/// The bound message id's shape, on the same authority and the same reasoning: at the
/// admitted length the value reaches `Service::requeue_parked` and answers `Internal` off
/// the dead pool, and any other length is `admin::is_outbox_id`'s own `Invalid`.
#[doc(hidden)]
pub fn conformance_outbox_id_rejected(len: usize) -> bool {
    refused(call("resume", BTreeMap::new(), Some(uuid_shaped(len)), None))
}

fn call(action: &str, input: BTreeMap<String, String>, item: Option<String>, request_id: Option<String>) -> InvokeCall {
    InvokeCall {
        module: "mail".to_string(),
        action: action.to_string(),
        entity: None,
        input,
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item,
        request_id,
    }
}

/// A `len`-byte value in the uuid spelling, so the only thing that varies between the two
/// probe calls is the length.
fn uuid_shaped(len: usize) -> String {
    (0..len)
        .map(|i| match i {
            8 | 13 | 18 | 23 => '-',
            _ => 'a',
        })
        .collect()
}

fn refused(call: InvokeCall) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let provider = {
        let _guard = rt.enter();
        let cfg = MailConfig {
            provider: None,
            send_timeout: std::time::Duration::from_millis(DEFAULT_SEND_TIMEOUT_MS),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retention_days: DEFAULT_RETENTION_DAYS,
        };
        MailAdmin::new(Arc::new(Service::new(dead_pool())), &cfg)
    };
    let outcome: Result<InvokeOutcome, opsapi::Error> = rt.block_on(provider.invoke(call));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that reaches the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

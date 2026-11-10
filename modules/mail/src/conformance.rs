//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. These probes only exercise the same
//! production validators a durable delivery traverses — a fixture that restated a rule
//! instead of driving it would stay green after the rule was deleted.

use crate::service::{validate_new, NewMail};

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

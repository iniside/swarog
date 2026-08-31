//! `mailevents` — the published event vocabulary of the "mail" domain. Any module that
//! wants an outbound email sent imports this and appends `SEND_REQUESTED`; nobody imports
//! the mail implementation.
//!
//! The topic rides the **durable** plane. `mail` both defines and subscribes to it — a
//! deliberate deviation from "publisher owns the event, consumer owns the subscription":
//! the topic is a *command* ("send this"), not a *fact* about another domain, and the
//! alternative (one topic per sending module) would mean editing `mail` for every new
//! sender.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// RFC 5321 total-address max — matches `accounts`' `MAX_EMAIL_BYTES`.
pub const MAX_ADDRESS_BYTES: usize = 320;
/// Matches `notifications`' `MAX_TITLE_BYTES`.
pub const MAX_SUBJECT_BYTES: usize = 200;
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_KIND_BYTES: usize = 64;
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// Provider names a sender is registered under. The name arrives from `MAIL_PROVIDER` at
/// boot, never from an anonymous caller over the wire.
pub mod providers {
    pub const LOG: &str = "log";
    pub const SMTP: &str = "smtp";
}

/// A request to send one email, keyed by `idempotency_key` for exactly-once enqueue.
/// Delivery to the recipient is at-least-once — see the plan's "delivery contract" note.
///
/// Evolve additively (constraint #6): add fields or a `SendRequestedV2`, never reshape —
/// the retained durable JSON is the contract. No `Option<…>` field, deliberately: every
/// field is always meaningful, and contract-golden requires a second `None`-populated
/// sample for any optional one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequested {
    pub idempotency_key: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub kind: String,
}

/// `MinRetention { days: 7 }`: the durable log is the DELIVERY path, not the archive —
/// the authority for what was sent is `mail.outbox`. The history policy is IMMUTABLE
/// after the first emit.
pub static SEND_REQUESTED: LazyLock<EventType<SendRequested>> = LazyLock::new(|| {
    define(
        "mail.send_requested",
        1,
        HistoryPolicy::MinRetention { days: 7 },
    )
});

/// Fully-POPULATED wire sample per defined `(topic, version)`: every field set, so serde's
/// actual JSON keys land in the golden and a silent `#[serde(rename)]` or a reshaped field
/// fails the blocking stage instead of poisoning retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![(
        "mail.send_requested",
        1,
        serde_json::to_value(SendRequested {
            idempotency_key: "verify-player-1".to_string(),
            to: "player@example.com".to_string(),
            subject: "Verify your address".to_string(),
            body: "Click the link to verify.".to_string(),
            kind: "verification".to_string(),
        })
        .expect("SendRequested serializes to json"),
    )]
}

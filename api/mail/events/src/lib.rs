//! `mailevents` — the published event vocabulary of the "mail" domain. Any module that
//! wants an outbound email sent imports this and appends `SEND_REQUESTED`; nobody imports
//! the mail implementation.
//!
//! The topic rides the **durable** plane. `mail` both defines and subscribes to it — a
//! deliberate deviation from "publisher owns the event, consumer owns the subscription":
//! the topic is a *command* ("send this"), not a *fact* about another domain, and the
//! alternative (one topic per sending module) would mean editing `mail` for every new
//! sender.
//!
//! **Accepted risk: `body` carries rendered content, which may be a secret.** "The sender
//! renders, mail transports" (no template registry here) means a caller filling in a
//! password-reset link or verification token puts that plaintext into the durable log —
//! `eventctl` prints it verbatim, and retention governs only already-consumed events, so
//! it survives at least as long as the slowest subscriber. The mitigation lives on the
//! storage side, not the contract: `mail`'s outbox blanks `body` once a row reaches `sent`,
//! and `SEND_REQUESTED`'s history policy is the minimum residency the delivery plane needs,
//! not an archival window. A future template-reference field would remove the plaintext
//! from this payload but push every sender's data shape into the transport — the coupling
//! this crate exists to avoid — so it is deferred, not silently assumed away.
//!
//! That mitigation costs one property of THIS contract, not of any one implementation:
//! once a delivered request's body has been dropped, a later event reusing its
//! `idempotency_key` with genuinely different content is indistinguishable from a replay
//! of the delivered one and is discarded as such, with nothing reported. Reusing one key
//! for two different messages is a producer bug in every case; after delivery it is a
//! silent one, so a producer must mint a fresh key per message rather than per attempt.

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

/// A request to send one email. Enqueue is exactly-once per `idempotency_key`; delivery to
/// the recipient is at-least-once — a crash between the provider accepting the message and
/// the status commit re-sends, so `body` must be safe for the recipient to receive twice.
///
/// Evolve additively (constraint #6): a new field needs `#[serde(default)]` or must be
/// `Option` (owing a second `None`-populated golden sample) — a retained pre-change event
/// has no key for it, so a bare required field fails `decode` and pauses every subscription
/// on this topic. Anything that can't satisfy that is a `SendRequestedV2`, not a field add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequested {
    pub idempotency_key: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub kind: String,
}

/// `MinRetention { days: 1 }`: the minimum secret residency the delivery plane needs, not
/// an archival window — see the crate doc's accepted-risk paragraph. Retention is
/// checkpoint-coupled (governs only already-consumed events), so this bounds how long a
/// delivered event's plaintext body can still be sitting unconsumed, not how long a
/// consumed one is kept. The authority for what was sent is `mail.outbox`. The history
/// policy is IMMUTABLE after the first emit.
pub static SEND_REQUESTED: LazyLock<EventType<SendRequested>> =
    LazyLock::new(|| define("mail.send_requested", 1, HistoryPolicy::MinRetention { days: 1 }));

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

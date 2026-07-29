//! `walletevents` — the published event vocabulary of the "wallet" domain. Anyone who
//! reacts to money moving imports this; nobody imports the wallet implementation.
//!
//! The topic rides the **durable** plane, emitted INSIDE the transaction that appends the
//! `wallet.ledger` row and updates the balance — so the event is durable iff the movement
//! is.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires once per APPLIED movement — never on the idempotent-replay branch, which moves no
/// money. `delta` is signed (negative for a debit) and `ledger_id` points at the
/// `wallet.ledger` row that is the authority for the movement.
///
/// Evolve additively (constraint #6): add fields or a `ChangedV2`, never reshape — the
/// retained durable JSON is the contract. No `Option<…>` field, deliberately: every field
/// is always meaningful, and contract-golden requires a second `None`-populated sample for
/// any optional one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub player_id: String,
    pub currency: String,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    pub ledger_id: String,
}

/// `MinRetention { days: 30 }` matches `AUDIT_RETENTION_DAYS`'s default: the durable log is
/// the DELIVERY path, not the archive — the authority for money history is `wallet.ledger`.
/// The history policy is IMMUTABLE after the first emit.
pub static CHANGED: LazyLock<EventType<Changed>> =
    LazyLock::new(|| define("wallet.changed", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Fully-POPULATED wire sample per defined `(topic, version)`: every field set, so serde's
/// actual JSON keys land in the golden and a silent `#[serde(rename)]` or a reshaped field
/// fails the blocking stage instead of poisoning retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![(
        "wallet.changed",
        1,
        serde_json::to_value(Changed {
            player_id: "player-1".to_string(),
            currency: "gold".to_string(),
            delta: -25,
            balance_after: 75,
            reason: "admin-revoke".to_string(),
            ledger_id: "ledger-1".to_string(),
        })
        .expect("Changed serializes to json"),
    )]
}

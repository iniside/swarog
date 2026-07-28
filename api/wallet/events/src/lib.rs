//! `walletevents` — the published event vocabulary of the "wallet" domain. Anyone who
//! reacts to money moving (audit: record the ledger row) imports this; nobody imports
//! the wallet implementation.
//!
//! The topic rides the **durable** plane: wallet `emit_tx`s it INSIDE the same
//! transaction that appends the `wallet.ledger` row and updates the balance, so the
//! event is durable iff the movement is, and a cross-process consumer receives it by
//! pulling from its own checkpointed subscription against the shared event log.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires once per APPLIED money movement — never on the idempotent-replay branch, which
/// moves no money. `delta` is signed (negative for a debit), `balance_after` is the
/// player's resulting balance in that currency, and `ledger_id` points at the
/// append-only `wallet.ledger` row that is the authority for the movement.
///
/// No `Option<…>` field, deliberately: every field is always meaningful for an applied
/// movement, and contract-golden requires a second `None`-populated sample for any
/// optional field. Evolve additively (constraint #6): add fields / a `ChangedV2`, never
/// reshape — the retained durable JSON is the contract.
///
/// `Serialize`/`Deserialize` are load-bearing: the durable transport collapses the
/// payload to JSON at the `emit_tx`/`on_tx` boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub player_id: String,
    pub currency: String,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    pub ledger_id: String,
}

/// The `wallet.changed` topic. `MinRetention { days: 30 }` matches `AUDIT_RETENTION_DAYS`'s
/// default: the durable log is the DELIVERY path, not the archive — the authority for
/// money history is `wallet.ledger`, a real table we keep — so `KeepForever` would be
/// hoarding. The history policy is IMMUTABLE after the first emit.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers pass
/// it as `&*walletevents::CHANGED` (or just `&walletevents::CHANGED`, which auto-derefs).
pub static CHANGED: LazyLock<EventType<Changed>> =
    LazyLock::new(|| define("wallet.changed", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Fully-POPULATED wire sample for the contract-golden fingerprint: every field set so
/// serde's actual JSON keys land in the golden. `contract-golden` flattens this into
/// `payload.<key>:<type>` lines; a silent `#[serde(rename)]` or a reshaped field then
/// fails the blocking stage instead of poisoning retained durable JSON. One entry per
/// defined `(topic, version)`.
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

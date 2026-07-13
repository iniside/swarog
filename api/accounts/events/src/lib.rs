//! `accountsevents` — the published event vocabulary of the "accounts" domain (port
//! of Go's `api/accounts/accountsevents`). Anyone who reacts to player lifecycle
//! imports this; nobody imports the accounts implementation.
//!
//! Deliberate deviation from Go (the durable-events rule, plan Step 6): Go emitted
//! `player.registered` on the plain sync bus; here it rides the **durable** plane —
//! the accounts module `emit_tx`s it INSIDE the registration store transaction, so
//! the event is durable iff the player row is, and a cross-process consumer
//! (audit-svc from Step 8 on) receives it by pulling from its own checkpointed
//! subscription against the shared event log.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires the first time an identity provisions a NEW player — for any provider
/// (`"dev"` today, `"epic"` for first-sight OIDC logins). It carries our
/// product-scoped player id, never a provider's external id. Evolve additively
/// (constraint #6).
///
/// `Serialize`/`Deserialize` are load-bearing: the durable transport collapses the
/// payload to JSON at the `emit_tx`/`on_tx` boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerRegistered {
    pub player_id: String,
    pub display_name: String,
    pub provider: String,
}

/// The `player.registered` topic. Not yet consumed by any module (Go carried a
/// `topiccheck:allow-unsubscribed` for the same reason — match/rating wiring is a
/// later step; audit subscribes in Step 8).
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*accountsevents::PLAYER_REGISTERED`.
pub static PLAYER_REGISTERED: LazyLock<EventType<PlayerRegistered>> =
    LazyLock::new(|| define("player.registered", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Fully-POPULATED wire sample for the contract-golden fingerprint (Step 5): every
/// field set so serde's actual JSON keys land in the golden. `contract-golden`
/// flattens this into `payload.<key>:<type>` lines; a silent `#[serde(rename)]` or a
/// reshaped field then fails the blocking stage instead of poisoning retained durable
/// JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![(
        "player.registered",
        1,
        serde_json::to_value(PlayerRegistered {
            player_id: "player-1".to_string(),
            display_name: "Aria".to_string(),
            provider: "dev".to_string(),
        })
        .expect("PlayerRegistered serializes to json"),
    )]
}

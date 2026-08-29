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

/// Fires the first time an identity provisions a NEW player. `provider` is the
/// `accounts.identities.provider` column value the provisioning path wrote — NOT the
/// credential-verifier registry, which is a strictly narrower set: the federated paths
/// write a registered provider name (`epic`, `google`, `guest` today), while
/// dev/password registration writes `dev`, a provider no verifier exists for. A
/// consumer filtering this field must treat accounts' provider-name constants as the
/// authority, not the verifier registry. It carries our product-scoped player id, never
/// a provider's external id. Evolve additively (constraint #6).
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

/// Fires when a player that held ONLY guest identities gains its first non-guest
/// identity — the promotion a consumer treats as "this player became real". Emitted
/// in the same transaction as the identity row, so it is durable iff the link is.
///
/// `from_provider` is the constant `"guest"`: guest is the only promotable state this
/// build ships, and the emit condition is "had no non-guest identity", which today can
/// only mean guest-only. A second promotable origin would carry its own name here.
/// `to_provider` is the `accounts.identities.provider` value just linked.
///
/// `Serialize`/`Deserialize` are load-bearing: the durable transport collapses the
/// payload to JSON at the `emit_tx`/`on_tx` boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerPromoted {
    pub player_id: String,
    pub from_provider: String,
    pub to_provider: String,
}

/// The `player.promoted` topic. Retained 7 days like `player.registered`, so a
/// consumer subscribing `AfterRegistration` sees a promotion that happened while it
/// was down.
pub static PLAYER_PROMOTED: LazyLock<EventType<PlayerPromoted>> =
    LazyLock::new(|| define("player.promoted", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Fully-POPULATED wire sample for the contract-golden fingerprint (Step 5): every
/// field set so serde's actual JSON keys land in the golden. `contract-golden`
/// flattens this into `payload.<key>:<type>` lines; a silent `#[serde(rename)]` or a
/// reshaped field then fails the blocking stage instead of poisoning retained durable
/// JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![
        (
            "player.registered",
            1,
            serde_json::to_value(PlayerRegistered {
                player_id: "player-1".to_string(),
                display_name: "Aria".to_string(),
                provider: "dev".to_string(),
            })
            .expect("PlayerRegistered serializes to json"),
        ),
        (
            "player.promoted",
            1,
            serde_json::to_value(PlayerPromoted {
                player_id: "player-1".to_string(),
                from_provider: "guest".to_string(),
                to_provider: "epic".to_string(),
            })
            .expect("PlayerPromoted serializes to json"),
        ),
    ]
}

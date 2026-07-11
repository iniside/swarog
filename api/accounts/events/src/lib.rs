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

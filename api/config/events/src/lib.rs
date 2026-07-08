//! `configevents` — the published contract of the config domain (port of Go's
//! `api/config/configevents`): the `config.changed` event the listener fires after
//! a setting write is observed. It is the ONLY surface other modules share with
//! config (payload + descriptor).
//!
//! Emitted on the SYNC in-process bus (`bus::Bus::emit` / `on`), NOT the durable
//! plane — a `config.changed` is an eventually-consistent cache-refresh signal, not
//! a cross-process durable event, so it never touches the outbox.

use std::sync::LazyLock;

use bus::{define, EventType};
use serde::{Deserialize, Serialize};

/// Carries the namespaced setting that just changed and its new value. Evolve
/// additively (constraint #6): add fields / a `ChangedV2`, never reshape.
///
/// `Serialize`/`Deserialize` keep it wire-ready even though today it rides only the
/// sync bus (which needs neither), so a future durable/remote path costs nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub namespace: String,
    pub key: String,
    pub value: String,
}

/// The `config.changed` topic. The listener `emit`s it once the in-memory cache has
/// been refreshed with the new value, so a subscriber that re-pulls via the config
/// service sees the fresh value.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*configevents::CHANGED`.
pub static CHANGED: LazyLock<EventType<Changed>> = LazyLock::new(|| define("config.changed"));

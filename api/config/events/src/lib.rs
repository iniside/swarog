//! `configevents` — the published contract of the config domain (port of Go's
//! `api/config/configevents`): the `config.changed` event the listener fires after
//! a setting write is observed. It is the ONLY surface other modules share with
//! config (payload + descriptor).
//!
//! As of Step 5 this rides the **durable** plane (`bus::emit_tx` / `on_tx`), NOT the
//! sync bus: under the fortress topology config lives in its own process, so the
//! cache-refresh signal must cross a process boundary (config-svc's outbox → POST
//! `/events` → inventory-svc's cache + starter-spec reload). The config listener is
//! the sole producer (`emit_tx` in its own short tx); consumers subscribe with a
//! stable name (`on_tx(..., "inventory")` / `"config-cache"`).

use std::sync::LazyLock;

use bus::{define, EventType};
use serde::{Deserialize, Serialize};

/// Carries the namespaced setting that just changed and its new value. Evolve
/// additively (constraint #6): add fields / a `ChangedV2`, never reshape.
///
/// `Serialize`/`Deserialize` are load-bearing: the durable transport collapses the
/// payload to JSON at the `emit_tx`/`on_tx` boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub namespace: String,
    pub key: String,
    pub value: String,
}

/// The `config.changed` topic. The listener `emit_tx`s it once the in-memory cache has
/// been refreshed with the new value, so a subscriber that re-pulls via the config
/// service sees the fresh value.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*configevents::CHANGED`.
pub static CHANGED: LazyLock<EventType<Changed>> = LazyLock::new(|| define("config.changed"));

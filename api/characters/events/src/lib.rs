//! `charactersevents` — the published event contract of the "characters" domain
//! (port of Go's `api/characters/charactersevents`). Anyone reacting to character
//! lifecycle (e.g. `inventory`: grant a starter item on create, wipe holdings on
//! delete) imports this; nobody imports the characters implementation.
//!
//! Unlike `configevents` (sync in-process bus), these ride the **durable** plane
//! (`bus::emit_tx` / `bus::on_tx`), atomic with the domain write — so the payloads
//! are `Serialize`/`Deserialize` (the transport collapses `T` to JSON at the
//! emit_tx/on_tx boundary). The serde field names (`character_id`, `player_id`, …)
//! are the wire contract: the producer (characters) and every durable consumer
//! (inventory, Step 9) must agree on them.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires when a player creates a character. Evolve additively (constraint #6): add
/// fields / a `CreatedV2`, never reshape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    pub character_id: String,
    pub player_id: String,
    pub name: String,
    pub class: String,
}

/// Fires when a character is removed. Consumers (e.g. inventory) use it to clean up
/// their own data for that character — no cross-module foreign key needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    pub character_id: String,
    pub player_id: String,
}

/// The `character.created` topic. Emitted via `bus::emit_tx` inside the domain tx
/// that inserts the character row, so the event is durable iff the character is.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*charactersevents::CREATED` (or just `&charactersevents::CREATED`,
/// which auto-derefs).
pub static CREATED: LazyLock<EventType<Created>> =
    LazyLock::new(|| define("character.created", 1, HistoryPolicy::MinRetention { days: 7 }));

/// The `character.deleted` topic. Emitted (in the same tx as the delete) only when a
/// row was actually removed — a delete of a non-owned/absent character emits nothing.
pub static DELETED: LazyLock<EventType<Deleted>> =
    LazyLock::new(|| define("character.deleted", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Fully-POPULATED wire samples for the contract-golden fingerprint (Step 5): one
/// entry per payload struct, every field set (so serde's actual JSON keys — not the
/// Rust field names cargo-public-api sees — land in the golden). `contract-golden`
/// flattens each value into `payload.<key>:<type>` lines; a silent `#[serde(rename)]`
/// or a reshaped field then fails the blocking stage instead of poisoning retained
/// durable JSON. Keep every field populated (Options `Some`, collections non-empty).
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![
        (
            "character.created",
            1,
            serde_json::to_value(Created {
                character_id: "char-1".to_string(),
                player_id: "player-1".to_string(),
                name: "Aria".to_string(),
                class: "mage".to_string(),
            })
            .expect("Created serializes to json"),
        ),
        (
            "character.deleted",
            1,
            serde_json::to_value(Deleted {
                character_id: "char-1".to_string(),
                player_id: "player-1".to_string(),
            })
            .expect("Deleted serializes to json"),
        ),
    ]
}

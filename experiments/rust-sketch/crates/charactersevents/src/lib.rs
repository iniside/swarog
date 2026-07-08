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

use bus::{define, EventType};
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
pub static CREATED: LazyLock<EventType<Created>> = LazyLock::new(|| define("character.created"));

/// The `character.deleted` topic. Emitted (in the same tx as the delete) only when a
/// row was actually removed — a delete of a non-owned/absent character emits nothing.
pub static DELETED: LazyLock<EventType<Deleted>> = LazyLock::new(|| define("character.deleted"));

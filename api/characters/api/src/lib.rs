//! `charactersapi` — the characters module's PURE, transport-free capability
//! contract (port of Go's `api/characters/charactersapi`). It declares the three
//! capabilities characters exposes and applies `#[rpc(prefix = "characters")]` to
//! the two wire capabilities so the transport-FREE surface (per-method wire
//! envelopes, `METHOD_*` consts, and — for `#[http]` methods —
//! `operations()`/`route_bindings()`) is GENERATED into child `*_rpc` modules
//! rather than hand-written. The edge-dependent glue (`Client`, `register_server`,
//! `provide_remote`) lives in the sibling `charactersrpc` crate, which expands this
//! crate's metadata-callback macros (`characters_ownership_meta!` /
//! `characters_player_meta!`) — so THIS crate never depends on `edge`.
//!
//! The identity convention (see `rpc_macro`): a method needing the caller's VERIFIED
//! player identity declares a leading `opsapi::Identity` parameter — the macro strips
//! it from the wire body and reconstructs it from the mutually-authenticated envelope
//! on the server side. `Ownership::owner_of` is wire-only (no identity, no `#[http]`);
//! `Player`'s create/list/delete are identity-bearing and HTTP-bound.
//!
//! Domain CONSUMERS import this ONLY to name a trait for `registry::require`
//! (rule-4's nominal-typing cost); they never import the `characters` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// A player-owned character. `player_id` is a plain reference to accounts.players —
/// no cross-module foreign key (logical isolation). It lives here (not in the impl
/// crate) because it is a return type of the `Player` capability, so the generated
/// glue must be able to name it; the `characters` module re-exports it.
///
/// `created_at` is carried as a String (the Postgres `timestamptz::text` rendering),
/// not a typed timestamp — the sketch keeps the value opaque as it flows through the
/// wire/admin; the field names match Go's JSON tags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Character {
    pub id: String,
    pub player_id: String,
    pub name: String,
    pub class: String,
    pub created_at: String,
}

/// Resolves a character's owning player. `owner_of` is the sync capability a peer's
/// inventory resolves over the QUIC edge to authorize a character's inventory. It is
/// WIRE-ONLY: no leading `Identity` (unauthenticated) and no `#[http]` (not a
/// gateway route). Returns `Ok(None)` for a genuine "no such character" so a
/// transport failure (an `Err`) surfaces distinctly from a real miss — the Rust twin
/// of Go's `(playerID, ok, err)`.
#[rpc(prefix = "characters")]
#[async_trait]
pub trait Ownership: Send + Sync {
    async fn owner_of(&self, character_id: String) -> Result<Option<String>, Error>;
}

/// The characters module's player-facing capability: the three operations a player
/// performs on their OWN characters. Each takes its caller identity as the leading
/// `Identity` param (injected by the gateway after bearer verification), NEVER a
/// body field — so a client cannot act as another player. The `#[http]` bindings are
/// the single source the gateway route table + both backends read.
#[rpc(prefix = "characters")]
#[async_trait]
pub trait Player: Send + Sync {
    /// Create a character owned by the caller. `class` defaults to `"novice"` when
    /// empty (in the impl). 201 on success.
    #[http(verb = "POST", path = "/characters", auth = "player", success = 201)]
    async fn create(&self, identity: Identity, name: String, class: String) -> Result<Character, Error>;

    /// List the caller's own characters. 200.
    #[http(verb = "GET", path = "/characters", auth = "player", success = 200)]
    async fn list(&self, identity: Identity) -> Result<Vec<Character>, Error>;

    /// Delete one of the caller's characters; deleting a non-owned/absent character
    /// is `Status::NotFound` (→ 404). The `{id}` path wildcard rides into the wire
    /// field `character_id`. 204 (no body) on success.
    #[http(verb = "DELETE", path = "/characters/{id}", auth = "player", success = 204, path_args(character_id = "id"))]
    async fn delete(&self, identity: Identity, character_id: String) -> Result<(), Error>;
}

/// The characters module's admin fan-out capability: returns this module's admin
/// page (KPIs + table) as `adminapi::ItemData`. In Milestone 1 it is a plain
/// capability the module implements and the LOCAL admin contribution reads — NO
/// `#[rpc]` (the edge admin fan-out is Milestone 2). No player identity is involved.
#[async_trait]
pub trait Admin: Send + Sync {
    async fn admin_data(&self) -> Result<adminapi::ItemData, Error>;
}

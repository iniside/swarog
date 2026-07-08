//! `inventoryapi` — the inventory module's pure, transport-free capability contract
//! (port of Go's `api/inventory/inventoryapi`). It declares the `Holdings`
//! capability — the three operations a player performs against their OWN inventory —
//! and applies `#[rpc(prefix = "inventory")]` so the transport glue (per-method wire
//! envelopes, `Method*` consts, a `Client` over `opsapi::Caller`, `register_server`,
//! and — for `#[http]` methods — `operations()`/`route_bindings()`) is GENERATED into
//! the child `holdings_rpc` module rather than hand-written.
//!
//! The identity convention (see `rpc_macro`): every `Holdings` method needs the
//! caller's VERIFIED player identity, so each declares a leading `opsapi::Identity`
//! parameter — the macro strips it from the wire body and reconstructs it from the
//! mutually-authenticated envelope on the server side, so a client can NEVER read or
//! mutate another player's inventory. All three methods are HTTP-bound.
//!
//! Domain CONSUMERS do not import this crate (rule 4's nominal-typing note): it is
//! reached only by the generated glue and the `remote` stub — the provider-owned
//! contract surface, same precedent as each domain's `<module>events` crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One item stack an owner holds. It lives here (not in the impl crate) because it is
/// the return type of the `Holdings` capability, so the generated glue must be able
/// to name it; the `inventory` module uses it directly. The serde field names
/// (`owner_type`/`owner_id`/`item_id`/`item_name`/`quantity`) are the player-facing
/// wire shape — matching Go's JSON tags exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub owner_type: String,
    pub owner_id: String,
    pub item_id: String,
    pub item_name: String,
    pub quantity: i64,
}

/// The inventory module's player-facing capability: the three operations a player
/// performs against their OWN inventory. Each takes its caller `Identity` as the
/// leading parameter (injected by the gateway after bearer verification), NEVER a
/// body field — so a client cannot act on another player's inventory. The `#[http]`
/// bindings are the single source the gateway route table + both backends read;
/// verbs/paths/success/body-keys mirror Go's `inventoryapi.HTTPBindings` verbatim.
#[rpc(prefix = "inventory")]
#[async_trait]
pub trait Holdings: Send + Sync {
    /// The caller's own (player-owned) holdings. 200.
    #[http(verb = "GET", path = "/inventory/me", auth = "player", success = 200)]
    async fn list_mine(&self, identity: Identity) -> Result<Vec<Holding>, Error>;

    /// A character's holdings, but only if the caller OWNS the character — otherwise
    /// a Forbidden outcome (never another player's inventory). A genuinely unknown
    /// character is NotFound; an ownership-lookup transport failure is Unavailable.
    /// The `{id}` path wildcard rides into the wire field `character_id`. 200.
    #[http(
        verb = "GET",
        path = "/inventory/character/{id}",
        auth = "player",
        success = 200,
        path_args(character_id = "id")
    )]
    async fn list_character(&self, identity: Identity, character_id: String) -> Result<Vec<Holding>, Error>;

    /// Adds `qty` of `item_id` to the caller's own inventory (the simulated-IAP path,
    /// gated by `INVENTORY_DEV_GRANT`). A non-positive qty or an unknown item is an
    /// Invalid outcome. The body key stays `item_id` (matching Go's `BodyNames`
    /// override). Returns the caller's updated holdings. 200.
    #[http(
        verb = "POST",
        path = "/inventory/me/grant",
        auth = "player",
        success = 200,
        body_names(item_id = "item_id")
    )]
    async fn grant(&self, identity: Identity, item_id: String, qty: i64) -> Result<Vec<Holding>, Error>;
}

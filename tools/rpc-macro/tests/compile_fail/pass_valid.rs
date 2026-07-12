//! A valid `#[rpc]` trait exercising both `path_args` (placeholder in exact bijection
//! with the mapped param) and `body_names` (a real param renamed). Must compile.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Holding {
    pub item_id: String,
    pub quantity: i64,
}

#[rpc(prefix = "inventory")]
#[async_trait]
pub trait Holdings: Send + Sync {
    #[http(
        verb = "GET",
        path = "/inventory/character/{id}",
        auth = "player",
        success = 200,
        path_args(character_id = "id")
    )]
    async fn list_character(&self, identity: Identity, character_id: String) -> Result<Vec<Holding>, Error>;

    #[http(
        verb = "POST",
        path = "/inventory/me/grant",
        auth = "player",
        success = 200,
        body_names(item_id = "sku")
    )]
    async fn grant(&self, identity: Identity, item_id: String, qty: i64) -> Result<Vec<Holding>, Error>;
}

fn main() {}

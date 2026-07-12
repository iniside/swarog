//! `body_names` key `item` names no parameter of `grant` (the param is `item_id`) —
//! must fail to compile.
#![allow(unused_imports)]

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;

#[rpc(prefix = "inventory")]
#[async_trait]
pub trait Holdings: Send + Sync {
    #[http(
        verb = "POST",
        path = "/inventory/me/grant",
        auth = "player",
        success = 200,
        body_names(item = "sku")
    )]
    async fn grant(&self, identity: Identity, item_id: String, qty: i64) -> Result<(), Error>;
}

fn main() {}

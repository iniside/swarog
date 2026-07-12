//! `path_args` key `character` names no parameter of `delete` (the param is
//! `character_id`) — must fail to compile.
#![allow(unused_imports)]

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;

#[rpc(prefix = "characters")]
#[async_trait]
pub trait Player: Send + Sync {
    #[http(
        verb = "DELETE",
        path = "/characters/{id}",
        auth = "player",
        success = 204,
        path_args(character = "id")
    )]
    async fn delete(&self, identity: Identity, character_id: String) -> Result<(), Error>;
}

fn main() {}

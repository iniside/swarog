//! The path template has a `{id}` placeholder but `path_args` maps the param to a
//! different wildcard (`cid`), so the placeholder is fed by nothing and the value
//! fills nothing — must fail to compile.
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
        path_args(character_id = "cid")
    )]
    async fn delete(&self, identity: Identity, character_id: String) -> Result<(), Error>;
}

fn main() {}

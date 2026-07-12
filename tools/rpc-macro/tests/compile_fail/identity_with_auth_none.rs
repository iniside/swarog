#![allow(unused_imports)]

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;

#[rpc(prefix = "bad")]
#[async_trait]
trait Bad: Send + Sync {
    #[http(verb = "GET", path = "/bad", auth = "none", success = 200)]
    async fn get(&self, identity: Identity) -> Result<(), Error>;
}

fn main() {}

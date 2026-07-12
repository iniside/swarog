#![allow(unused_imports)]

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;

#[rpc(prefix = "bad")]
#[async_trait]
trait Bad: Send + Sync {
    #[http(verb = "GET", path = "/bad", auth = "none", success = 199)]
    async fn get(&self) -> Result<(), Error>;
}

fn main() {}

//! Harness crate for the `#[rpc]` proc-macro. This lib target plays the `<name>api`
//! role of the Step-2 split: it declares the sample capability trait, so `#[rpc]`
//! generates the PURE surface (wire envelopes, `METHOD_*` consts,
//! `operations()`/`route_bindings()`) into `sample_rpc` plus the
//! `sample_sample_meta!` metadata-callback macro. The integration test
//! (`tests/roundtrip.rs`) plays the `<name>rpc` role: it expands the callback macro
//! through `rpc_macro::generate_glue` — an integration test is a SEPARATE crate, so
//! the two-crate metadata handoff is exercised for real — and drives the generated
//! Client / register_server / operations end-to-end (over real edge QUIC and via
//! the gateway decode/invoke/encode glue).

use async_trait::async_trait;
use opsapi::{Error, Identity};
use serde::{Deserialize, Serialize, Serializer};

// --- Domain types the sample capability exchanges ---------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub item_id: String,
    pub qty: i64,
    pub owner: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub player_id: String,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct FailingValue;

impl Serialize for FailingValue {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(serde::ser::Error::custom(
            "intentional failing Serialize fixture",
        ))
    }
}

// --- The sample capability trait --------------------------------------------
//
// `#[rpc]` sits ABOVE `#[async_trait]` so it parses the `async fn`s first, then
// re-emits the trait for async_trait to desugar.

#[rpc_macro::rpc(prefix = "sample")]
#[async_trait]
pub trait Sample: Send + Sync {
    /// HTTP-bound, needs identity, returns a Vec (a body arg + identity).
    #[http(verb = "POST", path = "/sample/grant", auth = "player", success = 200)]
    async fn grant(&self, caller: Identity, item_id: String, qty: i64)
        -> Result<Vec<Holding>, Error>;

    /// HTTP-bound with a PATH arg + identity; a good Err-branch probe.
    #[http(
        verb = "GET",
        path = "/sample/character/{id}",
        auth = "player",
        success = 200,
        path_args(character_id = "id")
    )]
    #[retry_safe]
    async fn list_character(
        &self,
        caller: Identity,
        character_id: String,
    ) -> Result<Vec<Holding>, Error>;

    /// Wire-only (no `#[http]`), unauthenticated: no identity param, marshals all
    /// args. Mirrors characters' `OwnerOf`.
    #[retry_safe]
    async fn owner_of(&self, character_id: String) -> Result<Owner, Error>;

    /// Wire-only method returning an `Option<T>` — the exact shape of characters'
    /// real `Ownership::owner_of` (`Result<Option<String>, Error>`). This is the
    /// regression probe for the response-envelope `Option<Option<T>>` collapse: an
    /// `Ok(None)` MUST round-trip as a genuine `None`, NOT surface as a transport /
    /// internal error.
    async fn find_owner(&self, character_id: String) -> Result<Option<String>, Error>;

    /// Wire-only regression fixture: the implementation succeeds, but its return
    /// value deliberately cannot be serialized into the generated response.
    async fn failing_value(&self) -> Result<FailingValue, Error>;
}

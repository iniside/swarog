//! `partyrpc` — the party domain's GENERATED transport glue: the edge-dependent half of
//! the `#[rpc]` codegen, expanded from `partyapi`'s metadata-callback macro. The
//! `player_rpc` module carries the `Client`, `register_server` and `provide_remote`, and
//! `pub use`s the api crate's pure module, so `partyrpc::player_rpc::*` is a drop-in
//! superset.
//!
//! Rule 5: reached ONLY by the `party` module itself (own glue is sanctioned) and `cmd/*`
//! binaries — never by a domain consumer, which imports `partyapi` to name the trait.

// The glue's method signatures re-resolve at THIS invocation site (the metadata travels
// as tokens), so `partyapi`'s domain types and the identity/error types must be in scope
// here exactly as they are there.
use opsapi::{Error, Identity};
use partyapi::*;

partyapi::party_player_meta!(rpc_macro::generate_glue);

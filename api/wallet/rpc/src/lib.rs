//! `walletrpc` — the wallet domain's GENERATED transport glue: the edge-dependent half of
//! the `#[rpc]` codegen, expanded from `walletapi`'s metadata-callback macros. Each
//! `<snake>_rpc` module carries the `Client`, `register_server` and `provide_remote`, and
//! `pub use`s the api crate's pure module, so `walletrpc::<snake>_rpc::*` is a drop-in
//! superset.
//!
//! Rule 5: reached ONLY by the `wallet` module itself (own glue is sanctioned) and `cmd/*`
//! binaries — never by a domain consumer, which imports `walletapi` to name a trait.
//!
//! There is deliberately NO `remote_factories()` / `provide_factories()` aggregator:
//! nothing turns a wallet peer's capability into a registry entry — gateway-svc contributes
//! wallet's HTTP routes through `remote::Stub::describe_peer`, admin-svc pulls the admin
//! page through `adminrpc::admin_remote_factory`, and the monolith hosts the real service.

// The glue's method signatures re-resolve at THIS invocation site (the metadata travels as
// tokens), so `walletapi`'s domain types and the identity/error types must be in scope here
// exactly as they are there.
use opsapi::{Error, Identity};
use walletapi::*;

walletapi::wallet_wallet_meta!(rpc_macro::generate_glue);
walletapi::wallet_player_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registrations, re-exported so the `wallet` module
/// reaches them through its OWN glue crate: archcheck forbids a module→foreign-rpc edge,
/// while this module→own-rpc→adminrpc chain is fine.
pub use adminrpc::{register_admin, register_admin_submit};

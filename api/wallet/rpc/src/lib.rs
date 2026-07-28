//! `walletrpc` — the wallet domain's GENERATED transport glue: the edge-dependent half
//! of the `#[rpc]` codegen, split out of the pure `walletapi` contract.
//!
//! Each `<snake>_rpc` module below is expanded from `walletapi`'s metadata-callback
//! macro through [`rpc_macro::generate_glue`] and contains the `Client` (the
//! split-topology edge client implementing the source trait over an [`opsapi::Caller`]),
//! `register_server` (installs one `edge::IdentityHandler` adapter per method), and
//! `provide_remote` (provides the `Client` under the capability's canonical registry
//! key). Each also `pub use`s the api crate's pure module, so `walletrpc::<snake>_rpc::*`
//! is a drop-in superset.
//!
//! Rule 5: this crate is reached ONLY by the `wallet` module itself (a module importing
//! its OWN glue is sanctioned) and `cmd/*` binaries — never by a domain consumer (they
//! import `walletapi` to name a trait, rule 4).
//!
//! There is deliberately NO `remote_factories()` / `provide_factories()` aggregator:
//! every existing aggregator exists because some process turns a peer's capability into
//! a registry entry, and nothing does that for wallet — gateway-svc contributes wallet's
//! HTTP routes through `remote::Stub::describe_peer`, admin-svc pulls the wallet admin
//! page through `adminrpc::admin_remote_factory`, and the monolith hosts the real
//! service locally. An aggregator with zero callers is dead code, not symmetry.

// The glue's method signatures re-resolve at THIS invocation site (the metadata travels
// as tokens), so the api crate's domain types + the identity/error types must be in
// scope here exactly as they are in `walletapi`'s lib.rs.
use opsapi::{Error, Identity};
use walletapi::*;

walletapi::wallet_wallet_meta!(rpc_macro::generate_glue);
walletapi::wallet_player_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registrations, re-exported from `adminrpc` so the
/// `wallet` module registers `admin.adminData` (read) AND `admin.adminSubmit` (opt-in
/// remote write) through its OWN glue crate — never importing `adminrpc` directly
/// (archcheck forbids a module→foreign-rpc edge; this module→own-rpc→adminrpc chain is
/// fine). Both are wired into wallet's edge face when its admin page lands.
pub use adminrpc::{register_admin, register_admin_submit};

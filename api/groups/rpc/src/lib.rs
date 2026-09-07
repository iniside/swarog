//! `groupsrpc` — the groups domain's GENERATED transport glue: the edge-dependent half
//! of the `#[rpc]` codegen, expanded from `groupsapi`'s metadata-callback macros. The
//! `player_rpc`/`membership_rpc` modules each carry the `Client`, `register_server`
//! and `provide_remote`, and `pub use` the api crate's pure module, so
//! `groupsrpc::<snake>_rpc::*` is a drop-in superset.
//!
//! Rule 5: reached ONLY by the `groups` module itself (own glue is sanctioned) and
//! `cmd/*` binaries — never by a domain consumer, which imports `groupsapi` to name
//! the trait.
//!
//! No `remote_factories()` / `provide_factories()` aggregator YET — not a permanent
//! decision. An aggregator exists to turn a groups peer's capability into ANOTHER
//! process's registry entry, and no other process consumes one today: gateway-svc
//! contributes groups' HTTP routes through `remote::Stub::describe_peer`, admin-svc pulls
//! the admin page through `adminrpc::admin_remote_factory`, and the monolith hosts the
//! real service. The first [`groupsapi::Membership`] consumer is what adds one — the way
//! `accountsrpc::remote_factories` grew its `accounts.directory` entry when `friends`
//! needed `Directory`.

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so `groupsapi`'s domain types and the identity/error types must
// be in scope here exactly as they are there.
use groupsapi::*;
use opsapi::{Error, Identity};

groupsapi::groups_player_meta!(rpc_macro::generate_glue);
groupsapi::groups_membership_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registrations, re-exported so the `groups` module
/// reaches them through its OWN glue crate: archcheck forbids a module→foreign-rpc
/// edge, while this module→own-rpc→adminrpc chain is fine.
pub use adminrpc::{register_admin, register_admin_submit};

//! `notificationsrpc` — the notifications domain's GENERATED transport glue: the
//! edge-dependent half of the `#[rpc]` codegen, expanded from `notificationsapi`'s
//! metadata-callback macro. The `<snake>_rpc` module carries the `Client`,
//! `register_server` and `provide_remote`, and `pub use`s the api crate's pure module, so
//! `notificationsrpc::player_rpc::*` is a drop-in superset.
//!
//! Rule 5: reached ONLY by the `notifications` module itself (own glue is sanctioned) and
//! `cmd/*` binaries — never by a domain consumer, which imports `notificationsapi` to name
//! the trait.
//!
//! There is deliberately NO `remote_factories()` / `provide_factories()` aggregator:
//! nothing turns a notifications peer's capability into a registry entry — gateway-svc
//! contributes its HTTP routes through `remote::Stub::describe_peer`, admin-svc pulls the
//! admin page through `adminrpc::admin_remote_factory`, and the monolith hosts the real
//! service.

// The glue's method signatures re-resolve at THIS invocation site (the metadata travels as
// tokens), so `notificationsapi`'s domain types and the identity/error types must be in
// scope here exactly as they are there.
use notificationsapi::*;
use opsapi::{Error, Identity};

notificationsapi::notifications_player_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registrations, re-exported so the `notifications` module
/// reaches them through its OWN glue crate: archcheck forbids a module→foreign-rpc edge,
/// while this module→own-rpc→adminrpc chain is fine.
pub use adminrpc::{register_admin, register_admin_submit};

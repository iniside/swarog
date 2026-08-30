//! `accountsrpc` — the accounts domain's GENERATED transport glue (port of Go's
//! `api/accounts/accountsrpc` + `accountsauthrpc`): the edge-dependent half of the
//! `#[rpc]` codegen, split out of the pure `accountsapi` contract.
//!
//! Each `<snake>_rpc` module below is expanded from `accountsapi`'s
//! metadata-callback macro through [`rpc_macro::generate_glue`] and contains the
//! `Client` (the split-topology edge client implementing the source trait over an
//! [`opsapi::Caller`]), `register_server` (installs one `edge::IdentityHandler`
//! adapter per method), and `provide_remote` (provides the `Client` under the
//! capability's canonical registry key). It also `pub use`s the api crate's pure
//! module.
//!
//! Rule 5: this crate is reached ONLY by the `accounts` module itself (a module
//! importing its OWN glue is sanctioned), `remote`, and `cmd/*` binaries — never by
//! a domain consumer (the gateway's verifier adapter imports `accountsapi` to name
//! `dyn Sessions`, rule 4).

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's domain types + the identity/error types
// must be in scope here exactly as they are in `accountsapi`'s lib.rs.
use accountsapi::*;
use opsapi::{Error, Identity};

accountsapi::accounts_sessions_meta!(rpc_macro::generate_glue);
accountsapi::accounts_auth_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registration, re-exported from `adminrpc` so the
/// `accounts` module registers `admin.adminData` through its OWN glue crate (never a
/// foreign rpc import — archcheck-clean).
pub use adminrpc::register_admin;

/// The accounts provider's client-registration closures for a process where the
/// provider lives in a PEER process (accounts-svc). Consumed by [`remote::Stub`]:
/// the composition root (`cmd/*`) passes `accountsrpc::remote_factories()` into
/// `Stub::new`.
///
///   - `accounts.sessions` — the [`Sessions`] client, the capability the gateway's
///     verifier adapter resolves so bearer verification crosses the mTLS edge to
///     accounts-svc (closing the `DevSessionVerifier` trust hole in the split),
///   - `accounts.auth` — the [`Auth`] client, PLUS the auth ops'
///     `route_bindings()` into the gateway slots (no `LOCAL_SLOT` — no in-process
///     invoker exists, so the front dispatches every `Auth` op remotely).
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![
        Box::new(|ctx, caller| sessions_rpc::provide_remote(ctx.registry(), caller)),
        Box::new(|ctx, caller| {
            auth_rpc::provide_remote(ctx.registry(), caller);
            for rb in auth_rpc::route_bindings() {
                ctx.contribute(opsapi::SLOT, rb.operation);
                ctx.contribute(opsapi::BINDING_SLOT, rb.binding);
            }
        }),
    ]
}

/// The accounts provider's CAPABILITY-ONLY client-registration closures for a
/// data-driven front door (the D2 routing-as-data path). Dials the peer over the edge
/// EXACTLY as [`remote_factories`] does (same registry key, same generated `Client`),
/// but provides ONLY the capability the gateway resolves as a SYNC dep —
/// `accounts.sessions`, the [`Sessions`] client its bearer-verifier adapter `require`s —
/// and contributes NO `#[http]` route bindings.
///
/// Why a distinct entry point: a D2 gateway rebuilds its route table from each svc's
/// runtime `describe()`, so a factory that ALSO re-contributed the auth ops' static
/// routes to [`opsapi::SLOT`]/[`opsapi::BINDING_SLOT`] would collide with that describe
/// pass (`RouteTable::build` bails on a duplicate provider/method). The `Auth` ops
/// are therefore NEITHER provided as a `dyn Auth` client
/// NOR route-contributed here — the D2 front routes them over the edge from the describe
/// manifest, never via a typed capability `require`. Non-describe consumers keep using
/// [`remote_factories`] (provide + routes); this is ADDITIVE, not a replacement. (As of D2
/// `cmd/gateway-svc` is a describe consumer and calls THIS entry point.)
pub fn provide_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, caller| {
        sessions_rpc::provide_remote(ctx.registry(), caller)
    })]
}

#[cfg(test)]
mod tests;

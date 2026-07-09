//! `apikeysrpc` — the apikeys domain's GENERATED transport glue: the edge-dependent
//! half of the `#[rpc]` codegen, split out of the pure `apikeysapi` contract.
//!
//! The `keys_rpc` module below is expanded from `apikeysapi`'s metadata-callback
//! macro through [`rpc_macro::generate_glue`] and contains the `Client` (the
//! split-topology edge client implementing the source trait over an
//! [`opsapi::Caller`]), `register_server` (installs one `edge::IdentityHandler`
//! adapter per method), and `provide_remote` (provides the `Client` under the
//! capability's canonical registry key). It also `pub use`s the api crate's pure
//! module.
//!
//! Rule 5: this crate is reached ONLY by the `apikeys` module itself (a module
//! importing its OWN glue is sanctioned), `remote`, and `cmd/*` binaries — never by a
//! domain consumer (the gateway's key verifier adapter imports `apikeysapi` to name
//! `dyn Keys`, rule 4).

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's domain types + the error type must be in
// scope here exactly as they are in `apikeysapi`'s lib.rs.
use apikeysapi::*;
use opsapi::Error;

apikeysapi::apikeys_keys_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registration, re-exported from `adminrpc` so the
/// `apikeys` module registers `admin.adminData` through its OWN glue crate (never a
/// foreign rpc import — archcheck-clean). Wired into the edge face in Step 6.
pub use adminrpc::register_admin;

/// The apikeys provider's client-registration closure for a process where the provider
/// lives in a PEER process (apikeys-svc). Consumed by [`remote::Stub`]: the composition
/// root (`cmd/*`) passes `apikeysrpc::remote_factories()` into `Stub::new`.
///
///   - `apikeys.keys` — the [`Keys`] client, the capability the gateway's key verifier
///     adapter resolves so per-request key checks cross the mTLS edge to apikeys-svc.
///     Wire-only (no `#[http]` route bindings — the gateway never fronts a key op).
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, caller| {
        keys_rpc::provide_remote(ctx.registry(), caller)
    })]
}

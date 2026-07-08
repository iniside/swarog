//! `charactersrpc` — the characters domain's GENERATED transport glue (port of Go's
//! `api/characters/charactersrpc`): the edge-dependent half of the `#[rpc]` codegen,
//! split out of the pure `charactersapi` contract in the Step-2 fortress refactor.
//!
//! Each `<snake>_rpc` module below is expanded from `charactersapi`'s
//! metadata-callback macro through [`rpc_macro::generate_glue`] and contains the
//! `Client` (the split-topology edge client implementing the source trait over an
//! [`opsapi::Caller`]), `register_server` (installs one `edge::IdentityHandler`
//! adapter per method), and `provide_remote` (provides the `Client` under the
//! capability's canonical registry key). It also `pub use`s the api crate's pure
//! module, so `charactersrpc::<snake>_rpc::*` is a drop-in superset of the old
//! fused module.
//!
//! Rule 5: this crate is reached ONLY by the `characters` module itself (a module
//! importing its OWN glue is sanctioned), `remote`, and `cmd/*` binaries — never by
//! a domain consumer (they import `charactersapi` to name a trait, rule 4).

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's domain types + the identity/error types
// must be in scope here exactly as they are in `charactersapi`'s lib.rs.
use charactersapi::*;
use opsapi::{Error, Identity};

charactersapi::characters_ownership_meta!(rpc_macro::generate_glue);
charactersapi::characters_player_meta!(rpc_macro::generate_glue);

/// The characters provider's client-registration closures for a process where the
/// provider lives in a PEER process. Consumed by [`remote::Stub`]: the composition
/// root (`cmd/*`) passes `charactersrpc::remote_factories()` into `Stub::new`. The
/// canonical [`remote::RemoteFactory`] type is owned by `core/remote` (the crate that
/// applies these closures); this glue names it as a dependency:
///
///   - `characters.ownership` — the [`Ownership`] client (inventory's authz dep),
///   - `characters.player` — the [`Player`] client, PLUS the player ops'
///     `route_bindings()` into the gateway slots (no `LOCAL_SLOT` — no in-process
///     invoker exists, so the gateway dispatches these ops remotely).
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![
        Box::new(|ctx, caller| ownership_rpc::provide_remote(ctx.registry(), caller)),
        Box::new(|ctx, caller| {
            player_rpc::provide_remote(ctx.registry(), caller);
            for rb in player_rpc::route_bindings() {
                ctx.contribute(opsapi::SLOT, rb.operation);
                ctx.contribute(opsapi::BINDING_SLOT, rb.binding);
            }
        }),
    ]
}

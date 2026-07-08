//! `matchrpc` — the match domain's GENERATED transport glue (port of Go's
//! `api/match/matchrpc`): the edge-dependent half of the `#[rpc]` codegen, expanded
//! from `matchapi`'s metadata-callback macro through [`rpc_macro::generate_glue`]. The
//! `match_rpc` module below contains the `Client` (the split-topology edge client
//! implementing [`matchapi::Match`] over an [`opsapi::Caller`]), `register_server`
//! (installs one `edge::IdentityHandler` adapter per method), and `provide_remote`.
//!
//! Rule 5: this crate is reached ONLY by the `match` module itself (a module importing
//! its OWN glue is sanctioned), `remote`, and `cmd/*` binaries — never by a domain
//! consumer.

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's error type must be in scope exactly as in
// `matchapi`'s lib.rs. (No domain glob: `report` names no `matchapi` type beyond
// `Error` — the generated glue qualifies the trait itself via the `api =` param.)
use opsapi::Error;

matchapi::match_match_meta!(rpc_macro::generate_glue);

/// The match provider's client-registration closures for a process where the front
/// door lives elsewhere. Consumed by [`remote::Stub`]: the composition root
/// (`cmd/gateway-svc`) passes `matchrpc::remote_factories()` into `Stub::new`.
///
/// Match is fronted-only — no peer `require`s a match capability — so this contributes
/// the report `route_bindings()` ONLY (front-door routing; no `LOCAL_SLOT`, the gateway
/// dispatches `match.report` remotely to match-svc's edge) and makes no capability
/// provide.
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, _caller| {
        for rb in match_rpc::route_bindings() {
            ctx.contribute(opsapi::SLOT, rb.operation);
            ctx.contribute(opsapi::BINDING_SLOT, rb.binding);
        }
    })]
}

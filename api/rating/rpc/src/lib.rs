//! `ratingrpc` — the rating domain's GENERATED transport glue: the edge-dependent half
//! of the `#[rpc]` codegen, expanded from `ratingapi`'s metadata-callback macro through
//! [`rpc_macro::generate_glue`]. The `mmr_reader_rpc` module below contains the `Client`
//! (the split-topology edge client implementing [`ratingapi::MmrReader`] over an
//! [`opsapi::Caller`]), `register_server` (installs the edge adapter), and
//! `provide_remote` (provides the `Client` under the canonical `rating.mmr_reader` key).
//!
//! Rule 5: reached ONLY by the `rating` module itself (a module importing its OWN glue
//! is sanctioned), `remote`, and `cmd/*` binaries — never by a domain consumer (they
//! import `ratingapi` to name `dyn MmrReader`, rule 4).

// The glue's method signatures re-resolve at THIS invocation site, so the api crate's
// error type must be in scope exactly as in `ratingapi`'s lib.rs. (No domain glob:
// `mmr` returns a builtin `i64` and names no `ratingapi` type beyond `Error`.)
use opsapi::Error;

ratingapi::rating_mmr_reader_meta!(rpc_macro::generate_glue);

/// The rating provider's client-registration closures for a process where `rating`
/// lives in a PEER process. Consumed by [`remote::Stub`]: the composition root
/// (`cmd/match-svc`) passes `ratingrpc::remote_factories()` into `Stub::new`, so
/// `match`'s `require::<dyn MmrReader>(rating.mmr_reader)` resolves to the edge-backed
/// `Client` — the registry SWAP, with match's code unchanged. Wire-only (no `#[http]`),
/// so no route bindings: rating fronts no HTTP op.
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, caller| {
        mmr_reader_rpc::provide_remote(ctx.registry(), caller);
    })]
}

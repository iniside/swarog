//! `leaderboardrpc` — the leaderboard domain's GENERATED transport glue (port of Go's
//! `api/leaderboard/leaderboardrpc`): the edge-dependent half of the `#[rpc]` codegen,
//! expanded from `leaderboardapi`'s metadata-callback macro through
//! [`rpc_macro::generate_glue`]. The `leaderboard_rpc` module below contains the
//! `Client` (the split-topology edge client over an [`opsapi::Caller`]),
//! `register_server` (installs one edge adapter per method), and `provide_remote`.
//!
//! Rule 5: reached ONLY by the `leaderboard` module itself (a module importing its OWN
//! glue is sanctioned), `remote`, and `cmd/*` binaries — never by a domain consumer.

// The glue's method signatures re-resolve at THIS invocation site, so the api crate's
// types must be in scope exactly as in `leaderboardapi`'s lib.rs.
use leaderboardapi::*;
use opsapi::Error;

leaderboardapi::leaderboard_leaderboard_meta!(rpc_macro::generate_glue);

/// The leaderboard provider's client-registration closures for a process where the
/// front door lives elsewhere. Consumed by [`remote::Stub`]: the composition root
/// (`cmd/gateway-svc`) passes `leaderboardrpc::remote_factories()` into `Stub::new`.
///
/// Leaderboard is fronted-only — no peer `require`s a leaderboard capability — so this
/// contributes the `top_scores` `route_bindings()` ONLY (front-door routing; no
/// `LOCAL_SLOT`, the gateway dispatches `leaderboard.topScores` remotely to
/// leaderboard-svc's edge) and makes no capability provide.
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, _caller| {
        for rb in leaderboard_rpc::route_bindings() {
            ctx.contribute(opsapi::SLOT, rb.operation);
            ctx.contribute(opsapi::BINDING_SLOT, rb.binding);
        }
    })]
}

//! `matchapi` — the match module's PURE, transport-free capability contract (port of
//! Go's `api/match/matchapi`). It declares the single `Match` capability and applies
//! `#[rpc(prefix = "match")]` so the transport-FREE surface (per-method wire envelopes,
//! `METHOD_*` consts, `operations()`/`route_bindings()`) is GENERATED into the child
//! `match_rpc` module rather than hand-written. The edge-dependent glue (`Client`,
//! `register_server`, `provide_remote`) lives in the sibling `matchrpc` crate, which
//! expands this crate's metadata-callback macro (`match_match_meta!`) — so THIS crate
//! never depends on `edge`.
//!
//! `report` is a public (`auth = "none"`) op: no caller `Identity`, exactly as
//! `POST /match/report` had no bearer check in Go. Its public body keys stay the
//! Go-default capitalized `Winner`/`Loser` via the `body_names` override, so the
//! external contract is unchanged from the pre-migration handler.
//!
//! Domain CONSUMERS do not import this crate: match has no domain consumers. It is
//! reached only by the generated glue (`matchrpc`) and the `remote` stub — the
//! provider-owned contract surface, same precedent as each domain's `<module>events`.

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;

/// The match module's public capability: reporting a match result. It takes no caller
/// identity — the op is `auth = "none"`, a public write, exactly as `POST /match/report`
/// was before migration. The match service implements it (recording the match row +
/// emitting `match.finished` in one tx, after a synchronous MMR read); the gateway/edge
/// glue is generated from it.
#[rpc(prefix = "match")]
#[async_trait]
pub trait Match: Send + Sync {
    /// Records that `winner` beat `loser`. The public body keys are the Go-default
    /// capitalized `Winner`/`Loser` (a `body_names` override). 202 on success, no body.
    #[http(
        verb = "POST",
        path = "/match/report",
        auth = "none",
        success = 202,
        body_names(winner = "Winner", loser = "Loser")
    )]
    async fn report(&self, winner: String, loser: String) -> Result<(), Error>;
}

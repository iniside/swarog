//! `leaderboardapi` — the leaderboard module's PURE, transport-free capability contract
//! (port of Go's `api/leaderboard/leaderboardapi`). It declares the single `Leaderboard`
//! capability (a public read of the top scores) and applies `#[rpc(prefix =
//! "leaderboard")]` so the transport-FREE surface (wire envelopes, `METHOD_*` consts,
//! `operations()`/`route_bindings()`) is GENERATED into the child `leaderboard_rpc`
//! module. The edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives
//! in the sibling `leaderboardrpc` crate, which expands this crate's metadata-callback
//! macro (`leaderboard_leaderboard_meta!`) — so THIS crate never depends on `edge`.
//!
//! `top_scores` is `auth = "none"` (a public read, exactly as `GET /leaderboard` was
//! before migration): no caller `Identity`. Leaderboard has no domain consumers — this
//! crate is reached only by the generated glue and the `remote` stub.

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One player's standing. It lives here (not the impl crate) because it is the return
/// type of the `Leaderboard` capability, so the generated glue must be able to name it;
/// the `leaderboard` module uses it directly. The serde field names (`player`/`wins`)
/// are the public wire shape the pre-migration handler wrote, unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub player: String,
    pub wins: i64,
}

/// The leaderboard module's public capability: reading the top scores. It takes no
/// caller identity — the op is `auth = "none"`, a public read, exactly as
/// `GET /leaderboard` was before migration. The leaderboard service implements it; the
/// gateway/edge glue is generated from it.
#[rpc(prefix = "leaderboard")]
#[async_trait]
pub trait Leaderboard: Send + Sync {
    /// The top-ranked players (wins desc, player asc), capped at 100 — the same shape
    /// and limit as the pre-migration handler. 200.
    #[http(verb = "GET", path = "/leaderboard", auth = "none", success = 200)]
    async fn top_scores(&self) -> Result<Vec<Score>, Error>;
}

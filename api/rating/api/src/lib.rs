//! `ratingapi` — the rating module's PURE, transport-free capability contract. It
//! declares the single `MmrReader` capability and applies `#[rpc(prefix = "rating")]`
//! so the transport-FREE surface (wire envelope, `METHOD_MMR` const) is GENERATED into
//! the child `mmr_reader_rpc` module. The edge-dependent glue (`Client`,
//! `register_server`, `provide_remote`) lives in the sibling `ratingrpc` crate, which
//! expands this crate's metadata-callback macro (`rating_mmr_reader_meta!`) — so THIS
//! crate never depends on `edge`.
//!
//! `MmrReader::mmr` is WIRE-ONLY: no leading `Identity` (an internal peer call, not a
//! player-authenticated one) and no `#[http]` (not a gateway route) — the exact twin of
//! `charactersapi::Ownership`. The match module resolves it over the QUIC edge (the
//! `rating.mmr_reader` registry key) to read the players' MMR before recording a match.
//!
//! Domain CONSUMERS (only `match`) import this to name the trait for
//! `registry::require::<dyn MmrReader>`; they never import the `rating` impl crate.

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;

/// Reads a player's current matchmaking rating (MMR). Wire-only (no identity, no
/// `#[http]`): the sync capability `match` resolves over the edge to log/decide on the
/// players' standing before recording a result. `rating` starts every unseen player at
/// 1000 in its `rating.ratings` projection, so a genuine "unseen player" is the default
/// 1000, never an error —
/// an `Err` is a transport/infrastructure failure.
#[rpc(prefix = "rating")]
#[async_trait]
pub trait MmrReader: Send + Sync {
    #[retry_safe]
    async fn mmr(&self, player_id: String) -> Result<i64, Error>;
}

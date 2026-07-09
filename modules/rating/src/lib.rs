//! `rating` — an in-memory matchmaking-rating (MMR) service (port of Go's
//! `modules/rating`). It PROVIDES the wire-only `MmrReader` capability (a sync read of a
//! player's MMR, resolved by `match` over the registry / QUIC edge) and REACTS to
//! `match.finished` on the durable plane (+15 winner / -15 loser). `match` has zero
//! knowledge that `rating` exists — it publishes durably, rating subscribes.
//!
//! ## In-memory, by design (documented consequence)
//! Ratings live in a `RwLock<HashMap>` with a 1000 default, exactly as Go kept them.
//! Owning no schema means: **a rating-svc restart resets every MMR to 1000 while the
//! rest of the system keeps running.** Accepted for a for-fun backend. The durable
//! `on_tx` subscription still needs the messaging inbox (for the exactly-once claim), so
//! a process hosting rating carries a DB pool + the messaging module even though rating
//! writes no schema of its own — the inbox-dedup tx is used only to CLAIM the event; the
//! handler then mutates memory and ignores the handed connection.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use lifecycle::{Caps, Context, Module};
use opsapi::Error;
use ratingapi::MmrReader;
use registry::key;

/// A player's starting rating when never seen before.
const DEFAULT_MMR: i64 = 1000;
/// The delta applied to each player on a finished match (win / loss).
const WIN_DELTA: i64 = 15;
const LOSS_DELTA: i64 = 15;

/// The stable inbox-dedup subscriber name for rating's `match.finished` subscription.
const SUBSCRIBER: &str = "rating";

// ============================================================================
// Service — the in-memory MMR store. Backs the `MmrReader` capability (registry +
// edge face) and the `match.finished` reaction.
// ============================================================================

pub struct Service {
    mmr: RwLock<HashMap<String, i64>>,
}

impl Service {
    fn new() -> Service {
        Service {
            mmr: RwLock::new(HashMap::new()),
        }
    }

    /// The current rating of a player, or the 1000 default if never seen.
    fn get(&self, player_id: &str) -> i64 {
        self.mmr
            .read()
            .unwrap()
            .get(player_id)
            .copied()
            .unwrap_or(DEFAULT_MMR)
    }

    /// Applies a finished-match result: winner +15, loser -15 (from each player's
    /// current rating, defaulting to 1000). The single mutation path, invoked by the
    /// durable `match.finished` handler.
    fn apply_result(&self, winner: &str, loser: &str) {
        let w = self.get(winner) + WIN_DELTA;
        let l = self.get(loser) - LOSS_DELTA;
        let mut g = self.mmr.write().unwrap();
        g.insert(winner.to_string(), w);
        g.insert(loser.to_string(), l);
    }
}

#[async_trait]
impl MmrReader for Service {
    /// Reads a player's MMR. An unseen player is the 1000 default, never an error — an
    /// `Err` would be an infrastructure failure, and this reads only memory.
    async fn mmr(&self, player_id: String) -> Result<i64, Error> {
        Ok(self.get(&player_id))
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The rating module. Holds the constructed service (shared between `register`, the
/// `match.finished` reaction, and the edge face). Edge exposure is topology-blind:
/// `init` contributes the generated RPC face to `edge::EDGE_SLOT` unconditionally, and
/// `app::run` installs it iff this process serves an internal QUIC edge.
pub struct Rating {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Rating {
    fn default() -> Self {
        Rating::new()
    }
}

impl Rating {
    pub fn new() -> Rating {
        Rating {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("rating.register must run before init")
            .clone()
    }
}

#[async_trait]
impl Module for Rating {
    fn name(&self) -> &str {
        "rating"
    }

    /// No `MIGRATE`: rating persists nothing (in-memory by design). Only `REGISTER`
    /// (it Provides `MmrReader` in phase 1); `init` wires the subscription + edge face.
    fn caps(&self) -> Caps {
        Caps::REGISTER
    }

    /// Phase 1, BEFORE any `init`: builds the in-memory service and offers it under the
    /// canonical `rating.mmr_reader` key, so `match`'s `require::<dyn MmrReader>`
    /// resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = Arc::new(Service::new());
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("rating.register ran twice"))?;
        ctx.registry()
            .provide::<dyn MmrReader>(key("rating", "mmr_reader"), svc);
        Ok(())
    }

    /// Only wires up — no I/O (#8). Subscribes `match.finished` on the DURABLE plane
    /// (`on_tx`): the transport runs the handler inside a per-`(event_id,"rating")`
    /// inbox-dedup tx (exactly-once in BOTH topologies), and the handler mutates the
    /// in-memory ratings (ignoring the handed conn — rating writes no schema).
    /// Also contributes the generated `MmrReader` edge face so a peer's `match` resolves
    /// `rating.mmr` over QUIC when this process serves an internal edge.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        let reactor = svc.clone();
        ctx.bus().on_tx(
            &matchevents::FINISHED,
            SUBSCRIBER,
            move |_delivery, e: matchevents::Finished| {
                let reactor = reactor.clone();
                Box::pin(async move {
                    reactor.apply_result(&e.winner, &e.loser);
                    Ok(())
                })
            },
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer's `match` resolves `rating.mmr` over QUIC); in the monolith it is never
        // applied. Own glue (sanctioned): the generated register_server face lives in
        // `ratingrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                ratingrpc::mmr_reader_rpc::register_server(server, svc.clone());
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. No DB needed — the typed handler and the MMR math run entirely in memory.
// In-crate so they can drive the private `Service` directly.
// ============================================================================
#[cfg(test)]
mod tests;

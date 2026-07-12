//! Library half of `server`'s (the monolith's) composition root (Step 10): the real
//! module list, extracted so `tools/checkmodules` can build the SAME set the process
//! boots without hand-mirroring it.
//!
//! Two carve-outs, both required to keep the fortress/demos rules holding
//! textually AND transitively through `tools/checkmodules`:
//!   - **`demos/webui` is EXCLUDED here.** `main.rs` pushes it onto the returned
//!     `Vec` itself, after calling `modules()`. If this crate imported `webui`,
//!     `checkmodules → server (this lib)` would become a second consumer of a
//!     `demos/*` crate, breaking archcheck's "demos importable ONLY by cmd/server"
//!     rule (the rule can't tell "cmd/server's OWN main" from "a tool that merely
//!     depends on cmd/server's lib" — so the lib itself must never link `webui`).
//!   - **`player` (the QUIC player-edge socket handle) is accepted, not
//!     constructed.** `main.rs` owns the `Arc<Mutex<edge::PlayerServer>>` and
//!     decides whether to install it via `with_player_edge`; a checker passes
//!     `None` so it never touches a real QUIC listener. Gateway hosts no durable
//!     subscription either way, so the player-edge presence is invisible to
//!     `topiccheck`/`requirecheck`'s recorded event/require graph.

use std::sync::{Arc, Mutex};

use lifecycle::{Module, ProcessWiring};

/// The monolith hosts every module locally, so `wiring` (peer addresses, passthrough
/// origins) is unused — nothing here dials a peer. The parameter exists so this lib
/// shares the one `modules(&ProcessWiring, …)` shape the gateway-hosting `cmd/*` libs
/// use.
pub fn modules(
    _wiring: &ProcessWiring,
    player: Option<Arc<Mutex<edge::PlayerServer>>>,
) -> Vec<Box<dyn Module>> {
    let mut gw = gateway::Gateway::new();
    if let Some(p) = player {
        gw = gw.with_player_edge(p);
    }

    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(config::Config::new()),         // DB-backed config: schema "config", provides "config.reader"
        Box::new(characters::Characters::new()), // player characters; owns schema "characters"
        Box::new(inventory::Inventory::new()),   // owner-scoped inventories; depends on characters + config
        Box::new(accounts::Accounts::new()),     // player identity: sessions + dev/epic auth; owns schema "accounts"
        Box::new(admin::Admin::new()),           // GameOps portal at /admin; renders LOCAL contributions (all providers in-process)
        Box::new(audit::Audit::new()),           // append-only event ledger; owns schema "audit", records durable events in-process
        Box::new(scheduler::Scheduler::new()),   // data-driven durable event source; owns schema "scheduler", emits scheduler.fired
        Box::new(rating::Rating::new()),         // in-memory MMR; provides "rating.mmr_reader", reacts to match.finished (+15/-15)
        Box::new(match_module::MatchModule::new()), // records matches (schema "match"); reads rating sync, emits match.finished durably
        Box::new(leaderboard::LeaderboardModule::new()), // win tally; owns schema "leaderboard", reacts to match.finished, serves GET /leaderboard
        Box::new(apikeys::ApiKeys::new()),       // API-key policy store: schema "apikeys", provides "apikeys.keys" for the gateway's key check
        Box::new(gw),                            // HTTP + player QUIC front, auth-once (real accounts sessions)
    ]
}

#[cfg(test)]
mod tests;

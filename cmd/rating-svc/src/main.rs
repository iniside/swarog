//! `rating-svc` — the rating fortress process (Step 10). It hosts `rating` and stands
//! up one shared QUIC edge server (`EDGE_ADDR`, `:9007` in the run scripts); `rating`
//! contributes its `MmrReader` face to `edge::EDGE_SLOT` (topology-blind), and
//! `app::run` installs it so match-svc resolves `rating.mmr` over the mutually-
//! authenticated edge.
//!
//! rating owns NO schema (in-memory MMR, 1000 default), but its durable subscription
//! (`rating.match-finished.v1`) to `match.finished` needs the plane's pull worker and
//! checkpoint, so this process needs a DB pool and thus hosts the durable-events plane
//! (app-owned, DB ⇒ plane). The worker drains the shared log against this
//! subscription's cursor and runs rating's `on_tx` (+15/-15) per delivery; the effect
//! is in-memory (restart resets — accepted until the persistent-projection step), so
//! only redelivery, not the effect, is bounded by the checkpoint. It PRODUCES no
//! events; durable delivery needs NO per-process env.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server; `rating` contributes its `MmrReader` face during
    // `init`, `app::run` applies + `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // No gateway (no ops, no player front).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(rating::Rating::new()),
    ];

    // Serves rating.mmr on its own mTLS edge (EDGE_ADDR); no player front — rating is a
    // wire-only provider + a durable reactor, never fronted directly by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

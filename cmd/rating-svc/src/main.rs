//! `rating-svc` — the rating fortress process (Step 10). It hosts `rating` and stands
//! up one shared QUIC edge server (`EDGE_ADDR`, `:9007` in the run scripts); `rating`
//! contributes its `MmrReader` face to `edge::EDGE_SLOT` (topology-blind), and
//! `app::run` installs it so match-svc resolves `rating.mmr` over the mutually-
//! authenticated edge.
//!
//! rating owns NO schema (in-memory MMR, 1000 default), but its durable `on_tx("rating")`
//! subscription to `match.finished` needs the durable-events inbox, so this process needs
//! a DB pool and thus hosts the durable-events plane (app-owned, DB ⇒ plane). Its
//! `POST /events` inbound sink (mounted by the plane on the HTTP server) is how
//! match-svc's relay delivers `match.finished` here; rating's `on_tx` applies +15/-15 on
//! the handed inbox-dedup tx (exactly-once) then mutates memory. It PRODUCES no events,
//! so it names no `EVENTS_SUBSCRIBERS` (pure sink) — EVENTS_ORIGIN is still set distinct
//! per process by the run scripts.

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

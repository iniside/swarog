//! `leaderboard-svc` — the leaderboard fortress process (Step 10). It hosts leaderboard
//! and stands up one shared QUIC edge server; `leaderboard` contributes its
//! `leaderboard.topScores` face to `edge::EDGE_SLOT` (topology-blind), and `app::run`
//! installs it so gateway-svc can dispatch `GET /leaderboard` Remote to this process.
//! leaderboard OWNS a schema, so this process needs a DB pool and thus hosts the
//! durable-events plane (app-owned, DB ⇒ plane): its pull worker drains
//! `leaderboard.match-finished.v1` from the shared log, and leaderboard's durable
//! `on_tx` upserts wins+1 on the handed delivery tx, atomically with the cursor.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so leaderboard needs no accounts stub for a bearer
//! verifier. It serves `leaderboard.topScores` ONLY over the internal mTLS edge; HTTP here
//! is just the infra surface (`/healthz`, `/readyz`, `/metrics`), no typed ops.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server; `leaderboard` contributes its `topScores` face during
    // `init`, `app::run` applies + `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods = leaderboard_svc::modules(&ProcessWiring::new());

    // No player front: leaderboard-svc is fronted by gateway-svc, never directly by
    // players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

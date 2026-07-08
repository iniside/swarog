//! `leaderboard-svc` — the leaderboard fortress process (Step 10). It hosts leaderboard +
//! messaging and stands up one shared QUIC edge server; `leaderboard` contributes its
//! `leaderboard.topScores` face to `edge::EDGE_SLOT` (topology-blind), and `app::run`
//! installs it so gateway-svc can dispatch `GET /leaderboard` Remote to this process.
//! leaderboard OWNS a schema + an inbox, so this process needs a DB pool + the messaging
//! durable plane: its `POST /events` inbound sink receives `match.finished` from
//! match-svc's relay, and leaderboard's durable `on_tx` upserts wins+1 on the handed
//! inbox-dedup tx.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so leaderboard needs no accounts stub for a bearer
//! verifier. It serves `leaderboard.topScores` ONLY over the internal mTLS edge; HTTP here
//! is just the infra surface (`/healthz`, `/readyz`, `/metrics`, `/events`), no typed ops.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server; `leaderboard` contributes its `topScores` face during
    // `init`, `app::run` applies + `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — the relay/inbound halt before
    // leaderboard tears down.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(leaderboard::LeaderboardModule::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // No player front: leaderboard-svc is fronted by gateway-svc, never directly by
    // players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

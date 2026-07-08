//! `leaderboard-svc` — the leaderboard fortress process (Step 10). It hosts gateway +
//! leaderboard + messaging and stands up one shared QUIC edge server; `leaderboard`
//! contributes its `leaderboard.topScores` face to `edge::EDGE_SLOT` (topology-blind),
//! and `app::run` installs it so gateway-svc can dispatch `GET /leaderboard` Remote to
//! this process. leaderboard OWNS a schema + an inbox, so this process needs a DB pool +
//! the messaging durable plane: its `POST /events` inbound sink receives `match.finished`
//! from match-svc's relay, and leaderboard's durable `on_tx` upserts wins+1 on the handed
//! inbox-dedup tx. Mirrors `characters-svc`'s shape exactly (gateway + module +
//! messaging + accounts stub, edge server, no player front).

use std::sync::{Arc, Mutex};

use lifecycle::Module;

/// Reads `env_key`, falling back to `default` when unset or blank — a NUMERIC
/// `host:port` (Rust's `SocketAddr` needs a literal IP, unlike Go's dialer).
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server; `leaderboard` contributes its `topScores` face during
    // `init`, `app::run` applies + `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — the relay/inbound halt before
    // leaderboard tears down.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(gateway::Gateway::new()),
        Box::new(leaderboard::LeaderboardModule::new()),
        Box::new(messaging::Messaging::new()),
        // Real session verification: the accounts stub fills the gateway's verifier.
        Box::new(remote::Stub::new(
            "accounts",
            &env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
    ];

    // No player front: leaderboard-svc is fronted by gateway-svc, never directly by
    // players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

//! `match-svc` — the match fortress process (Step 10). It hosts match + messaging and
//! stands up one shared QUIC edge server; `match` contributes its `match.report` face to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it so gateway-svc can
//! dispatch `POST /match/report` Remote to this process. match fills
//! its `rating` dependency with a `remote::Stub`: the stub `provide`s the edge-backed
//! `MmrReader` client under the SAME registry key the local `rating` module would, so
//! `match`'s `require::<dyn MmrReader>` resolves REMOTELY over mTLS to rating-svc — the
//! registry SWAP, with match's code unchanged. The match outbox relay runs HERE (it
//! drains match's own `match.finished` rows) and POSTs them to rating-svc / leaderboard-
//! svc / audit-svc via EVENTS_SUBSCRIBERS. Mirrors `characters-svc`'s shape exactly.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so match needs no accounts stub for a bearer verifier. It
//! serves `match.report` ONLY over the internal mTLS edge; HTTP here is just the infra
//! surface (`/healthz`, `/readyz`, `/metrics`, `/events`), no typed ops.

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

    // One shared QUIC edge server for the whole process; `match` contributes its
    // `match.report` face during `init`, `app::run` applies + `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — the relay halts delivery before match
    // tears down. The `rating` stub's `register` provides the remote MmrReader before
    // match's `init` requires it (two-phase Build).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(match_module::MatchModule::new()),
        Box::new(messaging::Messaging::new()),
        // `rating` lives in rating-svc: this stub swaps in the edge-backed MmrReader so
        // match's sync pre-emit read dials rating-svc over mTLS (lazy dial).
        Box::new(remote::Stub::new(
            "rating",
            &env_addr("RATING_EDGE_ADDR", "127.0.0.1:9007"),
            ratingrpc::remote_factories(),
        )),
    ];

    // No player front: match-svc serves peers over the internal mTLS edge, not players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

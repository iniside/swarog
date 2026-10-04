//! `party-svc` — the party fortress process. It hosts party and fills its `accounts`
//! dependency (`accountsapi::Directory`) with a `remote::Stub`, so party's
//! `require::<dyn Directory>` resolves REMOTELY with party's code unchanged, and it holds a
//! `remote::PushSender` forwarding `ctx.push()` to the front that owns the player's
//! WebSocket (`GATEWAY_EDGE_ADDR`).
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc and the monolith. It serves `party.*` ONLY over the internal mTLS edge;
//! HTTP here is just the infra surface (`/healthz`, `/readyz`, `/metrics`).

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

/// Reads `env_key`, falling back to `default` when unset or blank — a NUMERIC
/// `host:port` (Rust's `SocketAddr` needs a literal IP).
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let wiring = ProcessWiring::new()
        .with_peer("accounts", env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"))
        .with_peer("gateway", env_addr("GATEWAY_EDGE_ADDR", "127.0.0.1:9013"));
    let mods = party_svc::modules(&wiring);

    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

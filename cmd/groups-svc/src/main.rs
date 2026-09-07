//! `groups-svc` — the groups fortress process. It hosts groups and fills its
//! `accounts` dependency (`accountsapi::Directory`) with a `remote::Stub`: it `provide`s
//! an edge-backed client under the SAME registry key the local impl would, so groups'
//! `require::<dyn Directory>` resolves REMOTELY — the registry SWAP, with groups' code
//! unchanged.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so it needs no accounts stub for a bearer verifier — it
//! consumes `accounts.directory` purely as a sync capability. It serves `groups.*`
//! ONLY over the internal mTLS edge; gateway-svc dispatches Remote to it. HTTP here is
//! just the infra surface (`/healthz`, `/readyz`, `/metrics`), no typed ops. Its durable
//! event append rides the shared log this process's own DB pool writes into,
//! atomically with the domain row.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

/// Reads `env_key`, falling back to `default` when unset or blank — a NUMERIC
/// `host:port` (Rust's `SocketAddr` needs a literal IP). The run scripts set the peer
/// edge addresses.
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `groups` contributes its
    // `groups.*` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let wiring = ProcessWiring::new()
        .with_peer("accounts", env_addr("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"));
    let mods = groups_svc::modules(&wiring);

    // Serves groups ops on its own edge (`EDGE_ADDR`) so gateway-svc can dispatch
    // `groups.*` Remote to it. No player front here either way — this process is
    // fronted by gateway-svc, never directly by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

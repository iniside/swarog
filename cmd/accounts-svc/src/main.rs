//! `accounts-svc` — the accounts fortress process (Step 6). It hosts accounts +
//! messaging and stands up one shared QUIC edge server (`EDGE_ADDR`, `:9003` in the run
//! scripts); `accounts` contributes its Sessions + Auth faces to `edge::EDGE_SLOT`
//! (topology-blind), and `app::run` installs them on this server so every front process's
//! gateway verifies bearer tokens (`accounts.verifySession`) and fronts the auth ops over
//! the mutually-authenticated edge.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith. The typed auth ops (`/accounts/register|login|me`) are
//! fronted by gateway-svc, which dispatches them Remote to this process's edge. What this
//! process DOES serve on its own HTTP port are the Epic web-OAuth browser routes
//! (`POST /accounts/epic/start`, `GET /accounts/epic/callback`) — the accounts module
//! mounts those via `ctx.mount`, independent of any gateway — plus the infra surface
//! (`/healthz`, `/readyz`, `/metrics`, `/events`). MESSAGING_ORIGIN must be distinct per
//! process (never the `"monolith"` default); `player.registered` rides this outbox.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `accounts` contributes its
    // Sessions/Auth faces to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — delivery halts before accounts
    // tears down.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(accounts::Accounts::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // Serves accounts.* on its own mTLS edge (EDGE_ADDR); no player front — accounts
    // is fronted by front processes' gateways (HTTP/QUIC), plus its own Epic OAuth routes.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

//! `accounts-svc` — the accounts fortress process (Step 6). It hosts gateway +
//! accounts + messaging and stands up one shared QUIC edge server (`EDGE_ADDR`,
//! `:9003` in the run scripts); `accounts` contributes its Sessions + Auth faces to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs them on this server
//! so every peer process's gateway verifies bearer tokens (`accounts.verifySession`)
//! and fronts the auth ops over the mutually-authenticated edge.
//!
//! The gateway here resolves the LOCAL `accounts.sessions` capability (the accounts
//! module provides it in phase-1 register), so this process fronts
//! `/accounts/register|login|login/epic|me` on its own HTTP port with REAL session
//! auth. MESSAGING_ORIGIN must be distinct per process (never the `"monolith"`
//! default); `player.registered` rides this process's outbox.

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
        Box::new(gateway::Gateway::new()),
        Box::new(accounts::Accounts::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // Serves accounts.* on its own mTLS edge (EDGE_ADDR); no player front — accounts
    // is fronted by peers' gateways (HTTP/QUIC), and by its own HTTP port.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

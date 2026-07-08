//! `inventory-svc` — process B of the split (port of Go's `cmd/inventory-svc`). It
//! hosts gateway + config + inventory + messaging and fills inventory's `characters`
//! dependency with a `remote::Stub`: the stub `provide`s an edge-backed
//! `characters.ownership` client (dialing A), so inventory's
//! `require::<dyn Ownership>` resolves REMOTELY — the registry SWAP, with inventory's
//! code unchanged. It reaches the `charactersapi` contract + `charactersrpc` glue
//! only transitively (via the stub / inventory), and NOT the characters IMPL. B now
//! ALSO stands up its OWN shared QUIC edge server (`EDGE_ADDR`, default `:9001`);
//! `inventory` contributes its `inventory.*` face to `edge::EDGE_SLOT`
//! (topology-blind) and `app::run` installs it here, so B SERVES `inventory.*` ops
//! for a peer — gateway-svc hosts no providers, so it resolves `inventory.*` as
//! Remote and dials this edge (Step 6 of the QUIC player-front plan). B still dials
//! OUT to A over its own edge for ownership checks; it is client of A and server for
//! the front door at the same time.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

/// The peer QUIC edge address a `characters` stub dials: `CHARACTERS_EDGE_ADDR`, else
/// the shared default. A NUMERIC `host:port` (Rust's `SocketAddr` needs a literal IP,
/// unlike Go's dialer) — the run scripts set `127.0.0.1:9000`.
fn characters_edge_addr() -> String {
    std::env::var("CHARACTERS_EDGE_ADDR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1:9000".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `inventory` contributes its
    // `inventory.*` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it — mirrors
    // `characters-svc`'s pattern exactly.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging then the stub last. The stub's phase-1 `register` provides
    // characters.ownership BEFORE inventory's `init` requires it (guaranteed by the
    // two-phase Build regardless of list order); messaging's `register` installs the
    // durable transport before inventory's `on_tx` subscriptions.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(gateway::Gateway::new()),
        Box::new(config::Config::new()),
        Box::new(inventory::Inventory::new()),
        Box::new(messaging::Messaging::new()),
        Box::new(remote::Stub::new("characters", &characters_edge_addr())),
    ];

    // B now SERVES inventory ops on its own edge (`EDGE_ADDR`, default `:9001` in the
    // run scripts) so gateway-svc can dispatch `inventory.*` Remote to it. No player
    // front here either way — B is fronted by gateway-svc, never directly by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

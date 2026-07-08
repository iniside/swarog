//! `inventory-svc` — process B of the split (port of Go's `cmd/inventory-svc`). It
//! hosts gateway + config + inventory + messaging and fills inventory's `characters`
//! dependency with a `remote::Stub`: the stub `provide`s an edge-backed
//! `characters.ownership` client (dialing A), so inventory's
//! `require::<dyn Ownership>` resolves REMOTELY — the registry SWAP, with inventory's
//! code unchanged. It imports the `charactersapi` CONTRACT (transitively, via the
//! stub) but NOT the characters IMPL. No edge server: B dials OUT to A; nothing calls
//! B over the edge in the 2-process proof.

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

    // No edge server: B is a pure edge CLIENT of A in the 2-process proof.
    app::run(app::Config::from_env(), mods, None).await
}

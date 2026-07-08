//! `server` — the MONOLITH entrypoint (port of Go's `cmd/server`). It hosts EVERY
//! module in ONE process, with no edge server: every cross-module dependency resolves
//! locally through the registry (inventory's `require::<dyn Ownership>` takes the
//! in-process branch), so nothing crosses a QUIC boundary. The split entrypoints
//! (`characters-svc`, `inventory-svc`) each import only their own modules; this binary
//! is the opposite end — the full set.

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // All modules, hosted locally. Order note: messaging LAST — its phase-1 `register`
    // installs the durable transport before any consumer's `init` (guaranteed anyway
    // by the two-phase Build), and registration order governs Stop (reverse), so
    // last-registered stops FIRST — delivery halts before any producer/consumer tears
    // down.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(config::Config::new()),         // DB-backed config: schema "config", provides "config.reader"
        Box::new(characters::Characters::new()), // player characters; owns schema "characters"
        Box::new(inventory::Inventory::new()),   // owner-scoped inventories; depends on characters + config
        Box::new(gateway::Gateway::new()),       // HTTP front door: routes contributed player ops, auth-once
        Box::new(messaging::Messaging::new()),   // the durable async plane (transport + relay + inbox)
    ];

    // No edge server: every provider is in-process in the monolith.
    app::run(app::Config::from_env(), mods, None).await
}

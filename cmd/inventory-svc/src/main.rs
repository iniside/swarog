//! `inventory-svc` — process B of the split (port of Go's `cmd/inventory-svc`). It
//! hosts inventory and fills inventory's `characters` AND `config`
//! dependencies with `remote::Stub`s: each stub `provide`s an edge-backed client under
//! the SAME registry key the local impl would, so inventory's
//! `require::<dyn Ownership>` / `require::<dyn Config>` resolve REMOTELY — the registry
//! SWAP, with inventory's code unchanged.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so B needs no accounts stub for a bearer verifier. B
//! serves `inventory.*` ONLY over the internal mTLS edge; gateway-svc dispatches Remote
//! to it. HTTP here is just the infra surface (`/healthz`, `/readyz`, `/metrics`,
//! `/events`), no typed ops.
//!
//! Since Step 5 config is its OWN fortress process (config-svc): the `config` stub's
//! `configrpc` factory swaps in a snapshot-backed `CachedConfig` (boot-filled by one
//! `snapshot()` over the edge, refreshed on the durable `config.changed`). B reaches
//! the `charactersapi`/`configapi` contracts + the glue crates only via the stubs, NOT
//! the provider IMPL crates. B ALSO stands up its OWN shared QUIC edge server
//! (`EDGE_ADDR`, default `:9001`) so gateway-svc can dispatch `inventory.*` Remote to
//! it; it is client of A + config-svc and server for the front door at the same time.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

/// Reads `env_key`, falling back to `default` when unset or blank — a NUMERIC
/// `host:port` (Rust's `SocketAddr` needs a literal IP, unlike Go's dialer). The run
/// scripts set the peer edge addresses.
fn env_addr(env_key: &str, default: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `inventory` contributes its
    // `inventory.*` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it — mirrors
    // `characters-svc`'s pattern exactly.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // The durable-events plane (app-owned, DB ⇒ plane) is constructed and its transport
    // injected into the bus at Context construction, before any module's `init` — so the
    // `config` stub's factory (its `on_tx("config-cache")` subscription runs inside the
    // stub's `register`) always finds it. Each stub's `register` provides its capability
    // before inventory's `init` requires it (two-phase Build). The `config` stub also
    // runs a boot-fill snapshot in `start` — config-svc must already be up (the run
    // scripts start it first).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(inventory::Inventory::new()),
        // `remote` is generic (Steps 4–5): this composition root injects each provider's
        // swap closures explicitly, so `remote` never names `characters`/`config`.
        Box::new(remote::Stub::new(
            "characters",
            &env_addr("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
            charactersrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "config",
            &env_addr("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
            configrpc::remote_factories(),
        )),
    ];

    // B now SERVES inventory ops on its own edge (`EDGE_ADDR`, default `:9001` in the
    // run scripts) so gateway-svc can dispatch `inventory.*` Remote to it. No player
    // front here either way — B is fronted by gateway-svc, never directly by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

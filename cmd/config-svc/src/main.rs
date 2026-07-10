//! `config-svc` — the config fortress process (Step 5). It hosts config and stands up
//! one shared QUIC edge server; `config` contributes its wire-only `ConfigSnapshot` face
//! to `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it on this server so a
//! peer's `CachedConfig` (inventory-svc) resolves `config.snapshot` over the
//! mutually-authenticated edge. config's LISTEN/NOTIFY listener publishes
//! `config.changed` on the DURABLE plane (app-owned, DB ⇒ plane): one append onto the
//! shared XID-ordered log; consumers (inventory-svc, audit-svc) pull it with their own
//! workers against their own checkpoints.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so config needs no accounts stub for a bearer verifier.
//! config serves `config.snapshot` ONLY over the internal mTLS edge; HTTP here is just
//! the infra surface (`/healthz`, `/readyz`, `/metrics`), no typed ops. Durable
//! delivery needs NO per-process env (no origins, no subscriber routing). Ports/addrs
//! (PORT, EDGE_ADDR) are set by the run scripts.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `config` contributes its
    // `config.snapshot` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods = config_svc::modules(&ProcessWiring::new());

    // Serves config.snapshot on its own mTLS edge (EDGE_ADDR); no player front — config
    // is infrastructure, fronted by peers over the internal edge, never by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

//! `storage-svc` — the storage fortress process. It hosts storage and stands up one
//! shared QUIC edge server; `storage` contributes its `storage.*` face to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it here so gateway-svc can
//! dispatch the player ops Remote over the mutually-authenticated edge.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so it needs no accounts stub for a bearer verifier. HTTP
//! here is just the infra surface (`/healthz`, `/readyz`, `/metrics`), no typed ops.
//! Ports/addrs (PORT, EDGE_ADDR) are set by the run scripts.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `storage` contributes its
    // `storage.*` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods = storage_svc::modules(&ProcessWiring::new());

    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

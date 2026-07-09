//! `apikeys-svc` — the apikeys fortress process (Step 3). It hosts apikeys and stands
//! up one shared QUIC edge server; `apikeys` contributes its wire-only `Keys` face to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it on this server so a
//! peer's key verifier (gateway-svc, admin-svc) resolves `apikeys.keys` over the
//! mutually-authenticated edge.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so apikeys needs no accounts stub for a bearer verifier.
//! apikeys serves `apikeys.keys` ONLY over the internal mTLS edge; HTTP here is just
//! the infra surface (`/healthz`, `/readyz`, `/metrics`, `/events`), no typed ops.
//!
//! EVENTS_ORIGIN MUST be distinct per process (never the `"monolith"` default): the
//! relay drains ONLY its own origin's outbox rows, and the plane's start-time
//! origin-collision guard rejects a default origin alongside remote sinks. Ports/addrs
//! (PORT, EDGE_ADDR, EVENTS_SUBSCRIBERS, EVENTS_ORIGIN) are set by the run scripts.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `apikeys` contributes its
    // `apikeys.keys` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(apikeys::ApiKeys::new()),
    ];

    // Serves apikeys.keys on its own mTLS edge (EDGE_ADDR); no player front — apikeys
    // is infrastructure, fronted by peers over the internal edge, never by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

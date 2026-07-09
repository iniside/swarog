//! `config-svc` — the config fortress process (Step 5). It hosts config and stands up
//! one shared QUIC edge server; `config` contributes its wire-only `ConfigSnapshot` face
//! to `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it on this server so a
//! peer's `CachedConfig` (inventory-svc) resolves `config.snapshot` over the
//! mutually-authenticated edge. config's LISTEN/NOTIFY listener publishes
//! `config.changed` on the DURABLE plane; this process's durable-events relay (app-owned,
//! DB ⇒ plane) drains its own outbox rows and POSTs them to the peers named in
//! `EVENTS_SUBSCRIBERS` (the run scripts point `config.changed` at inventory-svc's
//! `/events`).
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc + the monolith, so config needs no accounts stub for a bearer verifier.
//! config serves `config.snapshot` ONLY over the internal mTLS edge; HTTP here is just
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

    // One shared QUIC edge server for this process. `config` contributes its
    // `config.snapshot` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(config::Config::new()),
    ];

    // Serves config.snapshot on its own mTLS edge (EDGE_ADDR); no player front — config
    // is infrastructure, fronted by peers over the internal edge, never by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

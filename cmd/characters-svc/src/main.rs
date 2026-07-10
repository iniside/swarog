//! `characters-svc` — process A of the split (port of Go's `cmd/characters-svc`). It
//! hosts ONLY characters and stands up one shared QUIC edge server; the durable-events
//! plane is app-owned (DB ⇒ plane), not a listed module;
//! `characters` contributes its `characters.ownerOf` + player-op faces to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs them on this server so a
//! peer's inventory can resolve ownership over the mutually-authenticated edge.
//! `character.created`/`.deleted` are appended onto the shared durable log in the
//! domain tx; consumers (inventory-svc, audit-svc) pull them with their own workers.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc and the monolith (`cmd/server`). A serves its ops ONLY over the internal
//! mTLS edge — gateway-svc dispatches `characters.*` Remote to it. HTTP here is just the
//! infra surface (`/healthz`, `/readyz`, `/metrics`), no typed ops.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for the whole process. Modules contribute their
    // RPC faces to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it (a single UDP
    // port serves every edge method). Standing this up is the composition root's
    // legitimate topology knowledge — the modules never see it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // No accounts stub: without a gateway there is no bearer verifier to feed, so this
    // process never dials accounts-svc.
    let mods = characters_svc::modules(&ProcessWiring::new());

    // No player front: A serves peers over the internal mutual-TLS edge, not players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

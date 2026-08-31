//! `mail-svc` — the mail fortress process. It hosts ONLY mail and stands up one shared
//! QUIC edge server; the durable-events plane is app-owned (DB => plane), not a listed
//! module. Its ingress is the durable `mail.send-requested.v1` subscription, which pulls
//! from the shared XID-ordered log and writes each outbox row on the handed delivery
//! transaction, atomically with the checkpoint advance.
//!
//! It dials NO peer — `requires()` is empty, so this process needs no `remote::Stub`
//! beyond what `metrics` brings.
//!
//! It hosts NO gateway (FrontDoor): the single public front door lives only in gateway-svc
//! and the monolith (`cmd/server`). HTTP here is just the infra surface (`/healthz`,
//! `/readyz`, `/metrics`), no typed ops.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for the whole process. Modules contribute their RPC
    // faces to `edge::EDGE_SLOT` during `init`; `app::run` applies the contributions onto
    // this server after Build, then `listen`s it. Standing this up is the composition
    // root's legitimate topology knowledge — the modules never see it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    let mods = mail_svc::modules(&ProcessWiring::new());

    // No player front: mail has no player-facing surface at all.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

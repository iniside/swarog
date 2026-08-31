//! `notifications-svc` — the notifications fortress process. It hosts ONLY
//! notifications and stands up one shared QUIC edge server; the durable-events plane
//! is app-owned (DB ⇒ plane), not a listed module. `notifications` contributes its
//! player-op faces plus `admin.adminData`/`admin.adminSubmit` to `edge::EDGE_SLOT`
//! (topology-blind), and `app::run` installs them on this server, so gateway-svc can
//! dispatch the player ops Remote and admin-svc can fetch/submit its admin page over
//! the mTLS edge. Its three durable subscriptions (`notifications.wallet-changed.v1`,
//! `notifications.player-promoted.v1`, `notifications.prune-on-scheduler.v1`) pull
//! from the shared XID-ordered log; each handler inserts on the handed delivery tx,
//! atomically with the checkpoint advance.
//!
//! It dials NO peer — `requires()` is empty, so this process needs no `remote::Stub`
//! beyond what `metrics` brings.
//!
//! It hosts NO gateway (FrontDoor) module: the single public front door lives only in
//! gateway-svc and the monolith (`cmd/server`). It serves its ops ONLY over the
//! internal mTLS edge; HTTP here is just the infra surface (`/healthz`, `/readyz`,
//! `/metrics`), no typed ops.

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

    let mods = notifications_svc::modules(&ProcessWiring::new());

    // No player front: notifications-svc is fronted by gateway-svc, never directly
    // by players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

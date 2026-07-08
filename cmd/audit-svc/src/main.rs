//! `audit-svc` — the audit fortress process (Step 8). It hosts `audit` + `messaging`
//! and stands up one shared QUIC edge server (`EDGE_ADDR`, `:9004` in the run scripts);
//! `audit` contributes its `admin.adminData` face to `edge::EDGE_SLOT` (topology-blind),
//! and `app::run` installs it on this server so admin-svc pulls audit's page over the
//! mutually-authenticated edge.
//!
//! Unlike the pure aggregators (gateway-svc/admin-svc) audit OWNS a schema (`audit`) and
//! an inbox, so this process needs a DB pool and the messaging durable plane. Its
//! `POST /events` inbound sink (mounted by messaging on the HTTP server) is how every
//! producer peer's relay delivers `character.created` / `character.deleted` /
//! `player.registered` / `config.changed` here; audit's `on_tx_raw` subscriptions
//! record each on the handed inbox-dedup tx. It PRODUCES no events, so it names no
//! `EVENTS_SUBSCRIBERS` (pure sink) — but MESSAGING_ORIGIN is still set distinct per
//! process by the run scripts, for its own outbox identity.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `audit` contributes its
    // `admin.adminData` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — delivery halts before audit tears
    // down. No gateway (no ops, no player front).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(audit::Audit::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // Serves audit.adminData on its own mTLS edge (EDGE_ADDR); no player front — audit
    // is a pure sink, fronted only by a remote admin over the internal edge.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

//! `audit-svc` — the audit fortress process (Step 8). It hosts `audit` and stands up
//! one shared QUIC edge server (`EDGE_ADDR`, `:9004` in the run scripts); `audit`
//! contributes its `admin.adminData` face to `edge::EDGE_SLOT` (topology-blind), and
//! `app::run` installs it on this server so admin-svc pulls audit's page over the
//! mutually-authenticated edge.
//!
//! Unlike the pure aggregators (gateway-svc/admin-svc) audit OWNS a schema (`audit`),
//! so this process needs a DB pool and thus hosts the durable-events plane (app-owned,
//! DB ⇒ plane). Its pull workers drain audit's six consumer-owned subscriptions
//! (`audit.<topic-kebab>.v1`) from the shared XID-ordered log; audit's `on_tx_raw`
//! handlers record each event on the handed delivery tx, atomically with the cursor
//! advance (exactly-once). It PRODUCES no events. Durable delivery needs NO
//! per-process env — no origins, no subscriber routing, no `POST /events` sink.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `audit` contributes its
    // `admin.adminData` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // No gateway (no ops, no player front).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(audit::Audit::new()),
    ];

    // Serves audit.adminData on its own mTLS edge (EDGE_ADDR); no player front — audit
    // is a pure sink, fronted only by a remote admin over the internal edge.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

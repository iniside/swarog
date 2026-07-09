//! `scheduler-svc` — the scheduler fortress process (Step 9). It hosts `scheduler` and
//! stands up one shared QUIC edge server (`EDGE_ADDR`, `:9005` in the run scripts);
//! `scheduler` contributes its read-only `admin.adminData` "Schedules"
//! face to `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it on this server
//! so admin-svc pulls the page over the mutually-authenticated edge.
//!
//! Like audit-svc it OWNS a schema (`scheduler`) and rides the durable-events plane
//! (app-owned, DB ⇒ plane) — but it is a PRODUCER: its 1s emission loop `emit_tx`s
//! `scheduler.fired` for every due schedule, one append onto the shared XID-ordered
//! log in the SAME tx as the `last_fired` bump; audit-svc's prune subscription pulls
//! it with its own worker and checkpoint. It serves NO player front and fronts no
//! typed ops, so no gateway module.
//!
//! Durable delivery needs NO per-process env (no origins, no subscriber routing).
//! `SCHEDULER_ENABLED` (default true) gates the emission loop. Ports/addrs are set by
//! the run scripts.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `scheduler` contributes its
    // `admin.adminData` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // No gateway (no ops, no player front).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(scheduler::Scheduler::new()),
    ];

    // Serves scheduler.adminData on its own mTLS edge (EDGE_ADDR); no player front —
    // scheduler is a pure durable producer, fronted only by a remote admin over the edge.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

//! `scheduler-svc` — the scheduler fortress process (Step 9). It hosts `scheduler` +
//! `messaging` and stands up one shared QUIC edge server (`EDGE_ADDR`, `:9005` in the
//! run scripts); `scheduler` contributes its read-only `admin.adminData` "Schedules"
//! face to `edge::EDGE_SLOT` (topology-blind), and `app::run` installs it on this server
//! so admin-svc pulls the page over the mutually-authenticated edge.
//!
//! Like audit-svc it OWNS a schema (`scheduler`) and rides the messaging durable plane —
//! but it is a PRODUCER: its 1s emission loop `emit_tx`s `scheduler.fired` for every due
//! schedule, and this process's relay drains its own outbox rows and POSTs them to the
//! peers named in `EVENTS_SUBSCRIBERS` (the run scripts point `scheduler.fired` at
//! audit-svc's `/events`, where audit's prune reacts). It serves NO player front and
//! fronts no typed ops, so no gateway module.
//!
//! MESSAGING_ORIGIN MUST be distinct per process (never the `"monolith"` default): the
//! relay drains ONLY its own origin's outbox rows, and messaging's origin-collision
//! guard rejects a default origin alongside remote sinks. `SCHEDULER_ENABLED` (default
//! true) gates the emission loop. Ports/addrs are set by the run scripts.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for this process. `scheduler` contributes its
    // `admin.adminData` face to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — the relay halts delivery before the
    // scheduler's emission loop tears down. No gateway (no ops, no player front).
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(scheduler::Scheduler::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // Serves scheduler.adminData on its own mTLS edge (EDGE_ADDR); no player front —
    // scheduler is a pure durable producer, fronted only by a remote admin over the edge.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

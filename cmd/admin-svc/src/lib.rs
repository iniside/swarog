//! Library half of `admin-svc`'s composition root: the real module list, extracted so
//! `tools/checkmodules` can build the SAME set the process boots without hand-mirroring
//! it. `main.rs` resolves each peer edge address from env into a [`ProcessWiring`]; the
//! checker harness passes an empty one and gets the same defaults back from
//! [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy address is safe.

use lifecycle::{Module, ProcessWiring};

/// One stub per peer carrying only the control-plane factory, so admin-svc gains the
/// peer's `cp.*` face as a `controlplane::Source` without becoming front-capable for the
/// peer's typed ops.
fn peer_face(peer: &'static str, wiring: &ProcessWiring, default: &str) -> Box<dyn Module> {
    Box::new(remote::Stub::new(
        peer,
        wiring.peer_or(peer, default),
        vec![controlplanerpc::remote_factory(peer)],
    ))
}

/// Composition root: the core admin portal plus metrics; no domain module; DB-backed.
/// Each peer's edge is dialled by the portal's manifest refresh.
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(admin::Admin::new()),
        peer_face("characters", wiring, "127.0.0.1:9000"),
        peer_face("inventory", wiring, "127.0.0.1:9001"),
        peer_face("config", wiring, "127.0.0.1:9002"),
        peer_face("accounts", wiring, "127.0.0.1:9003"),
        peer_face("audit", wiring, "127.0.0.1:9004"),
        peer_face("scheduler", wiring, "127.0.0.1:9005"),
        peer_face("apikeys", wiring, "127.0.0.1:9009"),
        peer_face("wallet", wiring, "127.0.0.1:9010"),
        peer_face("notifications", wiring, "127.0.0.1:9011"),
        peer_face("mail", wiring, "127.0.0.1:9012"),
        peer_face("friends", wiring, "127.0.0.1:9014"),
        peer_face("groups", wiring, "127.0.0.1:9015"),
        peer_face("storage", wiring, "127.0.0.1:9016"),
    ]
}

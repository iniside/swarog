//! Library half of `notifications-svc`'s composition root: the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` calls `modules(&wiring)` and separately owns
//! the runtime edge server — this crate never touches I/O.

use lifecycle::{Module, ProcessWiring};

/// notifications-svc `require`s no synchronous capability (retention is a config env
/// var), so the only address it takes from `wiring` is the push front's: the
/// `remote::PushSender` below is what makes `ctx.push()` in this process reach the
/// WebSocket connections gateway-svc owns, over `push.deliver` on its internal edge.
///
/// The sender is NOT a `remote::Stub`: it contributes no `opsapi::PEER_SLOT` address (the
/// front provides no `#[http]` op to route to) and no readiness check — push is
/// best-effort, so a front that is down or absent must never hold this process unready or
/// keep it from booting. That is also why the fleet declares no dependency on gateway-svc
/// here: gateway-svc already depends on notifications-svc, and the reverse edge would be a
/// cycle for a channel that tolerates the peer being gone.
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(notifications::NotificationsModule::new()),
        Box::new(remote::PushSender::new(
            wiring.peer_or("gateway", "127.0.0.1:9013"),
        )),
    ]
}

//! Library half of `party-svc`'s composition root: the real module list, extracted so
//! `tools/checkmodules` can build the SAME set the process boots without hand-mirroring
//! it. `main.rs` resolves the peer edge addresses from env into a [`ProcessWiring`] and
//! calls `modules(&wiring)`; the checker harness builds an empty `ProcessWiring` and gets
//! the same defaults back from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O,
//! so a dummy peer address is safe.

use lifecycle::{Module, ProcessWiring};

/// The `remote::PushSender` is what makes `ctx.push()` in this process reach the WebSocket
/// connections gateway-svc owns, over `push.deliver` on its internal edge. It is not a
/// `remote::Stub`: it contributes no peer address and no readiness check, so an absent
/// front never holds this process unready, and the fleet declares no gateway-svc
/// dependency (gateway-svc already depends on party-svc).
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(party::Party::new()),
        // `remote` is generic: this composition root injects the accounts provider's
        // swap closures explicitly, so `remote` never names `accounts`.
        Box::new(remote::Stub::new(
            "accounts",
            wiring.peer_or("accounts", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
        Box::new(remote::PushSender::new(
            wiring.peer_or("gateway", "127.0.0.1:9013"),
        )),
    ]
}

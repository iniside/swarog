//! Library half of `admin-svc`'s composition root (Step 10): the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves each peer edge address from env
//! (unchanged defaults) into a [`ProcessWiring`] and calls `modules(&wiring)`; the
//! checker harness builds an empty `ProcessWiring` and gets the same defaults back
//! from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy peer
//! address is safe.

use lifecycle::{Module, ProcessWiring};

/// One admin-only stub for `provider`: it applies JUST the admin fan-out factory (not
/// the provider's full `remote_factories()`), so admin-svc contributes the provider's
/// REMOTE admin item WITHOUT also becoming front-capable for its typed ops.
fn admin_stub(provider: &str, wiring: &ProcessWiring, default: &str) -> Box<dyn Module> {
    Box::new(remote::Stub::new(
        provider,
        &wiring.peer_or(provider, default),
        vec![adminrpc::admin_remote_factory(provider)],
    ))
}

/// The admin portal + one admin-only stub per provider. Each stub dials its peer's
/// edge lazily on the first /admin request that fetches its item.
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(admin::Admin::new()),
        admin_stub("characters", wiring, "127.0.0.1:9000"),
        admin_stub("inventory", wiring, "127.0.0.1:9001"),
        admin_stub("config", wiring, "127.0.0.1:9002"),
        admin_stub("accounts", wiring, "127.0.0.1:9003"),
        admin_stub("audit", wiring, "127.0.0.1:9004"),
        admin_stub("scheduler", wiring, "127.0.0.1:9005"),
        admin_stub("apikeys", wiring, "127.0.0.1:9009"),
    ]
}

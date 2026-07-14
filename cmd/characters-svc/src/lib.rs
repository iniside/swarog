//! Library half of `characters-svc`'s composition root (Step 10): the real module
//! list, extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves each peer edge address from env into
//! a [`ProcessWiring`] and calls `modules(&wiring)`; the checker harness builds an
//! empty `ProcessWiring` and gets the same defaults back from [`ProcessWiring::peer_or`]
//! — `register`/`init` do no I/O, so a dummy peer address is safe. This crate never
//! touches I/O; `main.rs` separately owns the runtime edge server.

use lifecycle::{Module, ProcessWiring};

/// characters-svc hosts characters and fills its `config` dependency with a
/// `remote::Stub` that dials config-svc over the QUIC edge: the stub `provide`s an
/// edge-backed `CachedConfig` under the SAME registry key the local impl would, so
/// characters' `require::<dyn Config>` resolves REMOTELY — the registry swap, with
/// characters' code unchanged. The `config` peer address comes from `wiring` (env in
/// `main.rs`, never in this lib). It hosts NO gateway (FrontDoor) — the single public
/// front door lives only in gateway-svc + the monolith — so no accounts stub is needed.
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(characters::Characters::new()),
        // `remote` is generic: this composition root injects config's swap closures
        // explicitly (via `configrpc::remote_factories()`), so `remote` never names
        // `config` and this crate never imports the config IMPL crate.
        Box::new(remote::Stub::new(
            "config",
            &wiring.peer_or("config", "127.0.0.1:9002"),
            configrpc::remote_factories(),
        )),
    ]
}

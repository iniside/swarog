//! Library half of `wallet-svc`'s composition root: the real module list, extracted so
//! `tools/checkmodules` can build the SAME set the process boots without hand-mirroring
//! it. `main.rs` resolves each peer edge address from env into a [`ProcessWiring`] and
//! calls `modules(&wiring)`; the checker harness builds an empty `ProcessWiring` and gets
//! the same defaults back from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O,
//! so a dummy peer address is safe. This crate never touches I/O; `main.rs` separately
//! owns the runtime edge server.

use lifecycle::{Module, ProcessWiring};

/// wallet-svc hosts wallet and fills its `config` dependency with a `remote::Stub` that
/// dials config-svc over the QUIC edge. The stub is NOT optional: `WalletModule::requires`
/// declares `config` and its `init` resolves `require::<dyn Config>(key("config","reader"))`
/// (the starter grant reads `wallet/starter_currency` + `wallet/starter_amount`), so a
/// wallet-svc without it would fail `app::validate_requires` at startup and, past that,
/// die in `init` on the missing capability. The stub `provide`s an edge-backed
/// `CachedConfig` under the SAME registry key the local impl would, so wallet's
/// `require::<dyn Config>` resolves REMOTELY — the registry swap, with wallet's code
/// unchanged. The `config` peer address comes from `wiring` (env in `main.rs`, never in
/// this lib). It hosts NO gateway (FrontDoor) — the single public front door lives only
/// in gateway-svc + the monolith — so no accounts stub is needed.
pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(wallet::WalletModule::new()),
        // `remote` is generic: this composition root injects config's swap closures
        // explicitly (via `configrpc::remote_factories()`), so `remote` never names
        // `config` and this crate never imports the config IMPL crate.
        Box::new(remote::Stub::new(
            "config",
            wiring.peer_or("config", "127.0.0.1:9002"),
            configrpc::remote_factories(),
        )),
    ]
}

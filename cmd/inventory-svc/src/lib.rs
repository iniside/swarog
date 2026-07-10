//! Library half of `inventory-svc`'s composition root (Step 10): the real module
//! list, extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves each peer edge address from env
//! (unchanged defaults) into a [`ProcessWiring`] and calls `modules(&wiring)`;
//! the checker harness builds an empty `ProcessWiring` and gets the same defaults
//! back from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy
//! peer address is safe.

use lifecycle::{Module, ProcessWiring};

pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(inventory::Inventory::new()),
        // `remote` is generic (Steps 4–5): this composition root injects each provider's
        // swap closures explicitly, so `remote` never names `characters`/`config`.
        Box::new(remote::Stub::new(
            "characters",
            &wiring.peer_or("characters", "127.0.0.1:9000"),
            charactersrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "config",
            &wiring.peer_or("config", "127.0.0.1:9002"),
            configrpc::remote_factories(),
        )),
    ]
}

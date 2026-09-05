//! Library half of `friends-svc`'s composition root: the real module list, extracted so
//! `tools/checkmodules` can build the SAME set the process boots without hand-mirroring
//! it. `main.rs` resolves the `accounts` peer edge address from env (unchanged default)
//! into a [`ProcessWiring`] and calls `modules(&wiring)`; the checker harness builds an
//! empty `ProcessWiring` and gets the same default back from
//! [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy peer address is
//! safe.

use lifecycle::{Module, ProcessWiring};

pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(friends::Friends::new()),
        // `remote` is generic: this composition root injects the accounts provider's
        // swap closures explicitly, so `remote` never names `accounts`.
        Box::new(remote::Stub::new(
            "accounts",
            wiring.peer_or("accounts", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
    ]
}

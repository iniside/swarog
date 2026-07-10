//! Library half of `match-svc`'s composition root (Step 10): the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves the `rating` peer edge address from
//! env (unchanged default) into a [`ProcessWiring`] and calls `modules(&wiring)`;
//! the checker harness builds an empty `ProcessWiring` and gets the same default back
//! from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy peer
//! address is safe.

use lifecycle::{Module, ProcessWiring};

pub fn modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(match_module::MatchModule::new()),
        // `rating` lives in rating-svc: this stub swaps in the edge-backed MmrReader so
        // match's sync pre-emit read dials rating-svc over mTLS (lazy dial).
        Box::new(remote::Stub::new(
            "rating",
            &wiring.peer_or("rating", "127.0.0.1:9007"),
            ratingrpc::remote_factories(),
        )),
    ]
}

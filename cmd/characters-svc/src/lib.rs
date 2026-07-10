//! Library half of `characters-svc`'s composition root (Step 10): the real module
//! list, extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` calls `modules(&wiring)` and separately owns
//! the runtime edge server — this crate never touches I/O.

use lifecycle::{Module, ProcessWiring};

/// characters-svc dials no peer (no accounts stub without a gateway to feed), so the
/// accepted `wiring` is unused today — the parameter exists so every `cmd/*-svc` lib
/// shares the one `modules(&ProcessWiring)` signature `tools/checkmodules` depends on.
pub fn modules(_wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(characters::Characters::new()),
    ]
}

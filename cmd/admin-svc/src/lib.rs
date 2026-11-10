//! Library half of `admin-svc`'s composition root (Step 10): the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it.

use lifecycle::{Module, ProcessWiring};

/// The admin module plus core metrics. The wiring carries no peers.
pub fn modules(_wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(admin::Admin::new()),
    ]
}

//! Library half of `leaderboard-svc`'s composition root (Step 10): the real module
//! list, extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` calls `modules(&wiring)` and separately owns
//! the runtime edge server — this crate never touches I/O.

use lifecycle::{Module, ProcessWiring};

/// leaderboard-svc dials no peer (it only SERVES `leaderboard.topScores` over its own
/// edge and pulls durable events), so the accepted `wiring` is unused today — the
/// parameter exists so every `cmd/*-svc` lib shares the one `modules(&ProcessWiring)`
/// signature `tools/checkmodules` depends on.
pub fn modules(_wiring: &ProcessWiring) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(leaderboard::LeaderboardModule::new()),
    ]
}

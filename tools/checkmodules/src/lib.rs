//! `checkmodules` — the single source for the per-process module sets both
//! `topiccheck` and `requirecheck` build to observe real subscribe/require call
//! sites (Step 3, abstraction-leak closures; per-process libs, Step 10). Before
//! Step 10 this crate hand-mirrored `cmd/server`'s module vec; now it imports the
//! `cmd/*` composition-root LIBS directly (each exports
//! `modules(&ProcessWiring, …)`, the exact list its `main.rs` boots), so a module
//! added to any `cmd/*` lib can't silently drop out of either harness's coverage.
//!
//! Every process' `modules()` is built with an EMPTY [`ProcessWiring`] (a dummy —
//! `register`/`init` do no I/O, so a missing peer address just falls back to that
//! lib's own default; the same trick `topiccheck`'s lazy DB pool already relies on)
//! and, for the two Gateway-hosting processes, `player: None` (no real QUIC socket
//! is ever allocated here).

use lifecycle::{Module, ProcessWiring};

/// A dummy wiring for the checker harnesses: empty, so every `peer_or` call falls
/// back to that lib's own default address. Safe because `register`/`init` never
/// dial it — only `start` (never run by these harnesses) would.
fn checker_wiring() -> ProcessWiring {
    ProcessWiring::new()
}

/// The monolith's module set — `cmd/server`'s lib, minus `demos/webui` (excluded by
/// the lib itself; `cmd/server`'s own `main.rs` is the only place that adds it back)
/// and with no player-edge socket allocated.
pub fn monolith_modules() -> Vec<Box<dyn Module>> {
    server::modules(&checker_wiring(), None)
}

/// Every split process paired with its module set, in split-proof port order.
/// `gateway-svc` hosts no durable subscription, so its module set is included for
/// completeness (a future validation may still want to see it) even though today's
/// topic/require checks find nothing interesting there.
fn split_process_modules() -> Vec<(&'static str, Vec<Box<dyn Module>>)> {
    let w = checker_wiring();
    vec![
        ("characters-svc", characters_svc::modules(&w)),
        ("inventory-svc", inventory_svc::modules(&w)),
        ("gateway-svc", gateway_svc::modules(&w, None)),
        ("config-svc", config_svc::modules(&w)),
        ("apikeys-svc", apikeys_svc::modules(&w)),
        ("accounts-svc", accounts_svc::modules(&w)),
        ("admin-svc", admin_svc::modules(&w)),
        ("audit-svc", audit_svc::modules(&w)),
        ("scheduler-svc", scheduler_svc::modules(&w)),
        ("match-svc", match_svc::modules(&w)),
        ("rating-svc", rating_svc::modules(&w)),
        ("leaderboard-svc", leaderboard_svc::modules(&w)),
    ]
}

/// The two deployment topologies both checker harnesses may want to validate
/// against: `Monolith` carries `cmd/server`'s single module list; `Split` carries
/// every `cmd/*-svc` process paired with its own list. Step 11 adds the
/// per-subscription-per-profile validation that actually iterates `Split`; Step 10
/// only wires the source data.
pub enum DeploymentProfile {
    Monolith,
    Split,
}

impl DeploymentProfile {
    /// Every process this profile boots, each paired with its process id (the
    /// monolith's is `"server"`).
    pub fn processes(&self) -> Vec<(&'static str, Vec<Box<dyn Module>>)> {
        match self {
            DeploymentProfile::Monolith => vec![("server", monolith_modules())],
            DeploymentProfile::Split => split_process_modules(),
        }
    }
}

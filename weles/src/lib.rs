//! Weles is a standalone fleet-orchestrator CLI (M0): it boots, health-checks,
//! and tears down a game-backend fleet (monolith or split topology) as an
//! independent top-level crate — zero-sharing by design, no dependency on any
//! workspace crate (core/*, api/*, modules/*, tools/*).
//! weles never builds; it executes artifacts staged in `<root>/deploy` via
//! `weles deploy`. It shares exactly one convention with `devctl`: the
//! `run/rollout.lock` protocol that keeps at most one rollout-bearing command
//! running against the shared local Postgres at a time.
//!
//! The library target exists so the crate's integration tests (`tests/`) can
//! drive internals such as [`platform`] directly while spawning the real
//! `weles` binary via `CARGO_BIN_EXE_weles`.
//!
//! **The supervisor is synchronous; tokio is confined to [`agentapi`].** M0
//! decided "no tokio" (design finding #13) on an M0-scoped premise: there was
//! nothing to serve. M1's agent must serve `resolve` over HTTP, which outgrows
//! that premise — so the runtime enters as a strictly bounded ISLAND on its own
//! thread, and everything the supervisor's correctness rests on
//! ([`platform`]'s `try_wait`/`spawn`, [`lock`], [`state`], [`prep`], the pure
//! decision functions, the signal handler, `Reporter`) stays sync on the
//! supervisor thread. See [`agentapi`]'s module doc for the full hard line.

pub mod agentapi;
pub mod cli;
pub mod control;
pub mod health;
pub mod lock;
pub mod platform;
pub mod prep;
pub mod supervisor;

// The PLATFORM-FREE master role lives in the internal `weles-master` sub-crate
// (an A2 boundary-drawing refactor): fleet composition ([`manifest`]), the
// strict `fleet.toml` loader/validator ([`fleet_toml`]), and the persisted
// supervisor-state schema ([`state`]). The dependency points agent -> master
// ONLY, so master code physically cannot name `platform`/`supervisor`/`lock`
// (the compiler rejects the `use` — see `weles-master`'s compile_fail proof).
//
// Re-exported at these SAME paths so `weles::manifest`/`fleet_toml`/`state`
// (and every `crate::…` path inside this crate) keep resolving unchanged — the
// role boundary moved, the public surface did not. This is NOT a zero-sharing
// violation: a weles-INTERNAL sub-crate is the sanctioned way to model the
// future master/agent process split without splitting the process yet.
pub use weles_master::{fleet_toml, manifest, state};

/// Committed-fleet fixtures for THIS crate's tests. The `weles-master` crate has
/// its own `#[cfg(test)]` copies (its manifest dir differs), and a `#[cfg(test)]`
/// item is never reachable across a crate boundary — so the agent-side tests
/// (`prep`, `agentapi`) load the same committed TOMLs through the public
/// [`fleet_toml::load`], resolved from THIS crate's manifest dir where the
/// fixtures live.
#[cfg(test)]
pub(crate) mod test_fixtures {
    use crate::fleet_toml::{self, Fleet};

    /// Loads `weles/fleet.split.toml` — the committed 12-process split fixture.
    pub(crate) fn load_split_fixture() -> Fleet {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fleet.split.toml");
        fleet_toml::load(&path).expect("weles/fleet.split.toml must load")
    }

    /// Loads `weles/fleet.monolith.toml` — the committed monolith fixture.
    pub(crate) fn load_monolith_fixture() -> Fleet {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fleet.monolith.toml");
        fleet_toml::load(&path).expect("weles/fleet.monolith.toml must load")
    }
}

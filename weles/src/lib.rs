//! Weles is a standalone fleet-supervisor CLI (M0): it builds, boots,
//! health-checks, and tears down a game-backend fleet (monolith or split
//! topology) as an independent top-level crate — zero-sharing by design, no
//! dependency on any workspace crate (core/*, api/*, modules/*, tools/*),
//! std-only (no tokio). It shares exactly one convention with `devctl`: the
//! `run/rollout.lock` protocol that keeps at most one rollout-bearing command
//! running against the shared local Postgres at a time.
//!
//! The library target exists so the crate's integration tests (`tests/`) can
//! drive internals such as [`platform`] directly while spawning the real
//! `weles` binary via `CARGO_BIN_EXE_weles`.

pub mod cli;
pub mod control;
pub mod health;
pub mod lock;
pub mod manifest;
pub mod platform;
pub mod prep;
pub mod state;
pub mod supervisor;

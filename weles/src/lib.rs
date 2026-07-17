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
pub mod manifest;
pub mod platform;
pub mod prep;
pub mod state;
pub mod supervisor;

//! Weles is a standalone fleet-supervisor CLI (M0): it builds, boots, health-checks,
//! and tears down a game-backend fleet (monolith or split topology) as an
//! independent top-level crate — zero-sharing by design, no dependency on any
//! workspace crate (core/*, api/*, modules/*, tools/*), std-only (no tokio). It
//! shares exactly one convention with `devctl`: the `run/rollout.lock` protocol
//! that keeps at most one rollout-bearing command running against the shared local
//! Postgres at a time.

use std::process::ExitCode;

mod cli;
mod control;
mod health;
mod lock;
mod manifest;
mod platform;
mod prep;
mod state;
mod supervisor;

use anyhow::{bail, Result};
use cli::Command;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("weles: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("weles: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Up { .. } => up(),
        Command::Status => status(),
        Command::Down => down(),
        Command::TestChild { spawn_grandchild } => test_child(spawn_grandchild),
    }
}

fn up() -> Result<()> {
    bail!("not implemented yet (Step N)")
}

fn status() -> Result<()> {
    bail!("not implemented yet (Step N)")
}

fn down() -> Result<()> {
    bail!("not implemented yet (Step N)")
}

fn test_child(_spawn_grandchild: bool) -> Result<()> {
    bail!("not implemented yet (Step N)")
}

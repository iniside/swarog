//! Binary entrypoint for `weles` — see the crate docs in `lib.rs`. Includes
//! the hidden `__test-child` fixture used by the platform containment tests.

use std::process::ExitCode;

mod fixture;

use anyhow::{bail, Result};
use weles::cli::{self, Command, Topology};
use weles::supervisor;

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
        Command::Up {
            topology,
            skip_build,
        } => up(topology, skip_build),
        Command::Status => status(),
        Command::Down => down(),
        Command::TestChild {
            spawn_grandchild,
            ignore_graceful,
            stubborn_grandchild,
        } => fixture::run(spawn_grandchild, ignore_graceful, stubborn_grandchild),
    }
}

fn up(topology: Topology, skip_build: bool) -> Result<()> {
    supervisor::run_up(topology, skip_build)
}

fn status() -> Result<()> {
    bail!("not implemented yet (M0 Step 6)")
}

fn down() -> Result<()> {
    bail!("not implemented yet (M0 Step 6)")
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

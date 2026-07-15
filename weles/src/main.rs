//! Binary entrypoint for `weles` — see the crate docs in `lib.rs`. Includes
//! the hidden `__test-child` fixture used by the platform containment tests.

use std::process::ExitCode;

mod fixture;

use anyhow::{bail, Context, Result};
use weles::cli::{self, Command, Topology};
use weles::{manifest, prep};

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
    // weles's own Cargo.toml sits directly at the repo root (unlike the
    // tools/* crates), so the workspace root is exactly one parent up from
    // the compile-time manifest dir.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("weles crate has no parent directory")?
        .to_path_buf();

    manifest::validate_disk(&root.join("cmd")).context("validate fleet manifest against cmd/*-svc on disk")?;
    manifest::validate_pg_budget().context("validate fleet Postgres session budget")?;

    let layout = prep::Layout::discover(root)?;

    if !skip_build {
        let mut packages: Vec<&str> = match topology {
            Topology::Split => manifest::split_fleet().iter().map(|svc| svc.pkg).collect(),
            Topology::Monolith => vec![manifest::monolith().pkg],
        };
        packages.extend(["adminctl", "edgeca"]);
        packages.sort_unstable();
        packages.dedup();
        prep::build(&layout, &packages)?;
    }

    prep::mint_ca(&layout)?;
    prep::seed_admin(&layout, &prep::database_url())?;

    bail!("supervisor loop lands in M0 Step 5")
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

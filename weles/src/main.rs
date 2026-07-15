//! Binary entrypoint for `weles` — see the crate docs in `lib.rs`. Includes
//! the hidden `__test-child` fixture used by the platform containment tests.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod fixture;

use anyhow::{bail, Context, Result};
use weles::cli::{self, Command, Topology};
use weles::state::{self, FleetState};
use weles::{control, supervisor};

/// Matches `tools/devctl/src/supervisor.rs::DOWN_TIMEOUT`: how long `weles down`
/// polls for the fleet to reach a terminal state before giving up.
const DOWN_TIMEOUT: Duration = Duration::from_secs(130);

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

/// `weles status`: reports the recorded fleet, connecting to a live supervisor
/// for a fresh per-service table and exiting 0.
fn status() -> Result<()> {
    let (state, endpoint) = match connect_target()? {
        Target::Connect { state, endpoint } => (state, endpoint),
        Target::Report(result) => return result,
    };
    let message = control::request(Path::new(&endpoint), "status", &state.supervisor)?;
    println!("{message}");
    Ok(())
}

/// `weles down`: asks a live supervisor to stop, then polls the state file
/// until the fleet reaches a terminal status (or the shutdown deadline).
fn down() -> Result<()> {
    let (state, endpoint) = match connect_target()? {
        Target::Connect { state, endpoint } => (state, endpoint),
        Target::Report(result) => return result,
    };
    let message = control::request(Path::new(&endpoint), "down", &state.supervisor)?;
    println!("{message}");
    control::wait_for_terminal(&state_path()?, &state.supervisor, DOWN_TIMEOUT)
}

/// A resolved control target: either connect to a live supervisor, or a
/// terminal message/error the caller should just return.
enum Target {
    Connect { state: FleetState, endpoint: String },
    Report(Result<()>),
}

/// Loads the state file and classifies it: connectable (live, non-terminal) or
/// a message to print / error to raise (inactive / stale / no state / booting).
fn connect_target() -> Result<Target> {
    let path = state_path()?;
    let Some(state) = state::load(&path)? else {
        bail!(
            "no weles fleet recorded (no {}) — run `weles up` first",
            path.display()
        );
    };
    let alive = control::supervisor_alive(&state.supervisor);
    match control::classify(&state, control::now_unix(), alive) {
        control::Disposition::Inactive(message) => {
            println!("{message}");
            Ok(Target::Report(Ok(())))
        }
        control::Disposition::Stale(message) => Ok(Target::Report(Err(anyhow::anyhow!(message)))),
        control::Disposition::Connect => match state.control_endpoint.clone() {
            Some(endpoint) => Ok(Target::Connect { state, endpoint }),
            // Live and non-terminal but no endpoint published yet: the fleet is
            // still booting (the endpoint is bound only once healthy).
            None => Ok(Target::Report(Err(anyhow::anyhow!(
                "weles: the {} fleet is still starting (no control endpoint yet) — retry shortly",
                state.topology
            )))),
        },
    }
}

/// `run/weles/state.json` under the repo root — discovered exactly like `up`
/// (this crate's `Cargo.toml` sits at the repo root, one parent up).
fn state_path() -> Result<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("weles crate has no parent directory")?
        .to_path_buf();
    Ok(root.join("run").join("weles").join("state.json"))
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

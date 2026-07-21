//! Binary entrypoint for `weles` — see the crate docs in `lib.rs`. Includes
//! the hidden `__test-child` fixture used by the platform containment tests.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod fixture;

use anyhow::{bail, Result};
use weles::cli::{self, Command};
use weles::state::{self, FleetState};
use weles::{control, prep, supervisor};

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
        Command::Up { dry_run, root } => up(dry_run, root),
        Command::Deploy { src_dir, fleet, root } => deploy(&src_dir, &fleet, root),
        Command::Status { root } => status(root),
        Command::Down { root } => down(root),
        Command::TestChild {
            spawn_grandchild,
            ignore_graceful,
            stubborn_grandchild,
        } => fixture::run(spawn_grandchild, ignore_graceful, stubborn_grandchild),
    }
}

fn up(dry_run: bool, root: Option<PathBuf>) -> Result<()> {
    if dry_run {
        return dry_run_fleet(root);
    }
    supervisor::run_up(root)
}

/// `weles up --dry-run`: load + validate the DEPLOYED `fleet.toml` and print a
/// summary, WITHOUT acquiring the rollout lock, running any `[[prepare]]` hook,
/// or spawning a service. `discover_layout` pins the deployed generation and
/// parses+validates its `fleet.toml` (the same load+validate `up` performs),
/// so this reuses the one deployed-fleet authority rather than re-locating the
/// file. Side-effect-free beyond ensuring `run/weles` exists.
fn dry_run_fleet(root: Option<PathBuf>) -> Result<()> {
    let layout = supervisor::discover_layout(root)?;
    let fleet = layout
        .fleet()
        .expect("discover_layout pins a validated fleet");
    println!(
        "weles: deployed fleet is valid — {} service(s), {} prepare hook(s), {} passthrough key(s)",
        fleet.services.len(),
        fleet.prepare.len(),
        fleet.passthrough.len()
    );
    for svc in &fleet.services {
        println!("  service {} (pkg {}, http :{})", svc.name, svc.pkg, svc.http_port);
    }
    for hook in &fleet.prepare {
        println!("  prepare {} (run {})", hook.name, hook.run);
    }
    Ok(())
}

/// `weles deploy <src-dir> --fleet <fleet.toml>`: stage the fleet binaries and
/// stamp the chosen `fleet.toml` into `<root>/deploy`.
fn deploy(src_dir: &str, fleet: &str, root: Option<PathBuf>) -> Result<()> {
    let layout = supervisor::discover_layout_for_deploy(root)?;
    prep::deploy(&layout, Path::new(src_dir), Path::new(fleet))
}

/// `weles status`: reports the recorded fleet, connecting to a live supervisor
/// for a fresh per-service table and exiting 0.
fn status(root: Option<PathBuf>) -> Result<()> {
    let (state, endpoint) = match connect_target(root)? {
        Target::Connect { state, endpoint } => (state, endpoint),
        Target::Report(result) => return result,
    };
    let message = control::request(Path::new(&endpoint), "status", &state.supervisor)?;
    println!("{message}");
    Ok(())
}

/// `weles down`: asks a live supervisor to stop, then polls the state file
/// until the fleet reaches a terminal status (or the shutdown deadline).
fn down(root: Option<PathBuf>) -> Result<()> {
    let (state, endpoint) = match connect_target(root.clone())? {
        Target::Connect { state, endpoint } => (state, endpoint),
        Target::Report(result) => return result,
    };
    let message = control::request(Path::new(&endpoint), "down", &state.supervisor)?;
    println!("{message}");
    control::wait_for_terminal(&state_path(root)?, &state.supervisor, DOWN_TIMEOUT)
}

/// A resolved control target: either connect to a live supervisor, or a
/// terminal message/error the caller should just return.
enum Target {
    Connect { state: FleetState, endpoint: String },
    Report(Result<()>),
}

/// How many times [`connect_target`] re-loads the state file waiting for a live
/// supervisor to publish its (pre-boot) control endpoint, and the gap between
/// tries. The endpoint is bound BEFORE boot, so a missing endpoint beside a live
/// non-terminal supervisor is only the sub-second prep window between the first
/// `Starting` checkpoint and the post-bind checkpoint — a brief retry closes it.
const ENDPOINT_RETRIES: u32 = 3;
const ENDPOINT_RETRY_GAP: Duration = Duration::from_millis(100);

/// Loads the state file and classifies it: connectable (live, non-terminal) or
/// a message to print / error to raise (inactive / stale / no state / pre-bind).
fn connect_target(root: Option<PathBuf>) -> Result<Target> {
    let path = state_path(root)?;
    for attempt in 0..=ENDPOINT_RETRIES {
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
                return Ok(Target::Report(Ok(())));
            }
            control::Disposition::Stale(message) => {
                return Ok(Target::Report(Err(anyhow::anyhow!(message))));
            }
            control::Disposition::Connect => match state.control_endpoint.clone() {
                Some(endpoint) => return Ok(Target::Connect { state, endpoint }),
                // Live and non-terminal but no endpoint yet: the fleet is in the
                // narrow pre-bind prep window (the endpoint is published BEFORE
                // boot, so this closes in well under a second). Re-load and retry
                // a few times before treating it as real.
                None => {
                    if attempt < ENDPOINT_RETRIES {
                        std::thread::sleep(ENDPOINT_RETRY_GAP);
                        continue;
                    }
                    return Ok(Target::Report(Err(anyhow::anyhow!(
                        "weles: the {} fleet is in very early startup; the control endpoint \
                         is not bound yet — try `weles status`/`down` again in a moment",
                        state.topology
                    ))));
                }
            },
        }
    }
    unreachable!("connect_target loop returns on every attempt")
}

/// `run/weles/state.json` under the runtime-resolved fleet root — the SAME
/// [`prep::resolve_root`] authority `up`/`deploy` use, so `status`/`down` and the
/// supervisor agree on the root (and therefore on the `rollout.lock` path)
/// without a second compile-time derivation.
fn state_path(root: Option<PathBuf>) -> Result<PathBuf> {
    let root = prep::resolve_root(root)?;
    Ok(root.join("run").join("weles").join("state.json"))
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

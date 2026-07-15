//! Hand-rolled CLI parsing for `weles` (house style — no clap; see
//! `tools/verifyctl/src/cli.rs` for the pattern this file copies).

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Topology {
    Split,
    Monolith,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Up { topology: Topology, skip_build: bool },
    Status,
    Down,
    /// Hidden test fixture for the platform containment tests — not listed
    /// in USAGE.
    TestChild {
        spawn_grandchild: bool,
        ignore_graceful: bool,
        /// The grandchild (implies spawning one) ignores the graceful signal.
        stubborn_grandchild: bool,
    },
}

pub const USAGE: &str = "\
weles - standalone fleet-supervisor CLI

USAGE:
  weles up [split|monolith] [--skip-build]
  weles status
  weles down

up defaults to the split topology.";

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command> {
    let mut args = args.into_iter();
    let Some(verb) = args.next() else {
        bail!("missing command\n\n{USAGE}");
    };
    match verb.as_str() {
        "up" => {
            let mut topology = Topology::Split;
            let mut topology_seen = false;
            let mut skip_build = false;
            for arg in args {
                match arg.as_str() {
                    "split" | "monolith" => {
                        if topology_seen {
                            bail!("topology given more than once\n\n{USAGE}");
                        }
                        topology_seen = true;
                        topology = if arg == "split" {
                            Topology::Split
                        } else {
                            Topology::Monolith
                        };
                    }
                    // Policy: repeating a boolean flag is idempotent and
                    // accepted (only conflicting values — two topologies —
                    // are rejected). Pinned by cli_tests.
                    "--skip-build" => skip_build = true,
                    other => bail!("unknown argument {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Command::Up {
                topology,
                skip_build,
            })
        }
        "status" => {
            expect_no_more_args(args)?;
            Ok(Command::Status)
        }
        "down" => {
            expect_no_more_args(args)?;
            Ok(Command::Down)
        }
        "__test-child" => {
            let mut spawn_grandchild = false;
            let mut ignore_graceful = false;
            let mut stubborn_grandchild = false;
            for arg in args {
                match arg.as_str() {
                    "--spawn-grandchild" => spawn_grandchild = true,
                    "--ignore-graceful" => ignore_graceful = true,
                    "--stubborn-grandchild" => stubborn_grandchild = true,
                    other => bail!("unknown argument {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Command::TestChild {
                spawn_grandchild,
                ignore_graceful,
                stubborn_grandchild,
            })
        }
        other => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

fn expect_no_more_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    if let Some(other) = args.into_iter().next() {
        bail!("unknown argument {other:?}\n\n{USAGE}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;

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
    Up { topology: Topology },
    /// Stage the fleet binaries from `src_dir` into `<root>/deploy`.
    Deploy { src_dir: String },
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
  weles deploy <src-dir>
  weles up [split|monolith]
  weles status
  weles down

up defaults to the split topology. weles never builds — it executes only the
binaries staged into <root>/deploy by `weles deploy` (<src-dir> resolves
relative to the current directory).";

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command> {
    let mut args = args.into_iter();
    let Some(verb) = args.next() else {
        bail!("missing command\n\n{USAGE}");
    };
    match verb.as_str() {
        "up" => {
            let mut topology = Topology::Split;
            let mut topology_seen = false;
            for arg in args {
                match arg.as_str() {
                    // The rollout-lease borrow marker, APPENDED to this argv by
                    // the parent that lent us its lease
                    // (`processctl::OwnedLease::spawn_borrower`). It is
                    // `crate::lock`'s to read (`borrow_inherited_if_present`
                    // scans `args_os` itself), not this parser's to interpret —
                    // but it lands here first, and rejecting it would make the
                    // whole borrow path unreachable from `weles up`: the very
                    // command that takes a lease. Accepted silently and ONLY on
                    // `up`, the only verb that is rollout-bearing; on `status`/
                    // `down`/`deploy` it stays an unknown argument, because
                    // there it means the caller is confused about what it
                    // spawned.
                    _ if arg == crate::lock::BORROWED_LEASE_ARG => {}
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
                    other => bail!("unknown argument {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Command::Up { topology })
        }
        "deploy" => {
            let Some(src_dir) = args.next() else {
                bail!("deploy requires a source directory\n\n{USAGE}");
            };
            expect_no_more_args(args)?;
            Ok(Command::Deploy { src_dir })
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

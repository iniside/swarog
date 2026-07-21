//! Hand-rolled CLI parsing for `weles` (house style — no clap; see
//! `tools/verifyctl/src/cli.rs` for the pattern this file copies).

use std::path::PathBuf;

use anyhow::{bail, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Boot the deployed fleet. `dry_run` validates the deployed `fleet.toml`
    /// and exits WITHOUT acquiring the rollout lock, running any prepare hook,
    /// or spawning a service. `root` is the optional `--root <path>` override
    /// for the fleet root (see [`crate::prep::resolve_root`]).
    Up { dry_run: bool, root: Option<PathBuf> },
    /// Stage the fleet binaries from `src_dir` into `<root>/deploy`, stamping
    /// the chosen `fleet.toml` (`fleet`) into the generation as the fleet `up`
    /// will boot. `root` is the optional `--root <path>` override.
    Deploy { src_dir: String, fleet: String, root: Option<PathBuf> },
    Status { root: Option<PathBuf> },
    Down { root: Option<PathBuf> },
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
  weles deploy <src-dir> --fleet <fleet.toml> [--root <path>]
  weles up [--dry-run] [--root <path>]
  weles status [--root <path>]
  weles down [--root <path>]

weles has no concept of split/monolith — it boots whatever fleet was deployed.
`deploy` stamps the chosen --fleet <fleet.toml> into the generation; `up` reads
it back from <root>/deploy and boots it. `up --dry-run` validates the deployed
fleet.toml and exits without acquiring the rollout lock, running a prepare hook,
or spawning. weles never builds — it executes only the binaries staged into
<root>/deploy by `weles deploy` (<src-dir> resolves relative to the current
directory).

--root <path> pins the fleet root (state/lock/deploy live under it); without it,
WELES_ROOT then a walk up from the current directory to the repo marker decide
the root (see resolve_root). Accepted on up/deploy/status/down alike.";

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command> {
    let mut args = args.into_iter();
    let Some(verb) = args.next() else {
        bail!("missing command\n\n{USAGE}");
    };
    match verb.as_str() {
        "up" => {
            let mut dry_run = false;
            let mut root: Option<PathBuf> = None;
            while let Some(arg) = args.next() {
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
                    "--dry-run" => {
                        if dry_run {
                            bail!("--dry-run given more than once\n\n{USAGE}");
                        }
                        dry_run = true;
                    }
                    "--root" => take_root(&mut root, &mut args)?,
                    other => bail!("unknown argument {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Command::Up { dry_run, root })
        }
        "deploy" => {
            let mut src_dir: Option<String> = None;
            let mut fleet: Option<String> = None;
            let mut root: Option<PathBuf> = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--fleet" => {
                        if fleet.is_some() {
                            bail!("--fleet given more than once\n\n{USAGE}");
                        }
                        let Some(path) = args.next() else {
                            bail!("--fleet requires a path\n\n{USAGE}");
                        };
                        fleet = Some(path);
                    }
                    "--root" => take_root(&mut root, &mut args)?,
                    other if other.starts_with("--") => {
                        bail!("unknown argument {other:?}\n\n{USAGE}")
                    }
                    _ => {
                        if src_dir.is_some() {
                            bail!("deploy takes a single source directory\n\n{USAGE}");
                        }
                        src_dir = Some(arg);
                    }
                }
            }
            let Some(src_dir) = src_dir else {
                bail!("deploy requires a source directory\n\n{USAGE}");
            };
            let Some(fleet) = fleet else {
                bail!("deploy requires --fleet <fleet.toml>\n\n{USAGE}");
            };
            Ok(Command::Deploy { src_dir, fleet, root })
        }
        "status" => Ok(Command::Status { root: parse_root_only(args)? }),
        "down" => Ok(Command::Down { root: parse_root_only(args)? }),
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

/// Consumes the value after a `--root` flag into `slot`, rejecting a repeat or a
/// missing path. `slot` is the per-verb root accumulator so every verb parses
/// `--root <path>` identically.
fn take_root(slot: &mut Option<PathBuf>, args: &mut impl Iterator<Item = String>) -> Result<()> {
    if slot.is_some() {
        bail!("--root given more than once\n\n{USAGE}");
    }
    let Some(path) = args.next() else {
        bail!("--root requires a path\n\n{USAGE}");
    };
    *slot = Some(PathBuf::from(path));
    Ok(())
}

/// Parses the tail of a verb that takes ONLY an optional `--root <path>`
/// (`status`/`down`): any other token is an unknown argument.
fn parse_root_only(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>> {
    let mut args = args.into_iter();
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => take_root(&mut root, &mut args)?,
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    Ok(root)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;

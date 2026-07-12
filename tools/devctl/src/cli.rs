use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Topology {
    Monolith,
    Split,
}

impl Topology {
    pub fn name(self) -> &'static str {
        match self {
            Self::Monolith => "monolith",
            Self::Split => "split",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Command {
    Up {
        topology: Topology,
        skip_build: bool,
        overrides: Vec<(String, String)>,
    },
    Status,
    Down,
    Help,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command> {
    let args: Vec<_> = args.into_iter().collect();
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Command::Help);
    };
    match command {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "status" if args.len() == 1 => Ok(Command::Status),
        "down" if args.len() == 1 => Ok(Command::Down),
        "up" => parse_up(&args[1..]),
        other => bail!("unknown command {other:?}"),
    }
}

fn parse_up(args: &[String]) -> Result<Command> {
    let mut topology = Topology::Monolith;
    let mut topology_seen = false;
    let mut skip_build = false;
    let mut overrides = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "monolith" if !topology_seen => {
                topology_seen = true;
                topology = Topology::Monolith;
            }
            "split" if !topology_seen => {
                topology_seen = true;
                topology = Topology::Split;
            }
            "microservices" if !topology_seen => {
                eprintln!("devctl: warning: 'microservices' is deprecated; use 'split'");
                topology_seen = true;
                topology = Topology::Split;
            }
            "--skip-build" => skip_build = true,
            "--env" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--env requires KEY=VALUE"))?;
                let (key, value) = value
                    .split_once('=')
                    .filter(|(key, _)| !key.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("--env requires KEY=VALUE"))?;
                overrides.push((key.to_string(), value.to_string()));
            }
            arg => bail!("unexpected argument {arg:?}"),
        }
        index += 1;
    }
    Ok(Command::Up {
        topology,
        skip_build,
        overrides,
    })
}

pub const USAGE: &str = "\
devctl - owned foreground development fleet supervisor

USAGE:
  devctl up [monolith|split] [--skip-build] [--env KEY=VALUE]
  devctl status
  devctl down

'microservices' is a temporary deprecated alias for 'split'.";

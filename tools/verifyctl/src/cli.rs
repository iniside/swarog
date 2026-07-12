use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Level {
    Fast,
    All,
    Slow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Verify,
    BlessPublicApi,
    BlessContractGolden,
    Help,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    pub action: Action,
    pub level: Level,
    pub strict: bool,
    pub install: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options> {
    let mut options = Options {
        action: Action::Verify,
        level: Level::Fast,
        strict: false,
        install: true,
    };
    let mut level_seen = false;
    let mut action_seen = false;
    for arg in args {
        match arg.as_str() {
            "--fast" | "--all" | "--slow" => {
                if level_seen {
                    bail!("--fast, --all, and --slow are mutually exclusive");
                }
                level_seen = true;
                options.level = match arg.as_str() {
                    "--fast" => Level::Fast,
                    "--all" => Level::All,
                    _ => Level::Slow,
                };
            }
            "--strict" => options.strict = true,
            "--no-install" => options.install = false,
            "--bless-public-api" | "--bless-contract-golden" => {
                if action_seen {
                    bail!("bless actions are mutually exclusive");
                }
                action_seen = true;
                options.action = if arg == "--bless-public-api" {
                    Action::BlessPublicApi
                } else {
                    Action::BlessContractGolden
                };
            }
            "-h" | "--help" if !action_seen => options.action = Action::Help,
            other => bail!("unknown argument {other:?}"),
        }
    }
    if options.action != Action::Verify
        && options.action != Action::Help
        && (level_seen || options.strict || !options.install)
    {
        bail!("bless actions cannot be combined with verification options");
    }
    Ok(options)
}

pub const USAGE: &str = "\
verifyctl - typed game-backend verification runner

USAGE:
  verifyctl [--fast|--all|--slow] [--strict] [--no-install]
  verifyctl --bless-public-api
  verifyctl --bless-contract-golden

--fast is the default. Bless actions are reserved until their stages are ported.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_and_actions_are_closed_and_exclusive() {
        assert_eq!(parse(Vec::new()).unwrap().level, Level::Fast);
        assert_eq!(
            parse(["--all".into(), "--strict".into()]).unwrap().level,
            Level::All
        );
        assert!(parse(["--fast".into(), "--slow".into()]).is_err());
        assert!(parse([
            "--bless-public-api".into(),
            "--bless-contract-golden".into()
        ])
        .is_err());
        assert!(parse(["--bless-public-api".into(), "--strict".into()]).is_err());
    }
}

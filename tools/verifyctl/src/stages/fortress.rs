use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::model::Outcome;
use crate::runner::Context;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let mut packages = vec!["server".to_string()];
    let cmd = ctx.root.join("cmd");
    for entry in std::fs::read_dir(&cmd).with_context(|| format!("read {}", cmd.display()))? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|v| v.ends_with("-svc"))
        {
            continue;
        }
        let manifest = path.join("Cargo.toml");
        if manifest.is_file() {
            packages.push(package_name(&manifest)?);
        }
    }
    packages.sort();
    packages.dedup();
    let mut args = vec![OsString::from("build")];
    for package in packages {
        args.push(OsString::from("-p"));
        args.push(OsString::from(package));
    }
    if ctx.cargo_os("fortress", &args)? != Outcome::Pass {
        return Ok(Outcome::Fail);
    }
    for (label, args) in [
        ("archcheck", &["run", "-q", "-p", "archcheck"][..]),
        (
            "requirecheck",
            &["run", "-q", "-p", "requirecheck", "--", "--strict"][..],
        ),
        (
            "durability",
            &["run", "-q", "-p", "topiccheck", "--", "--durability-strict"][..],
        ),
    ] {
        if ctx.cargo(label, args)? != Outcome::Pass {
            return Ok(Outcome::Fail);
        }
    }
    Ok(Outcome::Pass)
}

fn package_name(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("name = \"")
                .and_then(|v| v.strip_suffix('"'))
        })
        .map(str::to_owned)
        .with_context(|| format!("missing package name in {}", path.display()))
}

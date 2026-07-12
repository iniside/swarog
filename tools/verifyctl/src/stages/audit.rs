use std::ffi::OsString;

use anyhow::Result;

use crate::model::{Outcome, SkipReason};
use crate::runner::Context;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let mut executable = ctx.resolve("cargo-audit");
    if executable.is_none() {
        if !ctx.options.install {
            ctx.note("cargo-audit is missing and installation was explicitly disabled")?;
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        let installed = match ctx.cargo("audit-install", &["install", "cargo-audit", "--locked"]) {
            Ok(outcome) => outcome,
            Err(error) => {
                ctx.note(&format!(
                    "cargo-audit installation invocation failed: {error:#}"
                ))?;
                return Ok(Outcome::Fail);
            }
        };
        executable = ctx.resolve("cargo-audit");
        if installed != Outcome::Pass || executable.is_none() {
            return Ok(Outcome::Fail);
        }
    }
    match ctx.command(
        "audit",
        executable.expect("resolved above"),
        vec![
            OsString::from("audit"),
            OsString::from("--ignore"),
            OsString::from("RUSTSEC-2023-0071"),
        ],
    ) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            ctx.note(&format!("cargo-audit invocation failed: {error:#}"))?;
            Ok(Outcome::Fail)
        }
    }
}

use anyhow::Result;

use crate::model::{Outcome, SkipReason};
use crate::runner::Context;

const VERSION: &str = "0.22.2";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    if !ctx.on_path("cargo-audit") {
        if !ctx.options.install {
            ctx.note("cargo-audit is missing and installation was explicitly disabled")?;
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        let installed = ctx.cargo(
            "audit-install",
            &["install", "cargo-audit", "--locked", "--version", VERSION],
        )?;
        if installed != Outcome::Pass || !ctx.on_path("cargo-audit") {
            return Ok(Outcome::Fail);
        }
    }
    ctx.cargo("audit", &["audit", "--ignore", "RUSTSEC-2023-0071"])
}

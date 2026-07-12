use crate::{
    model::{Outcome, SkipReason},
    runner::Context,
};
use anyhow::Result;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    if ctx.cargo("cargo-mutants-tool", &["mutants", "--version"])? != Outcome::Pass {
        if !ctx.options.install {
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        if ctx.cargo(
            "cargo-mutants-install",
            &["install", "cargo-mutants", "--locked"],
        )? != Outcome::Pass
        {
            return Ok(Outcome::Fail);
        }
    }
    ctx.cargo(
        "mutants",
        &[
            "mutants",
            "-p",
            "edge",
            "-p",
            "gateway",
            "-p",
            "asyncevents",
            "-p",
            "registry",
            "-p",
            "bus",
            "--timeout",
            "300",
        ],
    )
}

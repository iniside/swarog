use crate::{
    model::{Outcome, SkipReason},
    runner::Context,
};
use anyhow::Result;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    if cfg!(windows) {
        return Ok(Outcome::Skip(SkipReason::NotApplicablePlatform));
    }
    if ctx.cargo("cargo-fuzz-tool", &["fuzz", "--help"])? != Outcome::Pass {
        if !ctx.options.install {
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        if ctx.cargo("cargo-fuzz-install", &["install", "cargo-fuzz", "--locked"])? != Outcome::Pass
        {
            return Ok(Outcome::Fail);
        }
    }
    let cargo = ctx
        .resolve("cargo")
        .ok_or_else(|| anyhow::anyhow!("cargo missing"))?;
    for target in ["frame_decode", "wire_decode"] {
        let args = [
            "+nightly",
            "fuzz",
            "run",
            target,
            "--",
            "-max_total_time=10",
            "-runs=100000",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect();
        if ctx.command_at(target, cargo.clone(), args, ctx.root.join("core/edge"))? != Outcome::Pass
        {
            return Ok(Outcome::Fail);
        }
    }
    Ok(Outcome::Pass)
}

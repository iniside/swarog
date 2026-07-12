use anyhow::Result;

use crate::model::Outcome;
use crate::runner::Context;

pub fn build(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("build", &["build", "--workspace"])
}

pub fn clippy(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo(
        "clippy",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

pub fn test(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("test", &["test", "--workspace"])
}

pub fn routecheck(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("routecheck", &["run", "-q", "-p", "routecheck"])
}

use anyhow::Result;

use crate::model::Outcome;
use crate::runner::Context;

pub fn build(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("build", &["build", "--workspace", "--exclude", "verifyctl"])
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
    let workspace = ctx.cargo("test", &["test", "--workspace", "--exclude", "verifyctl"])?;
    if workspace != Outcome::Pass {
        return Ok(workspace);
    }
    let target = ctx.root.join("target/verifyctl-self");
    ctx.cargo_os(
        "test-verifyctl",
        &[
            "test".into(),
            "-p".into(),
            "verifyctl".into(),
            "--target-dir".into(),
            target.into_os_string(),
        ],
    )
}

pub fn routecheck(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("routecheck", &["run", "-q", "-p", "routecheck"])
}

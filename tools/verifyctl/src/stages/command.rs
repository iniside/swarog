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

/// The `test` stage's precondition. `testdb`'s opt-out makes every database test skip,
/// and libtest captures a PASSING test's output — so the stage would be green and silent
/// about having proven nothing. The false green this stage exists to prevent, one exported
/// variable away, which is why it is refused here rather than merely announced.
pub(crate) fn database_skip_refusal() -> Option<String> {
    testdb::skip_allowed().then(|| {
        format!(
            "{} is on: every database test would skip and this run would prove nothing. \
             Unset it and start Postgres before verifying.",
            testdb::SKIP_ENV
        )
    })
}

pub fn test(ctx: &mut Context<'_>) -> Result<Outcome> {
    if let Some(refusal) = database_skip_refusal() {
        eprintln!("verifyctl: {refusal}");
        ctx.note(&refusal)?;
        return Ok(Outcome::Fail);
    }
    // `--no-fail-fast`: without it cargo stops at the first failing test binary, so one red
    // crate silently leaves every alphabetically-later workspace package unexecuted.
    let workspace = ctx.cargo(
        "test",
        &["test", "--workspace", "--exclude", "verifyctl", "--no-fail-fast"],
    )?;
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

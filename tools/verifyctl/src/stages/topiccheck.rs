use crate::{model::Outcome, runner::Context};
use anyhow::Result;
pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo(
        "topiccheck",
        &["run", "-q", "-p", "topiccheck", "--", "--strict"],
    )
}

use anyhow::Result;

use crate::model::Outcome;
use crate::runner::Context;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.splitproof()
}

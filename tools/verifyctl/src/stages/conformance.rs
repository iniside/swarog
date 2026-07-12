use crate::{model::Outcome, runner::Context};
use anyhow::Result;

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    // TODO(step 8): add `--deny-gaps` once conformancecheck owns that policy flag.
    ctx.cargo("conformance", &["run", "-q", "-p", "conformancecheck"])
}

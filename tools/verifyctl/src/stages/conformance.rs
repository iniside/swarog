use crate::{model::Outcome, runner::Context};
use anyhow::Result;

const ARGS: &[&str] = &["run", "-q", "-p", "conformancecheck", "--", "--deny-gaps"];

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("conformance", ARGS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_stage_denies_conformance_gaps() {
        assert_eq!(
            ARGS,
            ["run", "-q", "-p", "conformancecheck", "--", "--deny-gaps"]
        );
    }
}

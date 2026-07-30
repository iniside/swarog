use crate::{
    model::Outcome,
    runner::{Context, Exit},
};
use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

const ARGS: &[&str] = &["run", "-q", "-p", "conformancecheck", "--", "--deny-gaps"];
const TARGET: &str = "tools/conformance/input-fields.golden.tsv";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo("conformance", ARGS)
}

pub fn bless(root: &Path) -> Result<Exit> {
    super::recover_pending_replacement(root)?;
    let temp = super::temp_dir(root, "input-golden-bless")?;
    let proposed = temp.join("input-fields.golden.tsv");
    let status = std::process::Command::new("cargo")
        .current_dir(root)
        .args([
            "run",
            "-q",
            "-p",
            "conformancecheck",
            "--",
            "--write-input-golden",
        ])
        .arg(&proposed)
        .status()
        .context("render input-field golden")?;
    if !status.success() || !proposed.is_file() {
        let _ = std::fs::remove_dir_all(&temp);
        return Ok(Exit::Failed);
    }
    let result = super::replace_recoverably(root, &[(PathBuf::from(TARGET), Some(proposed))]);
    let _ = std::fs::remove_dir_all(temp);
    result?;
    Ok(Exit::Green)
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

    /// The bless target must be the file the conformance stage byte-compares, or
    /// blessing would write a snapshot nothing reads.
    #[test]
    fn bless_target_is_the_committed_input_golden() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(root.join(TARGET).is_file(), "{TARGET} must exist");
    }
}

use crate::{
    model::Outcome,
    runner::{Context, Exit},
};
use anyhow::{Context as _, Result};
use std::path::PathBuf;

const TARGET: &str = "docs/reference/contract-golden/contracts.txt";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    ctx.cargo(
        "contract-golden",
        &["run", "-q", "-p", "topiccheck", "--", "contract-golden"],
    )
}

pub fn bless() -> Result<Exit> {
    let root = super::workspace_root()?;
    super::recover_pending_replacement(&root)?;
    let temp = super::temp_dir(&root, "contract-golden-bless")?;
    let proposed = temp.join("contracts.txt");
    let status = std::process::Command::new("cargo")
        .current_dir(&root)
        .args([
            "run",
            "-q",
            "-p",
            "topiccheck",
            "--",
            "contract-golden",
            "--output",
        ])
        .arg(&proposed)
        .status()
        .context("render contract golden")?;
    if !status.success() || !proposed.is_file() {
        let _ = std::fs::remove_dir_all(&temp);
        return Ok(Exit::Failed);
    }
    let result = super::replace_recoverably(&root, &[(PathBuf::from(TARGET), Some(proposed))]);
    let _ = std::fs::remove_dir_all(temp);
    result?;
    Ok(Exit::Green)
}

use crate::{model::Outcome, runner::Context};
use anyhow::{Context as _, Result};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let temp = super::temp_dir(&ctx.log_dir, "codegen")?;
    let args = vec![
        OsString::from("run"),
        OsString::from("-q"),
        OsString::from("-p"),
        OsString::from("csharp-client-gen"),
        OsString::from("--"),
        OsString::from("--out"),
        temp.clone().into_os_string(),
    ];
    if ctx.cargo_os("codegen-generate", &args)? != Outcome::Pass {
        let _ = std::fs::remove_dir_all(temp);
        return Ok(Outcome::Fail);
    }
    let expected = tree(&temp)?;
    let actual = tree(&ctx.root.join("clients/csharp/Generated"))?;
    let pass = expected == actual;
    if !pass {
        ctx.note(&format!(
            "generated tree differs: expected {:?}, actual {:?}",
            expected.keys(),
            actual.keys()
        ))?;
    }
    let _ = std::fs::remove_dir_all(temp);
    Ok(if pass { Outcome::Pass } else { Outcome::Fail })
}

fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        if !at.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(at).with_context(|| format!("read {}", at.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else if path.is_file() {
                out.insert(path.strip_prefix(base)?.to_path_buf(), std::fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

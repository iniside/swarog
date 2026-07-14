use crate::{model::Outcome, runner::Context};
use anyhow::{Context as _, Result};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// `codegen-freshness` gates every committed GENERATED artifact against a fresh
/// regeneration: the C# client tree AND the `opscatalog/src/generated.rs` ops catalog.
/// Both must be byte-identical to their committed form or the stage FAILs (drift = a
/// stale artifact). Each is regenerated from the same impl-free `route_bindings()`
/// metadata the gateway route table uses, so a new/changed `#[http]` op that was not
/// re-blessed is caught here. Re-bless: `cargo run -p csharp-client-gen` (into
/// `clients/csharp/Generated`) / `cargo run -p opscatalog-gen`.
pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let csharp = csharp_fresh(ctx)?;
    let opscatalog = opscatalog_fresh(ctx)?;
    Ok(if csharp == Outcome::Pass && opscatalog == Outcome::Pass {
        Outcome::Pass
    } else {
        Outcome::Fail
    })
}

fn csharp_fresh(ctx: &mut Context<'_>) -> Result<Outcome> {
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

/// Regenerates the ops catalog to a temp file and diffs it against the committed
/// `opscatalog/src/generated.rs`. Any difference FAILs — the committed artifact is stale
/// against the live `#[http]` op surface. Re-bless with `cargo run -p opscatalog-gen`.
fn opscatalog_fresh(ctx: &mut Context<'_>) -> Result<Outcome> {
    let temp = super::temp_dir(&ctx.log_dir, "opscatalog")?;
    let out = temp.join("generated.rs");
    let args = vec![
        OsString::from("run"),
        OsString::from("-q"),
        OsString::from("-p"),
        OsString::from("opscatalog-gen"),
        OsString::from("--"),
        OsString::from("--out"),
        out.clone().into_os_string(),
    ];
    if ctx.cargo_os("opscatalog-generate", &args)? != Outcome::Pass {
        let _ = std::fs::remove_dir_all(temp);
        return Ok(Outcome::Fail);
    }
    let expected = std::fs::read(&out).with_context(|| format!("read {}", out.display()))?;
    let committed = ctx.root.join("opscatalog/src/generated.rs");
    let actual = std::fs::read(&committed).unwrap_or_default();
    let pass = expected == actual;
    if !pass {
        ctx.note(
            "opscatalog/src/generated.rs is stale against the live #[http] op surface -- \
             re-bless with `cargo run -p opscatalog-gen`",
        )?;
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

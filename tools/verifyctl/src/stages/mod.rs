pub mod admincheck;
pub mod audit;
pub mod codegen;
pub mod command;
pub mod conformance;
pub mod contract_golden;
pub mod csharp;
pub mod docs_current;
pub mod fortress;
pub mod fuzz;
pub mod mutants;
pub mod public_api;
pub mod splitproof;
pub mod topiccheck;
pub mod weles_async_island;
pub mod weles_fleet_parity;

use crate::model::{StageClass, StageId};
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};

pub type StageFn = fn(&mut crate::runner::Context<'_>) -> anyhow::Result<crate::model::Outcome>;

#[derive(Clone, Copy)]
pub struct Stage {
    pub id: StageId,
    pub class: StageClass,
    pub run: StageFn,
}

pub const BLOCKING: &[Stage] = &[
    Stage {
        id: StageId::Build,
        class: StageClass::Blocking,
        run: command::build,
    },
    Stage {
        id: StageId::Clippy,
        class: StageClass::Blocking,
        run: command::clippy,
    },
    Stage {
        id: StageId::Test,
        class: StageClass::Blocking,
        run: command::test,
    },
    Stage {
        id: StageId::Audit,
        class: StageClass::Blocking,
        run: audit::run,
    },
    Stage {
        id: StageId::Fortress,
        class: StageClass::Blocking,
        run: fortress::run,
    },
    Stage {
        id: StageId::Routecheck,
        class: StageClass::Blocking,
        run: command::routecheck,
    },
    Stage {
        id: StageId::CodegenFreshness,
        class: StageClass::Blocking,
        run: codegen::run,
    },
    Stage {
        id: StageId::ContractGolden,
        class: StageClass::Blocking,
        run: contract_golden::run,
    },
    Stage {
        id: StageId::Conformance,
        class: StageClass::Blocking,
        run: conformance::run,
    },
    Stage {
        id: StageId::DocsCurrent,
        class: StageClass::Blocking,
        run: docs_current::run,
    },
    Stage {
        id: StageId::WelesFleetParity,
        class: StageClass::Blocking,
        run: weles_fleet_parity::run,
    },
    Stage {
        id: StageId::WelesAsyncIsland,
        class: StageClass::Blocking,
        run: weles_async_island::run,
    },
    Stage {
        id: StageId::SplitProof,
        class: StageClass::Blocking,
        run: splitproof::run,
    },
];

pub const ADVISORY: &[Stage] = &[
    Stage {
        id: StageId::PublicApi,
        class: StageClass::Advisory,
        run: public_api::run,
    },
    Stage {
        id: StageId::Fuzz,
        class: StageClass::Advisory,
        run: fuzz::run,
    },
    Stage {
        id: StageId::CSharp,
        class: StageClass::Advisory,
        run: csharp::run,
    },
    Stage {
        id: StageId::Topiccheck,
        class: StageClass::Advisory,
        run: topiccheck::run,
    },
    Stage {
        id: StageId::Admincheck,
        class: StageClass::Advisory,
        run: admincheck::run,
    },
];

pub const SLOW: &[Stage] = &[Stage {
    id: StageId::Mutants,
    class: StageClass::Slow,
    run: mutants::run,
}];

pub fn manifest(level: crate::cli::Level, strict: bool) -> Vec<Stage> {
    let mut stages = BLOCKING.to_vec();
    if strict || matches!(level, crate::cli::Level::All | crate::cli::Level::Slow) {
        stages.extend_from_slice(ADVISORY);
    }
    if level == crate::cli::Level::Slow {
        stages.extend_from_slice(SLOW);
    }
    stages
}

pub(crate) fn temp_dir(parent: &Path, label: &str) -> Result<PathBuf> {
    let path = parent.join(format!(
        ".{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(path)
}

/// Replaces a fully-rendered baseline set while retaining enough state to recover after
/// an interrupted replacement. Every proposal is validated before the first tracked write.
pub(crate) fn replace_recoverably(
    root: &Path,
    proposals: &[(PathBuf, Option<PathBuf>)],
) -> Result<()> {
    let state = root.join("run/verify/bless-transaction");
    recover_replacement(root, &state)?;
    for source in proposals.iter().filter_map(|(_, source)| source.as_ref()) {
        if !source.is_file() {
            bail!("proposed baseline {} is missing", source.display());
        }
        let _ = std::fs::read(source).with_context(|| format!("validate {}", source.display()))?;
    }
    let backup = state.join("backup");
    std::fs::create_dir_all(&backup)?;
    let mut manifest = String::new();
    for (index, (relative, _)) in proposals.iter().enumerate() {
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("unsafe baseline path {}", relative.display());
        }
        let target = root.join(relative);
        let existed = target.is_file();
        manifest.push_str(&format!(
            "{}\t{}\t{}\n",
            index,
            if existed { 1 } else { 0 },
            relative.to_string_lossy()
        ));
        if existed {
            std::fs::copy(&target, backup.join(index.to_string()))?;
        }
    }
    std::fs::write(state.join("manifest.tsv"), manifest)?;
    let result = (|| -> Result<()> {
        for (index, (relative, source)) in proposals.iter().enumerate() {
            if std::env::var("VERIFYCTL_BLESS_FAIL_AT").ok().as_deref() == Some(&index.to_string())
            {
                bail!("injected replacement failure at {index}");
            }
            let target = root.join(relative);
            let Some(source) = source else {
                if target.is_file() {
                    std::fs::remove_file(target)?;
                }
                continue;
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let staged = target.with_extension(format!("verifyctl-new-{}", std::process::id()));
            std::fs::copy(source, &staged)?;
            if target.exists() {
                std::fs::remove_file(&target)?;
            }
            std::fs::rename(staged, target)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        recover_replacement(root, &state)?;
        return Err(error.context("replace baselines; previous files restored"));
    }
    std::fs::remove_dir_all(state)?;
    Ok(())
}

pub(crate) fn recover_pending_replacement(root: &Path) -> Result<()> {
    recover_replacement(root, &root.join("run/verify/bless-transaction"))
}

fn recover_replacement(root: &Path, state: &Path) -> Result<()> {
    let manifest_path = state.join("manifest.tsv");
    if !manifest_path.is_file() {
        return Ok(());
    }
    let manifest = std::fs::read_to_string(&manifest_path)?;
    for line in manifest.lines().rev() {
        let mut fields = line.splitn(3, '\t');
        let index = fields.next().context("backup index")?;
        let existed = fields.next().context("backup presence")? == "1";
        let relative = PathBuf::from(fields.next().context("backup path")?);
        let target = root.join(relative);
        if existed {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(state.join("backup").join(index), target)?;
        } else if target.is_file() {
            std::fs::remove_file(target)?;
        }
    }
    std::fs::remove_dir_all(state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_manifest_is_frozen() {
        let names = |level| {
            manifest(level, false)
                .iter()
                .map(|s| s.id.name())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(crate::cli::Level::Fast),
            vec![
                "build",
                "clippy",
                "test",
                "audit",
                "fortress",
                "routecheck",
                "codegen-freshness",
                "contract-golden",
                "conformance",
                "docs-current",
                "weles-fleet-parity",
                "weles-async-island",
                "split-proof"
            ]
        );
        assert_eq!(
            &names(crate::cli::Level::All)[13..],
            &["public-api", "fuzz", "csharp-client", "topiccheck", "admincheck"]
        );
        assert_eq!(names(crate::cli::Level::Slow).last(), Some(&"mutants"));
        assert_eq!(manifest(crate::cli::Level::Fast, true).len(), 18);
    }

    #[test]
    fn recoverable_replace_rolls_back_every_completed_write() {
        let root = temp_dir(&std::env::temp_dir(), "verifyctl-rollback").unwrap();
        std::fs::create_dir_all(root.join("tracked")).unwrap();
        std::fs::write(root.join("tracked/a"), "old-a").unwrap();
        std::fs::write(root.join("tracked/b"), "old-b").unwrap();
        std::fs::write(root.join("new-a"), "new-a").unwrap();
        std::fs::write(root.join("new-b"), "new-b").unwrap();
        for fail_at in ["0", "1"] {
            std::fs::write(root.join("tracked/a"), "old-a").unwrap();
            std::fs::write(root.join("tracked/b"), "old-b").unwrap();
            std::env::set_var("VERIFYCTL_BLESS_FAIL_AT", fail_at);
            assert!(replace_recoverably(
                &root,
                &[
                    ("tracked/a".into(), Some(root.join("new-a"))),
                    ("tracked/b".into(), Some(root.join("new-b")))
                ]
            )
            .is_err());
            std::env::remove_var("VERIFYCTL_BLESS_FAIL_AT");
            assert_eq!(
                std::fs::read_to_string(root.join("tracked/a")).unwrap(),
                "old-a"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("tracked/b")).unwrap(),
                "old-b"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

pub mod admincheck;
pub mod audit;
pub mod codegen;
pub mod command;
pub mod conformance;
pub mod contract_golden;
pub mod csharp;
pub mod docs_current;
pub(crate) mod fake_http;
pub mod fortress;
pub mod fuzz;
pub mod mutants;
pub mod public_api;
pub mod splitproof;
pub mod topiccheck;
pub mod weles_async_island;
pub mod weles_fleet_parity;
pub mod weles_managed_gateway;
pub mod weles_wire_contract;

use crate::model::{Outcome, Platform, SkipReason, StageClass, StageId};
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};

pub type StageFn = fn(&mut crate::runner::Context<'_>) -> anyhow::Result<crate::model::Outcome>;

#[derive(Clone, Copy)]
pub struct Stage {
    pub id: StageId,
    pub class: StageClass,
    /// Platforms on which this stage is DECLARED not-applicable — a static
    /// property of (stage, platform), auditable in this table rather than
    /// discovered at runtime from the exit code of the program under test. The
    /// runner short-circuits to `Skip(NotApplicablePlatform)` WITHOUT calling
    /// `run` when the current platform is listed here. Only ADVISORY stages may
    /// carry an entry and stay green (see `Summary::failed`).
    pub not_applicable_on: &'static [Platform],
    pub run: StageFn,
}

impl Stage {
    /// The outcome the runner records WITHOUT running `run`, when this stage is
    /// declared not-applicable on the current platform; `None` means "run it".
    /// This is the single decision point for the platform short-circuit — the
    /// runner calls it in place of `run`, and it never touches `run`.
    pub fn platform_short_circuit(&self) -> Option<Outcome> {
        Platform::current()
            .is_some_and(|current| self.not_applicable_on.contains(&current))
            .then_some(Outcome::Skip(SkipReason::NotApplicablePlatform))
    }
}

pub const BLOCKING: &[Stage] = &[
    Stage {
        id: StageId::Build,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: command::build,
    },
    Stage {
        id: StageId::Clippy,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: command::clippy,
    },
    Stage {
        id: StageId::Test,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: command::test,
    },
    Stage {
        id: StageId::Audit,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: audit::run,
    },
    Stage {
        id: StageId::Fortress,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: fortress::run,
    },
    Stage {
        id: StageId::Routecheck,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: command::routecheck,
    },
    Stage {
        id: StageId::CodegenFreshness,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: codegen::run,
    },
    Stage {
        id: StageId::ContractGolden,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: contract_golden::run,
    },
    Stage {
        id: StageId::Conformance,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: conformance::run,
    },
    Stage {
        id: StageId::DocsCurrent,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: docs_current::run,
    },
    Stage {
        id: StageId::WelesFleetParity,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: weles_fleet_parity::run,
    },
    Stage {
        id: StageId::WelesAsyncIsland,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: weles_async_island::run,
    },
    Stage {
        id: StageId::WelesWireContract,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: weles_wire_contract::run,
    },
    Stage {
        id: StageId::SplitProof,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: splitproof::run,
    },
    // LAST, and after split-proof deliberately: both boot a fleet against the
    // one shared Postgres (one rollout at a time), and this is the newer of the
    // two — a wedge here must not be able to cost the long-established proof its
    // run. Both borrow the same lease, in turn.
    Stage {
        id: StageId::WelesManagedGateway,
        class: StageClass::Blocking,
        not_applicable_on: &[],
        run: weles_managed_gateway::run,
    },
];

pub const ADVISORY: &[Stage] = &[
    Stage {
        id: StageId::PublicApi,
        class: StageClass::Advisory,
        not_applicable_on: &[],
        run: public_api::run,
    },
    Stage {
        id: StageId::Fuzz,
        class: StageClass::Advisory,
        // cargo-fuzz's runtime ships for Unix only; declared, not sniffed.
        not_applicable_on: &[Platform::Windows],
        run: fuzz::run,
    },
    Stage {
        id: StageId::CSharp,
        class: StageClass::Advisory,
        // msquic (the QUIC transport the C# fixture needs) ships Windows/Linux
        // only — no macOS build. Declared here so the stage short-circuits
        // BEFORE booting a monolith, instead of the old runtime exit-3 sniff
        // that could not tell a platform gap from a real client bug.
        not_applicable_on: &[Platform::MacOs],
        run: csharp::run,
    },
    Stage {
        id: StageId::Topiccheck,
        class: StageClass::Advisory,
        not_applicable_on: &[],
        run: topiccheck::run,
    },
    Stage {
        id: StageId::Admincheck,
        class: StageClass::Advisory,
        not_applicable_on: &[],
        run: admincheck::run,
    },
];

pub const SLOW: &[Stage] = &[Stage {
    id: StageId::Mutants,
    class: StageClass::Slow,
    not_applicable_on: &[],
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
                "weles-wire-contract",
                "split-proof",
                "weles-managed-gateway"
            ]
        );
        assert_eq!(
            &names(crate::cli::Level::All)[15..],
            &["public-api", "fuzz", "csharp-client", "topiccheck", "admincheck"]
        );
        assert_eq!(names(crate::cli::Level::Slow).last(), Some(&"mutants"));
        assert_eq!(manifest(crate::cli::Level::Fast, true).len(), 20);
    }

    #[test]
    fn every_stage_class_matches_the_manifest_it_lives_in() {
        // Which array a stage sits in is NOT what makes it blocking: `Summary`
        // keys off `result.class`, copied from `stage.class` in the runner. So
        // flipping a `class` to `Advisory` while leaving the stage in `BLOCKING`
        // would silently make it non-blocking — and `stage_manifest_is_frozen`,
        // which only reads names, would stay green. This is the assertion that
        // makes the two agree.
        assert!(BLOCKING.iter().all(|s| s.class == StageClass::Blocking));
        assert!(ADVISORY.iter().all(|s| s.class == StageClass::Advisory));
        assert!(SLOW.iter().all(|s| s.class == StageClass::Slow));
    }

    #[test]
    fn a_stage_not_applicable_here_short_circuits_without_running() {
        // A stage whose `run` panics: if the short-circuit ever reached `run`,
        // this test would panic instead of asserting.
        fn boom(_: &mut crate::runner::Context<'_>) -> anyhow::Result<Outcome> {
            panic!("run must not execute for a platform-exempt stage");
        }
        let all_platforms = &[Platform::Windows, Platform::Linux, Platform::MacOs];
        let exempt = Stage {
            id: StageId::Fuzz,
            class: StageClass::Advisory,
            not_applicable_on: all_platforms,
            run: boom,
        };
        // Declared not-applicable on every platform → short-circuit on any host,
        // WITHOUT touching `run`. The runner records exactly this in place of it.
        assert_eq!(
            exempt.platform_short_circuit(),
            Some(Outcome::Skip(SkipReason::NotApplicablePlatform))
        );

        // An applicable stage has no short-circuit → the runner calls `run`.
        let applicable = Stage {
            not_applicable_on: &[],
            ..exempt
        };
        assert_eq!(applicable.platform_short_circuit(), None);
    }

    #[test]
    fn platform_declarations_resolve_per_os() {
        let stage = |id| {
            BLOCKING
                .iter()
                .chain(ADVISORY)
                .chain(SLOW)
                .find(|s| s.id == id)
                .unwrap()
        };
        // The two declared exemptions, the whole set of them.
        assert_eq!(stage(StageId::Fuzz).not_applicable_on, &[Platform::Windows]);
        assert_eq!(stage(StageId::CSharp).not_applicable_on, &[Platform::MacOs]);
        // fuzz short-circuits ONLY on Windows; csharp ONLY on macOS. On the
        // current host, at most one of these declares itself exempt.
        let on_windows = Platform::current() == Some(Platform::Windows);
        let on_macos = Platform::current() == Some(Platform::MacOs);
        assert_eq!(
            stage(StageId::Fuzz).platform_short_circuit().is_some(),
            on_windows
        );
        assert_eq!(
            stage(StageId::CSharp).platform_short_circuit().is_some(),
            on_macos
        );
        // No BLOCKING (or SLOW) stage may carry a platform exemption: only an
        // ADVISORY stage may platform-escape green (`Summary::failed`).
        for s in BLOCKING.iter().chain(SLOW) {
            assert!(
                s.not_applicable_on.is_empty(),
                "{} is not ADVISORY and must not declare a platform exemption",
                s.id.name()
            );
        }
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

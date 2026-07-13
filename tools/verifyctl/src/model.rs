use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum StageId {
    Build,
    Clippy,
    Test,
    Audit,
    Fortress,
    Routecheck,
    CodegenFreshness,
    ContractGolden,
    Conformance,
    DocsCurrent,
    SplitProof,
    PublicApi,
    Fuzz,
    CSharp,
    Topiccheck,
    Mutants,
}

impl StageId {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Clippy => "clippy",
            Self::Test => "test",
            Self::Audit => "audit",
            Self::Fortress => "fortress",
            Self::Routecheck => "routecheck",
            Self::CodegenFreshness => "codegen-freshness",
            Self::ContractGolden => "contract-golden",
            Self::Conformance => "conformance",
            Self::DocsCurrent => "docs-current",
            Self::SplitProof => "split-proof",
            Self::PublicApi => "public-api",
            Self::Fuzz => "fuzz",
            Self::CSharp => "csharp-client",
            Self::Topiccheck => "topiccheck",
            Self::Mutants => "mutants",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageClass {
    Blocking,
    Advisory,
    Slow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    ExplicitNoInstallMissingTool,
    NotApplicablePlatform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Pass,
    Fail,
    Skip(SkipReason),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("PASS"),
            Self::Fail => f.write_str("FAIL"),
            Self::Skip(_) => f.write_str("SKIP"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageResult {
    pub id: StageId,
    pub class: StageClass,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    pub results: Vec<StageResult>,
}

impl Summary {
    pub fn push(&mut self, result: StageResult) {
        self.results.push(result);
    }

    pub fn failed(&self, strict: bool) -> bool {
        self.results.iter().any(|result| match result.outcome {
            Outcome::Fail => result.class != StageClass::Advisory || strict,
            Outcome::Skip(SkipReason::NotApplicablePlatform) => false,
            Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool) => false,
            Outcome::Pass => false,
        })
    }

    pub fn print(&self) {
        println!();
        println!("=== verify summary ===");
        println!(
            "{:<20} | {:<6} | {:<8} | Reason",
            "Stage", "Status", "Class"
        );
        for result in &self.results {
            let reason = match result.outcome {
                Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool) => {
                    "no-install: missing tool"
                }
                Outcome::Skip(SkipReason::NotApplicablePlatform) => {
                    "not applicable on this platform"
                }
                _ => "",
            };
            println!(
                "{:<20} | {:<6} | {:<8} | {}",
                result.id.name(),
                result.outcome,
                match result.class {
                    StageClass::Blocking => "blocking",
                    StageClass::Advisory => "advisory",
                    StageClass::Slow => "slow",
                },
                reason
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(class: StageClass, outcome: Outcome) -> StageResult {
        StageResult {
            id: StageId::Fuzz,
            class,
            outcome,
        }
    }

    #[test]
    fn summary_exit_matrix_preserves_strict_and_platform_rules() {
        let mut summary = Summary::default();
        summary.push(result(StageClass::Advisory, Outcome::Fail));
        assert!(!summary.failed(false));
        assert!(summary.failed(true));

        let mut platform = Summary::default();
        platform.push(result(
            StageClass::Advisory,
            Outcome::Skip(SkipReason::NotApplicablePlatform),
        ));
        assert!(!platform.failed(true));

        let mut no_install = Summary::default();
        no_install.push(result(
            StageClass::Blocking,
            Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool),
        ));
        assert!(!no_install.failed(true));

        let mut slow = Summary::default();
        slow.push(result(StageClass::Slow, Outcome::Fail));
        assert!(slow.failed(false));
    }
}

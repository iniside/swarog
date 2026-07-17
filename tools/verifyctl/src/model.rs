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
    SupportedTargets,
    SplitProof,
    PublicApi,
    Fuzz,
    CSharp,
    Topiccheck,
    Admincheck,
    WelesFleetParity,
    WelesAsyncIsland,
    WelesWireContract,
    WelesManagedGateway,
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
            Self::SupportedTargets => "supported-targets",
            Self::SplitProof => "split-proof",
            Self::PublicApi => "public-api",
            Self::Fuzz => "fuzz",
            Self::CSharp => "csharp-client",
            Self::Topiccheck => "topiccheck",
            Self::Admincheck => "admincheck",
            Self::WelesFleetParity => "weles-fleet-parity",
            Self::WelesAsyncIsland => "weles-async-island",
            Self::WelesWireContract => "weles-wire-contract",
            Self::WelesManagedGateway => "weles-managed-gateway",
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

/// The host a stage runs on. Applicability is declared as DATA on the `Stage`
/// table (`stages::Stage::not_applicable_on`) and resolved against
/// `Platform::current()` by the runner BEFORE `run` — never sniffed at runtime
/// from the exit code of the program under test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
}

impl Platform {
    /// The platform this process is running on, or `None` for an OS the port
    /// does not model (in which case no stage is treated as platform-exempt —
    /// it runs and is scored honestly).
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::MacOs),
            _ => None,
        }
    }
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
            // A platform exemption is only legitimate for a non-required stage.
            // A BLOCKING (or SLOW) stage must run on every platform it is asked
            // to run on; declaring it not-applicable here is a contradiction and
            // must not pass silently — default OR --strict. An ADVISORY stage
            // (fuzz on Windows) legitimately declares the exemption and stays
            // green in both modes: making it fail under --strict would
            // permanently red-wall the Windows dev box on `--all --strict`.
            Outcome::Skip(SkipReason::NotApplicablePlatform) => {
                result.class != StageClass::Advisory
            }
            // Deliberately kept green: CLAUDE.md sanctions a SKIP only for a
            // missing tool under explicit --no-install ("only a missing tool
            // with explicit --no-install is labeled SKIP"). Not to be
            // re-litigated with the NotApplicablePlatform rule above.
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
        // Advisory FAIL: green by default, red under --strict (unchanged).
        let mut summary = Summary::default();
        summary.push(result(StageClass::Advisory, Outcome::Fail));
        assert!(!summary.failed(false));
        assert!(summary.failed(true));

        // ADVISORY + NotApplicablePlatform → green, default AND --strict. This is
        // fuzz-on-Windows's legitimate exemption; the branch a naive "fail under
        // strict" fix would break, permanently red-walling `--all --strict`.
        let mut advisory_platform = Summary::default();
        advisory_platform.push(result(
            StageClass::Advisory,
            Outcome::Skip(SkipReason::NotApplicablePlatform),
        ));
        assert!(!advisory_platform.failed(false));
        assert!(!advisory_platform.failed(true));

        // BLOCKING + NotApplicablePlatform → FAIL, default AND --strict. The new
        // rule: a blocking stage must run everywhere; a platform escape is a
        // contradiction that must not green-pass (the reversal recorded in the
        // commit and Step 11 erratum).
        let mut blocking_platform = Summary::default();
        blocking_platform.push(result(
            StageClass::Blocking,
            Outcome::Skip(SkipReason::NotApplicablePlatform),
        ));
        assert!(blocking_platform.failed(false));
        assert!(blocking_platform.failed(true));

        // BLOCKING + ExplicitNoInstallMissingTool → green (unchanged,
        // deliberately kept — CLAUDE.md's sanctioned SKIP).
        let mut no_install = Summary::default();
        no_install.push(result(
            StageClass::Blocking,
            Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool),
        ));
        assert!(!no_install.failed(false));
        assert!(!no_install.failed(true));

        let mut slow = Summary::default();
        slow.push(result(StageClass::Slow, Outcome::Fail));
        assert!(slow.failed(false));
    }
}

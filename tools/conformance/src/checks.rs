//! The harness's pure logic: drift preflight diff, completeness matrix, and the
//! per-case evaluations that can be judged from plain data (probe results, argon
//! params). Everything here is deterministic and side-effect-free so `tests.rs`
//! can prove the failure modes — including the negative proof that a forgotten
//! module produces the expected per-entry drift error.

use std::collections::BTreeSet;

use crate::model::{ArgonParams, Convention, Entry, Fixture, Stance};

/// Core-infra modules hosted in every process that are NOT fortresses under
/// `modules/` — they appear in `checkmodules::monolith_modules()` but carry no
/// conformance entry. Mirrors CLAUDE.md rule 4: process infrastructure (metrics,
/// the planes, the DB, HTTP) is never declared as a domain capability, and the
/// fortress rule applies to `modules/` only. A named const, not a magic filter,
/// so adding a second core-infra module is an explicit, reviewable edit here.
pub const CORE_INFRA_MODULES: &[&str] = &["metrics"];

/// Stable kebab label for one convention — the report's column header and the
/// key used in every finding line.
pub fn conv_label(c: Convention) -> &'static str {
    match c {
        Convention::EnvValidation => "env-validation",
        Convention::InputByteCaps => "input-byte-caps",
        Convention::InfraOutage503 => "infra-outage-503",
        Convention::ArgonParity => "argon-parity",
    }
}

/// Does the fixture variant carry the payload its convention expects? A stance
/// like `(Convention::InputByteCaps, Applies(Fixture::EnvValidation(…)))` is a
/// wiring bug the completeness matrix must catch, not silently execute.
fn fixture_matches(c: Convention, f: &Fixture) -> bool {
    matches!(
        (c, f),
        (Convention::EnvValidation, Fixture::EnvValidation(_))
            | (Convention::InputByteCaps, Fixture::InputByteCaps(_))
            | (Convention::InfraOutage503, Fixture::InfraOutage503(_))
            | (Convention::ArgonParity, Fixture::ArgonParity(_))
    )
}

/// Phase 1 — the didn't-forget preflight. Three-way diff between (1) the
/// `modules/*` directories on disk, (2) the `entry.module` names in the
/// harness's hand-maintained `entries()` list, and (3) the `Module::name()` set
/// of the monolith (minus [`CORE_INFRA_MODULES`]). Every mismatch is its own
/// line with the concrete fix, so a forgotten module reads as an instruction,
/// not a puzzle. Any finding fails the run before a single assertion executes.
pub fn drift_findings(
    disk: &BTreeSet<String>,
    entry_names: &BTreeSet<String>,
    monolith_raw: &BTreeSet<String>,
) -> Vec<String> {
    let monolith: BTreeSet<&String> = monolith_raw
        .iter()
        .filter(|m| !CORE_INFRA_MODULES.contains(&m.as_str()))
        .collect();

    let mut findings = Vec::new();
    for m in disk.iter().filter(|m| !entry_names.contains(*m)) {
        findings.push(format!(
            "modules/{m} on disk has no conformance entry — add {m}::conformance::entry() \
             to tools/conformance policy"
        ));
    }
    for m in entry_names.iter().filter(|m| !disk.contains(*m)) {
        findings.push(format!(
            "conformance entry \"{m}\" has no modules/{m} directory on disk — remove the \
             stale entry from tools/conformance policy"
        ));
    }
    for m in monolith.iter().filter(|m| !entry_names.contains(**m)) {
        findings.push(format!(
            "monolith module \"{m}\" (checkmodules::monolith_modules) has no conformance \
             entry — add {m} to tools/conformance policy"
        ));
    }
    for m in entry_names.iter().filter(|m| !monolith.contains(m)) {
        findings.push(format!(
            "conformance entry \"{m}\" is not in the monolith module set \
             (checkmodules::monolith_modules) — register the module in cmd/server's lib \
             or remove the entry"
        ));
    }
    for m in disk.iter().filter(|m| !monolith.contains(m)) {
        findings.push(format!(
            "modules/{m} on disk is not in the monolith module set — register it in \
             cmd/server's lib"
        ));
    }
    for m in monolith.iter().filter(|m| !disk.contains(**m)) {
        findings.push(format!(
            "monolith module \"{m}\" has no modules/{m} directory on disk"
        ));
    }
    findings
}

/// Phase 2 — the completeness matrix. Every entry must declare exactly one
/// stance for every [`Convention::ALL`]; a `NotApplicable` needs a non-empty
/// `why`; an `Applies` must carry the matching fixture variant with at least
/// one case (a zero-case fixture is vacuously green — that is silence wearing
/// an Applies costume).
pub fn completeness_findings(entries: &[Entry]) -> Vec<String> {
    let mut findings = Vec::new();
    for entry in entries {
        let module = entry.module;
        for conv in Convention::ALL {
            let label = conv_label(conv);
            let declared: Vec<&Stance> = entry
                .stances
                .iter()
                .filter(|(c, _)| *c == conv)
                .map(|(_, s)| s)
                .collect();
            if declared.len() > 1 {
                findings.push(format!(
                    "{module}: {label} declared {} times — exactly one stance per convention",
                    declared.len()
                ));
            }
            let Some(stance) = declared.first() else {
                findings.push(format!(
                    "{module}: no stance declared for {label} — silence is not a stance"
                ));
                continue;
            };
            match stance {
                Stance::NotApplicable { why } => {
                    if why.trim().is_empty() {
                        findings.push(format!(
                            "{module}: NotApplicable for {label} with an empty why — a \
                             reviewer-checkable sentence is required"
                        ));
                    }
                }
                Stance::KnownGap { why, remediation } => {
                    if why.trim().is_empty() || remediation.trim().is_empty() {
                        findings.push(format!(
                            "{module}: KnownGap for {label} requires non-empty why and remediation"
                        ));
                    }
                }
                Stance::Applies(fixture) => {
                    if !fixture_matches(conv, fixture) {
                        findings.push(format!(
                            "{module}: stance for {label} carries a mismatched fixture \
                             variant — the fixture must match its convention"
                        ));
                        continue;
                    }
                    let cases = match fixture {
                        Fixture::EnvValidation(v) => v.len(),
                        Fixture::InputByteCaps(v) => v.len(),
                        Fixture::InfraOutage503(v) => v.len(),
                        Fixture::ArgonParity(_) => 1,
                    };
                    if cases == 0 {
                        findings.push(format!(
                            "{module}: Applies for {label} with zero cases — a vacuous \
                             fixture proves nothing"
                        ));
                    }
                }
            }
        }
    }
    findings
}

/// T8 verdict from the two probe results: input of exactly `cap` bytes must be
/// accepted and `cap + 1` bytes rejected. `None` = pass.
pub fn eval_cap_probe(
    name: &str,
    cap: usize,
    rejected_at_cap: bool,
    rejected_over_cap: bool,
) -> Option<String> {
    if rejected_at_cap {
        return Some(format!(
            "{name}: input of exactly {cap} bytes (the declared cap) was REJECTED — \
             enforcement is off by one (too tight) or the declared cap is wrong"
        ));
    }
    if !rejected_over_cap {
        return Some(format!(
            "{name}: input of {} bytes (cap {cap} + 1) was ACCEPTED — the byte cap is \
             not enforced",
            cap + 1
        ));
    }
    None
}

/// T2 verdict: every declared [`ArgonParams`] must be pairwise equal. Zero or
/// one participant yields no findings (the caller notes a single participant).
pub fn argon_parity_findings(params: &[(&str, ArgonParams)]) -> Vec<String> {
    let Some((first_module, first)) = params.first() else {
        return Vec::new();
    };
    params
        .iter()
        .skip(1)
        .filter(|(_, p)| p != first)
        .map(|(module, p)| {
            format!(
                "argon parity: {module} uses {p:?} but {first_module} uses {first:?} — \
                 every argon2 hasher in the tree must share one parameter set"
            )
        })
        .collect()
}

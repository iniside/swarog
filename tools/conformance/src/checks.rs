//! The harness's pure logic: drift preflight diff, completeness matrix, and the
//! per-case evaluations that can be judged from plain data (probe results, argon
//! params). Everything here is deterministic and side-effect-free so `tests.rs`
//! can prove the failure modes — including the negative proof that a forgotten
//! module produces the expected per-entry drift error.

use std::collections::{BTreeMap, BTreeSet};

use crate::input_inventory::{render_key, InputKey};
use crate::model::{ArgonParams, Convention, Entry, Fixture, InputPolicy, Stance};

/// Core-infra modules hosted in every process that are NOT fortresses under
/// `modules/` — they appear in `checkmodules::monolith_modules()` but carry no
/// conformance entry. Mirrors CLAUDE.md rule 4: process infrastructure (metrics,
/// the planes, the DB, HTTP) is never declared as a domain capability, and the
/// fortress rule applies to `modules/` only. A named const, not a magic filter,
/// so adding a second core-infra module is an explicit, reviewable edit here.
pub const CORE_INFRA_MODULES: &[&str] = &["metrics"];

pub const CORE_HOSTED_MODULES: &[&str] = &["admin"];

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
    for m in entry_names
        .iter()
        .filter(|m| !disk.contains(*m) && !CORE_HOSTED_MODULES.contains(&m.as_str()))
    {
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
    for m in monolith
        .iter()
        .filter(|m| !disk.contains(**m) && !CORE_HOSTED_MODULES.contains(&m.as_str()))
    {
        findings.push(format!(
            "monolith module \"{m}\" has no modules/{m} directory on disk"
        ));
    }
    findings
}

/// The reviewed per-provider credential caps of `accounts.loginFederated`.
///
/// The wire field `credential` is ONE `InputKey`, so its `Validated` row can state only
/// one number — but the cap it names is `CredentialVerifier::max_credential_bytes`,
/// which every provider answers for itself. Before `guest` there was one implementor
/// and the row was accidentally exact; a second verifier with a shorter bound makes the
/// row true only for the widest provider, and nothing in the OIDC-shaped `CapCase` would
/// notice. This list is that missing notice: it is diffed against the registry accounts
/// actually builds before any assertion runs, so a new provider, a changed bound, or a
/// verifier the production construction stopped registering each names itself here.
/// Each row is `(provider, cap, the CapCase that executes that cap)`. The third column
/// is what keeps the link from being a coincidence: several unrelated accounts caps
/// happen to be 128 bytes, so matching by NUMBER alone would let a deleted guest fixture
/// stay green behind the session-token case. The mapping is one CapCase per PROVIDER,
/// never per number: epic and google share a bound today, and one shared case would
/// execute whichever verifier the case happened to hold and leave the other unprobed.
pub const CREDENTIAL_CAPS: &[(&str, usize, &str)] = &[
    ("epic", 65_536, "accounts federated epic credential"),
    ("google", 65_536, "accounts federated google credential"),
    ("guest", 128, "accounts federated guest ticket"),
];

/// Phase 1b (credentials) — the per-provider cap tripwire. `registry` is the cap map the
/// production `ProviderConfig::from_vars -> providers` path yields, `known` is accounts'
/// own `KNOWN_PROVIDERS`, and `entries` supplies accounts' executable cap fixtures. Four
/// independent drifts, each its own line with the concrete fix.
pub fn credential_cap_findings(
    registry: &BTreeMap<String, usize>,
    known: &[&str],
    entries: &[Entry],
) -> Vec<String> {
    let listed: BTreeMap<&str, (usize, &str)> = CREDENTIAL_CAPS
        .iter()
        .map(|(provider, cap, case)| (*provider, (*cap, *case)))
        .collect();
    let mut findings = Vec::new();

    for name in known {
        if !registry.contains_key(*name) {
            findings.push(format!(
                "accounts::providers::KNOWN_PROVIDERS names {name}, but the production registry \
                 built no verifier for it — either the fixture in \
                 accounts::conformance::credential_caps does not configure {name}, or \
                 ProviderConfig::providers never registers it. Neither leaves a checkable \
                 credential cap for {name}"
            ));
        }
    }
    for (name, cap) in registry {
        match listed.get(name.as_str()) {
            None => findings.push(format!(
                "accounts registers a {name} credential verifier with a {cap}-byte cap that \
                 checks::CREDENTIAL_CAPS does not list — add (\"{name}\", {cap}) and give \
                 accounts an input-byte-caps CapCase probing {name}'s own \
                 max_credential_bytes"
            )),
            Some((listed_cap, _)) if listed_cap != cap => findings.push(format!(
                "{name}'s credential cap is {cap} bytes in accounts but {listed_cap} in \
                 checks::CREDENTIAL_CAPS — the accounts.loginFederated credential policy row \
                 states the widest of these, so re-review the row before updating the list"
            )),
            Some(_) => {}
        }
    }
    for name in listed.keys() {
        if !registry.contains_key(*name) {
            findings.push(format!(
                "checks::CREDENTIAL_CAPS lists {name}, which the production registry no longer \
                 builds a verifier for — remove the stale entry"
            ));
        }
    }

    let probed: BTreeSet<(&str, usize)> = entries
        .iter()
        .find(|entry| entry.module == "accounts")
        .and_then(|entry| match entry.stance(Convention::InputByteCaps) {
            Some(Stance::Applies(Fixture::InputByteCaps(cases))) => Some(cases),
            _ => None,
        })
        .map(|cases| cases.iter().map(|case| (case.name, case.cap)).collect())
        .unwrap_or_default();
    for (name, cap) in registry {
        let Some((_, case)) = listed.get(name.as_str()) else {
            continue;
        };
        if !probed.contains(&(*case, *cap)) {
            findings.push(format!(
                "{name}'s {cap}-byte credential cap is stated but never executed — accounts \
                 declares no input-byte-caps CapCase named \"{case}\" with cap {cap}, so \
                 nothing proves credential_within_cap enforces {name}'s own bound"
            ));
        }
    }
    findings
}

/// Phase 1c — every input policy's prose must actually say something. A blank `basis` or
/// `rationale` is the same silence the completeness matrix already rejects for
/// `NotApplicable`'s `why`: these two verdicts are the ONLY thing standing between a
/// discovered request string and a gate that says nothing about it.
pub fn input_policy_prose_findings(policies: &[(InputKey, InputPolicy)]) -> Vec<String> {
    policies
        .iter()
        .filter_map(|(key, policy)| {
            let blank = match policy {
                InputPolicy::Validated { basis, .. } => basis.trim().is_empty().then_some("basis"),
                InputPolicy::Opaque { rationale } => {
                    rationale.trim().is_empty().then_some("rationale")
                }
                InputPolicy::KnownGap { .. } => None,
            }?;
            Some(format!(
                "{}: {blank} is empty — a reviewer-checkable sentence is required",
                render_key(key)
            ))
        })
        .collect()
}

/// The conformance entries whose control-plane manifest declares `writes: true`.
///
/// Every `cp.invoke` input row is `Opaque` on the grounds that each writer proves its own
/// caps, so this hand list is diffed against the sources before any assertion runs, and
/// every writer on it must carry an executable `cp.invoke` byte-cap case.
pub const CONTROL_PLANE_WRITERS: &[&str] = &[];

/// The `CapCase` name marker that ties a byte-cap case to `cp.invoke`.
pub const CONTROL_PLANE_CAP_MARKER: &str = "cp.invoke";

/// Phase 1b — the control-plane writer tripwire. `on_disk` is the scanned set of writer
/// entry names; every difference from [`CONTROL_PLANE_WRITERS`] is its own line, and every
/// listed writer must have an input-byte-caps `CapCase` naming [`CONTROL_PLANE_CAP_MARKER`].
pub fn control_plane_writer_findings(
    on_disk: &BTreeSet<String>,
    entries: &[Entry],
) -> Vec<String> {
    let listed: BTreeSet<String> = CONTROL_PLANE_WRITERS.iter().map(|m| m.to_string()).collect();
    let mut findings = Vec::new();
    for writer in on_disk.difference(&listed) {
        findings.push(format!(
            "{writer} declares a control-plane manifest with `writes: true` but is not in \
             checks::CONTROL_PLANE_WRITERS — add it and give its conformance entry an \
             input-byte-caps CapCase for {CONTROL_PLANE_CAP_MARKER}"
        ));
    }
    for writer in listed.difference(on_disk) {
        findings.push(format!(
            "checks::CONTROL_PLANE_WRITERS lists {writer}, whose sources no longer declare \
             `writes: true` — remove the stale entry"
        ));
    }
    for writer in &listed {
        let Some(entry) = entries.iter().find(|entry| entry.module == writer) else {
            findings.push(format!(
                "checks::CONTROL_PLANE_WRITERS lists {writer}, which has no conformance entry"
            ));
            continue;
        };
        let covered = match entry.stance(Convention::InputByteCaps) {
            Some(Stance::Applies(Fixture::InputByteCaps(cases))) => cases
                .iter()
                .any(|case| case.name.contains(CONTROL_PLANE_CAP_MARKER)),
            _ => false,
        };
        if !covered {
            findings.push(format!(
                "{writer} writes through {CONTROL_PLANE_CAP_MARKER} but declares no executable \
                 input-byte-caps CapCase naming {CONTROL_PLANE_CAP_MARKER}"
            ));
        }
    }
    findings
}

/// Declared coverage boundaries: what an EXECUTED convention's fixture provably cannot
/// reach, per (module, convention). A `Stance::KnownGap` is the wrong shape for this — it
/// replaces the fixture, so declaring one would trade fourteen executing cases for a label
/// — and an inline comment is invisible to everyone reading the matrix. The report prints
/// these beside the table, and [`coverage_boundary_findings`] keeps them from rotting.
pub const COVERAGE_BOUNDARIES: &[(&str, Convention, &str)] = &[(
    "mail",
    Convention::EnvValidation,
    "run_env_case sets exactly ONE variable per case, so the smtp arm's per-field parses — \
     the host shape, the port range, the TLS mode name, the missing-TLS bail and the \
     both-or-neither credential rule — cannot be reached here: each needs MAIL_PROVIDER=smtp \
     and a second MAIL_SMTP_* value set at the same time. The arm's entry branch \
     (MAIL_SMTP_HOST required under MAIL_PROVIDER=smtp) IS executed. The rest are reachable \
     only through MailConfig::from_vars, which takes the variables as data",
)];

/// Phase 1d — the boundary list's own self-check. A boundary must name a module the policy
/// carries, qualify a convention that actually EXECUTES there (a boundary on a
/// `NotApplicable` cell says nothing), and carry a reviewer-checkable sentence.
pub fn coverage_boundary_findings(entries: &[Entry]) -> Vec<String> {
    let mut findings = Vec::new();
    for (module, convention, why) in COVERAGE_BOUNDARIES {
        let label = conv_label(*convention);
        if why.trim().is_empty() {
            findings.push(format!(
                "coverage boundary {module} × {label}: empty reason — a reviewer-checkable \
                 sentence is required"
            ));
        }
        let Some(entry) = entries.iter().find(|entry| entry.module == *module) else {
            findings.push(format!(
                "coverage boundary names {module}, which has no conformance entry"
            ));
            continue;
        };
        if !matches!(entry.stance(*convention), Some(Stance::Applies(_))) {
            findings.push(format!(
                "coverage boundary {module} × {label} qualifies a convention {module} does \
                 not execute — a boundary describes what running cases cannot reach, so \
                 remove it or give {label} an Applies fixture"
            ));
        }
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

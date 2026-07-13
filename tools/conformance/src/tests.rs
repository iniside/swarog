//! Unit tests for the harness's pure logic (`checks.rs`) plus real-data guards
//! over the actual `entries()` list. Deliberately NO T6 executor test here:
//! `cargo test` runs threads in one binary, and env mutation across threads is
//! the race the standalone single-threaded binary exists to avoid.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::model::{
    ArgonParams, CapCase, Convention, Entry, EnvCase, Fixture, InputPolicy, Stance,
};

use crate::checks::{
    argon_parity_findings, completeness_findings, drift_findings, eval_cap_probe,
    CORE_INFRA_MODULES,
};

#[test]
fn default_allows_gaps_but_deny_gaps_fails() {
    assert!(!crate::deny_gaps_fails(false, 6));
    assert!(crate::deny_gaps_fails(true, 6));
    assert!(!crate::deny_gaps_fails(true, 0));
    assert!(!crate::parse_deny_gaps(Vec::<String>::new()).unwrap());
    assert!(crate::parse_deny_gaps(["--deny-gaps".to_owned()]).unwrap());
    assert!(crate::parse_deny_gaps(["--unknown".to_owned()]).is_err());
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

// ---- Phase 1: drift preflight ------------------------------------------------

#[test]
fn drift_clean_when_all_three_agree() {
    let s = set(&["accounts", "match"]);
    assert!(drift_findings(&s, &s, &s).is_empty());
}

/// The negative proof the plan demands: a synthetic entry set missing one
/// on-disk module produces the exact per-entry "add it" instruction — a
/// forgotten module fails red, it does not silently look like "not applicable".
#[test]
fn drift_on_disk_module_without_entry_fails_with_the_add_hint() {
    let disk = set(&["accounts", "foo"]);
    let entries = set(&["accounts"]);
    let monolith = set(&["accounts", "foo"]);
    let findings = drift_findings(&disk, &entries, &monolith);
    assert!(
        findings.iter().any(|f| f
            == "modules/foo on disk has no conformance entry — add foo::conformance::entry() \
                to tools/conformance policy"),
        "expected the per-entry add hint, got: {findings:?}"
    );
    // The monolith leg reports it too — per-entry, one line per mismatch.
    assert!(
        findings
            .iter()
            .any(|f| f.starts_with("monolith module \"foo\"")),
        "expected the monolith-leg line too, got: {findings:?}"
    );
}

#[test]
fn drift_ignores_sanctioned_core_infra_modules() {
    assert!(CORE_INFRA_MODULES.contains(&"metrics"));
    let disk = set(&["accounts"]);
    let entries = set(&["accounts"]);
    // metrics is in the monolith set but is process infrastructure, not a
    // fortress — the named exception must not drift.
    let monolith = set(&["accounts", "metrics"]);
    assert!(drift_findings(&disk, &entries, &monolith).is_empty());
}

#[test]
fn drift_stale_entry_and_unregistered_module_each_get_lines() {
    // "ghost" has an entry but no dir and no monolith registration; "bar" is on
    // disk but not registered in the monolith.
    let disk = set(&["accounts", "bar"]);
    let entries = set(&["accounts", "bar", "ghost"]);
    let monolith = set(&["accounts"]);
    let findings = drift_findings(&disk, &entries, &monolith);
    assert!(findings
        .iter()
        .any(|f| f.starts_with("conformance entry \"ghost\" has no modules/ghost directory")));
    assert!(findings
        .iter()
        .any(|f| f.contains("entry \"ghost\" is not in the monolith module set")));
    assert!(findings
        .iter()
        .any(|f| f.starts_with("modules/bar on disk is not in the monolith module set")));
}

// ---- Phase 2: completeness matrix ---------------------------------------------

fn na(why: &'static str) -> Stance {
    Stance::NotApplicable { why }
}

fn full_entry(module: &'static str) -> Entry {
    Entry {
        module,
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![EnvCase {
                    var: "X",
                    bad_value: "0",
                }])),
            ),
            (Convention::InputByteCaps, na("no player input")),
            (Convention::InfraOutage503, na("no verifier")),
            (Convention::ArgonParity, na("no password hashing")),
        ],
    }
}

#[test]
fn completeness_full_entry_is_clean() {
    assert!(completeness_findings(&[full_entry("m")]).is_empty());
}

#[test]
fn completeness_missing_stance_fails() {
    let mut e = full_entry("m");
    e.stances.retain(|(c, _)| *c != Convention::ArgonParity);
    let findings = completeness_findings(&[e]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("no stance declared for argon-parity"));
    assert!(findings[0].contains("silence is not a stance"));
}

#[test]
fn completeness_empty_why_fails() {
    let mut e = full_entry("m");
    e.stances[1] = (Convention::InputByteCaps, na("   "));
    let findings = completeness_findings(&[e]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("NotApplicable for input-byte-caps with an empty why"));
}

#[test]
fn completeness_known_gap_requires_why_and_remediation() {
    let mut entry = full_entry("m");
    entry.stances[1] = (
        Convention::InputByteCaps,
        Stance::KnownGap {
            why: "wire field is uncapped",
            remediation: "add the shared validator",
        },
    );
    assert!(completeness_findings(&[entry.clone()]).is_empty());

    entry.stances[1] = (
        Convention::InputByteCaps,
        Stance::KnownGap {
            why: "wire field is uncapped",
            remediation: "",
        },
    );
    let findings = completeness_findings(&[entry]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("requires non-empty why and remediation"));
}

#[test]
fn completeness_mismatched_fixture_variant_fails() {
    let mut e = full_entry("m");
    e.stances[1] = (
        Convention::InputByteCaps,
        Stance::Applies(Fixture::EnvValidation(vec![EnvCase {
            var: "X",
            bad_value: "0",
        }])),
    );
    let findings = completeness_findings(&[e]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("mismatched fixture variant"));
}

#[test]
fn completeness_zero_case_fixture_fails() {
    let mut e = full_entry("m");
    e.stances[0] = (
        Convention::EnvValidation,
        Stance::Applies(Fixture::EnvValidation(Vec::new())),
    );
    let findings = completeness_findings(&[e]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("zero cases"));
}

#[test]
fn completeness_duplicate_stance_fails() {
    let mut e = full_entry("m");
    e.stances.push((Convention::ArgonParity, na("again")));
    let findings = completeness_findings(&[e]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("argon-parity declared 2 times"));
}

// ---- Phase 3: pure case evaluations --------------------------------------------

#[test]
fn cap_probe_at_cap_accepted_over_cap_rejected_passes() {
    assert!(eval_cap_probe("email", 320, false, true).is_none());
}

#[test]
fn cap_probe_rejecting_at_the_cap_fails_as_off_by_one() {
    let f = eval_cap_probe("email", 320, true, true).expect("must fail");
    assert!(f.contains("exactly 320 bytes"));
    assert!(f.contains("REJECTED"));
}

#[test]
fn cap_probe_accepting_over_the_cap_fails_as_unenforced() {
    let f = eval_cap_probe("email", 320, false, false).expect("must fail");
    assert!(f.contains("321 bytes"));
    assert!(f.contains("not enforced"));
}

#[test]
fn argon_parity_equal_and_single_are_clean_mismatch_fails() {
    let a = ArgonParams {
        m_cost: 65536,
        t_cost: 3,
        p_cost: 2,
        output_len: 32,
    };
    let b = ArgonParams { t_cost: 4, ..a };
    assert!(argon_parity_findings(&[]).is_empty());
    assert!(argon_parity_findings(&[("accounts", a)]).is_empty());
    assert!(argon_parity_findings(&[("accounts", a), ("admin", a)]).is_empty());
    let findings = argon_parity_findings(&[("accounts", a), ("admin", b)]);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("admin"));
    assert!(findings[0].contains("accounts"));
}

// ---- Real-data guards (no env mutation, no runtime) -----------------------------

/// The real entries pass the completeness matrix — every fortress declares a
/// stance (with a matching, non-vacuous fixture or a non-empty why) for every
/// convention.
#[test]
fn real_entries_pass_the_completeness_matrix() {
    let findings = completeness_findings(&crate::policy::entries());
    assert!(findings.is_empty(), "completeness findings: {findings:?}");
}

#[test]
fn real_policy_has_no_known_input_cap_gaps() {
    let entries = crate::policy::entries();
    let gaps: Vec<(&str, Convention)> = entries
        .iter()
        .flat_map(|entry| {
            entry.stances.iter().filter_map(|(convention, stance)| {
                matches!(stance, Stance::KnownGap { .. }).then_some((entry.module, *convention))
            })
        })
        .collect();
    assert!(
        gaps.is_empty(),
        "known module-level input-cap gaps: {gaps:?}"
    );
}

/// The real hand list matches `modules/*` on disk and the monolith module set —
/// the same preflight the binary runs, provable under `cargo test` because
/// plain constructors need neither env flips nor a runtime.
#[test]
fn real_entries_match_disk_and_monolith() {
    let disk: BTreeSet<String> = crate::crate_dirs(&crate::modules_dir())
        .into_iter()
        .collect();
    assert!(
        !disk.is_empty(),
        "modules/ scan found nothing — harness path bug"
    );
    let entry_names: BTreeSet<String> = crate::policy::entries()
        .iter()
        .map(|e| e.module.to_string())
        .collect();
    let monolith = crate::monolith_module_names();
    let findings = drift_findings(&disk, &entry_names, &monolith);
    assert!(findings.is_empty(), "drift findings: {findings:?}");
}

#[test]
fn real_rpc_input_inventory_is_exactly_covered_and_matches_golden() {
    let discovered = crate::input_inventory::discover(&crate::input_inventory::api_root()).unwrap();
    assert_eq!(
        discovered.len(),
        18,
        "unexpected request string inventory: {discovered:?}"
    );
    let policies = crate::policy::input_policies();
    let policy_keys = policies
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let findings = crate::input_inventory::policy_key_findings(&discovered, &policy_keys);
    assert!(findings.is_empty(), "input policy findings: {findings:?}");

    let actual = crate::input_inventory::render_golden(&discovered);
    let committed = std::fs::read_to_string(crate::input_inventory::golden_path()).unwrap();
    assert!(
        crate::input_inventory::golden_findings(&actual, &committed).is_empty(),
        "committed input golden is stale\nactual:\n{actual}"
    );
}

#[test]
fn real_input_policy_has_no_known_field_gaps() {
    let gaps = crate::policy::input_policies()
        .into_iter()
        .filter_map(|(key, policy)| {
            matches!(policy, InputPolicy::KnownGap { .. })
                .then_some(crate::input_inventory::render_key(&key))
        })
        .collect::<Vec<_>>();
    assert!(
        gaps.is_empty(),
        "known field-level input-cap gaps: {gaps:?}"
    );
}

/// CapCase probes stay callable as plain data — a smoke check that the fixture
/// plumbing (`Arc<dyn Fn>`) composes the way the executor uses it.
#[test]
fn cap_case_probe_plumbing_smoke() {
    let case = CapCase {
        name: "smoke",
        cap: 8,
        probe: Arc::new(|len| len > 8),
    };
    assert!(!(case.probe)(case.cap));
    assert!((case.probe)(case.cap + 1));
}

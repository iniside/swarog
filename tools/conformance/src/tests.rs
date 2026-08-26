//! Unit tests for the harness's pure logic (`checks.rs`) plus real-data guards
//! over the actual `entries()` list. Deliberately NO T6 executor test here:
//! `cargo test` runs threads in one binary, and env mutation across threads is
//! the race the standalone single-threaded binary exists to avoid.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::model::{
    ArgonParams, CapCase, Convention, Entry, EnvCase, Fixture, InputPolicy, Stance,
};

use crate::checks::{
    admin_submit_findings, argon_parity_findings, completeness_findings, credential_cap_findings,
    drift_findings, eval_cap_probe, input_policy_prose_findings, ADMIN_SUBMIT_MODULES,
    CORE_INFRA_MODULES,
};
use crate::input_inventory::{Exposure, InputKey};

#[test]
fn default_allows_gaps_but_deny_gaps_fails() {
    assert!(!crate::deny_gaps_fails(false, 6));
    assert!(crate::deny_gaps_fails(true, 6));
    assert!(!crate::deny_gaps_fails(true, 0));
    assert_eq!(
        crate::parse_args(Vec::<String>::new()).unwrap(),
        crate::Mode::Check { deny_gaps: false }
    );
    assert_eq!(
        crate::parse_args(["--deny-gaps".to_owned()]).unwrap(),
        crate::Mode::Check { deny_gaps: true }
    );
    assert!(crate::parse_args(["--unknown".to_owned()]).is_err());
}

/// The golden has exactly one writer, and `--help` names it — the discoverable
/// route to regenerating the snapshot must not be "read `render_golden`".
#[test]
fn write_input_golden_is_a_documented_mode_taking_a_path() {
    assert_eq!(
        crate::parse_args(["--write-input-golden".to_owned(), "out.tsv".to_owned()]).unwrap(),
        crate::Mode::WriteInputGolden("out.tsv".into())
    );
    assert!(crate::parse_args(["--write-input-golden".to_owned()]).is_err());
    assert!(crate::parse_args([
        "--write-input-golden".to_owned(),
        "out.tsv".to_owned(),
        "--deny-gaps".to_owned()
    ])
    .is_err());
    assert_eq!(crate::parse_args(["-h".to_owned()]).unwrap(), crate::Mode::Help);
    assert!(crate::USAGE.contains("--write-input-golden"));
    assert!(crate::USAGE.contains("--bless-input-golden"));
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

// ---- Phase 1b: the adminSubmit drift tripwire ---------------------------------

fn na(why: &'static str) -> Stance {
    Stance::NotApplicable { why }
}

fn caps_entry(module: &'static str, cases: usize) -> Entry {
    let mut entry = full_entry(module);
    entry.stances[1] = (
        Convention::InputByteCaps,
        Stance::Applies(Fixture::InputByteCaps(
            (0..cases)
                .map(|_| CapCase {
                    name: "case",
                    cap: 8,
                    probe: Arc::new(|len| len > 8),
                })
                .collect(),
        )),
    );
    entry
}

fn listed_entries(cases: usize) -> Vec<Entry> {
    ADMIN_SUBMIT_MODULES
        .iter()
        .map(|module| caps_entry(module, cases))
        .collect()
}

#[test]
fn admin_submit_list_matching_disk_with_cap_cases_is_clean() {
    let disk = set(ADMIN_SUBMIT_MODULES);
    assert!(admin_submit_findings(&disk, &listed_entries(1)).is_empty());
}

/// The branch the tripwire exists for: a THIRD module implements `AdminSubmit`. It adds no
/// `InputKey`, leaves the golden unchanged, and could declare `InputByteCaps` NotApplicable
/// with a plausible reason — so without this the whole `params.<value>` verdict silently
/// stops covering it.
#[test]
fn admin_submit_unlisted_implementor_fails_with_the_add_hint() {
    let mut disk = set(ADMIN_SUBMIT_MODULES);
    disk.insert("newthing".to_owned());
    let findings = admin_submit_findings(&disk, &listed_entries(1));
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("modules/newthing"), "{findings:?}");
    assert!(findings[0].contains("ADMIN_SUBMIT_MODULES"), "{findings:?}");
    assert!(findings[0].contains("CapCase"), "{findings:?}");
}

#[test]
fn admin_submit_stale_listing_fails() {
    let mut disk = set(ADMIN_SUBMIT_MODULES);
    let dropped = disk.iter().next().cloned().expect("non-empty list");
    disk.remove(&dropped);
    let findings = admin_submit_findings(&disk, &listed_entries(1));
    assert!(
        findings
            .iter()
            .any(|f| f.contains(&dropped) && f.contains("remove the stale entry")),
        "{findings:?}"
    );
}

/// A listed implementor whose input-byte-caps stance is a SENTENCE, not an executable
/// fixture — the exact shape a new module could use to pass while leaving its form values
/// uncapped.
#[test]
fn admin_submit_listed_module_without_an_executable_fixture_fails() {
    let disk = set(ADMIN_SUBMIT_MODULES);
    let entries: Vec<Entry> = ADMIN_SUBMIT_MODULES
        .iter()
        .map(|module| {
            let mut entry = caps_entry(module, 1);
            entry.stances[1] = (Convention::InputByteCaps, na("looks fine to me"));
            entry
        })
        .collect();
    let findings = admin_submit_findings(&disk, &entries);
    assert_eq!(findings.len(), ADMIN_SUBMIT_MODULES.len(), "{findings:?}");
    assert!(
        findings[0].contains("no executable input-byte-caps fixture"),
        "{findings:?}"
    );
    // A zero-case fixture is the same silence wearing an Applies costume.
    assert!(!admin_submit_findings(&disk, &listed_entries(0)).is_empty());
}

/// The scanning half, on a synthetic tree: it must see an impl in ANY source file (not
/// just lib.rs), accept the `use`-imported spelling, and ignore both a test-file fixture
/// and a commented-out line. A scanner that quietly returns nothing makes the whole
/// tripwire vacuous.
#[test]
fn admin_submit_scan_finds_an_impl_in_any_source_but_not_a_test_file() {
    let root = std::env::temp_dir().join(format!(
        "conformance-adminsubmit-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (module, file, body) in [
        (
            "alpha",
            "admin.rs",
            "#[async_trait]\nimpl adminapi::AdminSubmit for Service {}\n",
        ),
        ("beta", "lib.rs", "impl AdminSubmit for Service {}\n"),
        (
            "gamma",
            "tests.rs",
            "impl adminapi::AdminSubmit for Fixture {}\n",
        ),
        (
            "delta",
            "lib.rs",
            "// impl adminapi::AdminSubmit for Service {}\n",
        ),
    ] {
        let src = root.join(module).join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(root.join(module).join("Cargo.toml"), "").unwrap();
        std::fs::write(src.join(file), body).unwrap();
    }
    // gamma's only source is a test file, so it needs a real one to be a crate at all.
    std::fs::write(root.join("gamma/src/lib.rs"), "pub struct Fixture;\n").unwrap();

    let found = crate::admin_submit_impl_modules(&root).expect("scan");
    assert_eq!(found, set(&["alpha", "beta"]), "{found:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The hand list matches `modules/*/src` and every listed module backs the shared
/// `admin.adminSubmit params.<value>` verdict with a real probe — the same preflight the
/// binary runs, provable under `cargo test`.
#[test]
fn real_admin_submit_list_matches_disk_and_carries_cap_cases() {
    let on_disk = crate::admin_submit_impl_modules(&crate::modules_dir())
        .expect("scan modules/ for AdminSubmit impls");
    assert!(
        !on_disk.is_empty(),
        "modules/ scan found no AdminSubmit impl — harness path bug"
    );
    let findings = admin_submit_findings(&on_disk, &crate::policy::entries());
    assert!(findings.is_empty(), "adminSubmit findings: {findings:?}");
}

// ---- Phase 1b (credentials): the per-provider cap tripwire --------------------
//
// `credential_cap_findings` reads the production `checks::CREDENTIAL_CAPS` table
// directly (it is not a parameter), so every fixture here is built to align with —
// or deliberately diverge from — that fixed table rather than a synthetic one.

fn cap_case(name: &'static str, cap: usize) -> CapCase {
    CapCase { name, cap, probe: Arc::new(move |len| len > cap) }
}

/// The registry `checks::CREDENTIAL_CAPS` states today: `epic`/`google` at 65536,
/// `guest` at 128.
fn clean_registry() -> BTreeMap<String, usize> {
    [("epic", 65_536), ("google", 65_536), ("guest", 128)]
        .into_iter()
        .map(|(name, cap)| (name.to_string(), cap))
        .collect()
}

const CLEAN_KNOWN: &[&str] = &["epic", "google", "guest"];

/// The accounts entry whose `InputByteCaps` cases execute every provider's cap under
/// the exact case NAME `checks::CREDENTIAL_CAPS` links it to.
fn clean_accounts_entry() -> Entry {
    Entry {
        module: "accounts",
        stances: vec![(
            Convention::InputByteCaps,
            Stance::Applies(Fixture::InputByteCaps(vec![
                cap_case("accounts federated epic credential", 65_536),
                cap_case("accounts federated google credential", 65_536),
                cap_case("accounts federated guest ticket", 128),
            ])),
        )],
    }
}

/// registry == table == probed → clean.
#[test]
fn credential_cap_findings_all_three_sources_agree_is_clean() {
    let findings =
        credential_cap_findings(&clean_registry(), CLEAN_KNOWN, &[clean_accounts_entry()]);
    assert!(findings.is_empty(), "{findings:?}");
}

/// The registry's own cap disagrees with the reviewed number in
/// `checks::CREDENTIAL_CAPS` — a re-review finding naming both numbers.
#[test]
fn credential_cap_findings_registry_cap_mismatch_names_both_numbers() {
    let mut registry = clean_registry();
    registry.insert("guest".to_string(), 999);
    let findings = credential_cap_findings(&registry, CLEAN_KNOWN, &[clean_accounts_entry()]);
    assert!(
        findings.iter().any(|f| f.contains("999") && f.contains("128") && f.contains("guest")),
        "{findings:?}"
    );
}

/// The production registry built a verifier `checks::CREDENTIAL_CAPS` does not
/// list at all — the "add (...)" hint.
#[test]
fn credential_cap_findings_unlisted_registry_provider_gets_the_add_hint() {
    let mut registry = clean_registry();
    registry.insert("facebook".to_string(), 4096);
    let known: Vec<&str> = CLEAN_KNOWN.iter().copied().chain(["facebook"]).collect();
    let findings = credential_cap_findings(&registry, &known, &[clean_accounts_entry()]);
    assert!(
        findings.iter().any(|f| f.contains("facebook")
            && f.contains("4096")
            && f.contains("add (\"facebook\", 4096)")),
        "{findings:?}"
    );
}

/// `checks::CREDENTIAL_CAPS` names a provider the registry no longer builds —
/// the "remove the stale entry" finding, isolated by dropping `guest` from both the
/// registry AND `known` (so the missing-verifier finding does not also fire).
#[test]
fn credential_cap_findings_stale_table_entry_gets_the_remove_hint() {
    let mut registry = clean_registry();
    registry.remove("guest");
    let known: Vec<&str> = CLEAN_KNOWN.iter().copied().filter(|n| *n != "guest").collect();
    let entries = vec![Entry {
        module: "accounts",
        stances: vec![(
            Convention::InputByteCaps,
            Stance::Applies(Fixture::InputByteCaps(vec![
                cap_case("accounts federated epic credential", 65_536),
                cap_case("accounts federated google credential", 65_536),
            ])),
        )],
    }];
    let findings = credential_cap_findings(&registry, &known, &entries);
    assert!(
        findings
            .iter()
            .any(|f| f.contains("guest") && f.contains("remove the stale entry")),
        "{findings:?}"
    );
}

/// A `KNOWN_PROVIDERS` name the production registry never built a verifier for —
/// the two-cause finding (fixture didn't configure it, or `providers()` never
/// registers it).
#[test]
fn credential_cap_findings_known_but_unbuilt_provider_gets_the_two_cause_finding() {
    let known: Vec<&str> = CLEAN_KNOWN.iter().copied().chain(["apple"]).collect();
    let findings = credential_cap_findings(&clean_registry(), &known, &[clean_accounts_entry()]);
    assert!(
        findings.iter().any(|f| f.contains("apple")
            && f.contains("does not configure apple")
            && f.contains("never registers it")),
        "{findings:?}"
    );
}

/// THE regression this check exists to catch: matching a `CapCase` by cap NUMBER
/// alone let a deleted guest fixture stay green behind an unrelated 128-byte case
/// (`accounts session token`). Here `probed` carries a DIFFERENT case with the SAME
/// cap and NO case named for guest — the finding must still fire. A number-only
/// match would find `("accounts session token", 128)` and wrongly call guest's cap
/// executed.
#[test]
fn credential_cap_findings_same_cap_different_name_does_not_satisfy_guest() {
    let entries = vec![Entry {
        module: "accounts",
        stances: vec![(
            Convention::InputByteCaps,
            Stance::Applies(Fixture::InputByteCaps(vec![
                cap_case("accounts federated epic credential", 65_536),
                cap_case("accounts federated google credential", 65_536),
                cap_case("accounts session token", 128),
            ])),
        )],
    }];
    let findings = credential_cap_findings(&clean_registry(), CLEAN_KNOWN, &entries);
    assert!(
        findings.iter().any(|f| f.contains("guest")
            && f.contains("stated but never executed")
            && f.contains("accounts federated guest ticket")),
        "a name-blind match would wrongly treat guest's cap as executed: {findings:?}"
    );
}

/// No accounts entry at all (also covers an `InputByteCaps` `NotApplicable`
/// stance): `probed` must fall back to empty via `unwrap_or_default()`, not panic or
/// silently skip the "stated but never executed" findings — every registered cap
/// with a table entry gets one.
#[test]
fn credential_cap_findings_missing_accounts_entry_falls_back_to_empty_probed() {
    let findings = credential_cap_findings(&clean_registry(), CLEAN_KNOWN, &[]);
    for provider in ["epic", "google", "guest"] {
        assert!(
            findings
                .iter()
                .any(|f| f.contains(provider) && f.contains("stated but never executed")),
            "missing accounts entry must not silently swallow {provider}'s finding: {findings:?}"
        );
    }

    let na_entry = Entry {
        module: "accounts",
        stances: vec![(Convention::InputByteCaps, na("no player input"))],
    };
    let findings_na = credential_cap_findings(&clean_registry(), CLEAN_KNOWN, &[na_entry]);
    assert_eq!(findings, findings_na, "NotApplicable must behave the same as no entry");
}

/// The real table against the real production registry and the real accounts entry —
/// the same preflight the binary runs, provable under `cargo test`.
#[test]
fn real_credential_cap_list_matches_registry_and_execution() {
    let findings = credential_cap_findings(
        &accounts::conformance::credential_caps(),
        accounts::conformance::KNOWN_PROVIDERS,
        &crate::policy::entries(),
    );
    assert!(findings.is_empty(), "credential cap findings: {findings:?}");
}

// ---- Phase 1c: input-policy prose ----------------------------------------------

#[test]
fn blank_basis_or_rationale_is_a_finding() {
    let key = |field: &str| InputKey {
        wire_method: "demo.send".into(),
        wire_field_name: field.into(),
        exposure: Exposure::External,
    };
    let policies = vec![
        (
            key("a"),
            InputPolicy::Validated {
                cap: 8,
                basis: "   ",
            },
        ),
        (key("b"), InputPolicy::Opaque { rationale: "" }),
        (
            key("c"),
            InputPolicy::Validated {
                cap: 8,
                basis: "a real sentence",
            },
        ),
    ];
    let findings = input_policy_prose_findings(&policies);
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert!(findings[0].contains("basis is empty"), "{findings:?}");
    assert!(findings[1].contains("rationale is empty"), "{findings:?}");
}

#[test]
fn real_input_policies_all_carry_prose() {
    let findings = input_policy_prose_findings(&crate::policy::input_policies());
    assert!(findings.is_empty(), "input policy prose: {findings:?}");
}

// ---- Phase 2: completeness matrix ---------------------------------------------

fn full_entry(module: &'static str) -> Entry {
    Entry {
        module,
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![EnvCase {
                    var: "X",
                    bad_value: "0",
                    expect: "X",
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
            expect: "X",
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

/// The field-level input-cap gaps are pinned to the exact reported set — empty, since
/// every discovered RPC request string now carries a Validated or Opaque stance. A NEW
/// gap still fails here, and `--deny-gaps` (the blocking conformance stage) rejects every
/// entry in this list: it is a stop-the-line record, not a sanctioned exemption.
#[test]
fn real_input_policy_gaps_are_exactly_the_reported_set() {
    let gaps = crate::policy::input_policies()
        .into_iter()
        .filter_map(|(key, policy)| {
            matches!(policy, InputPolicy::KnownGap { .. })
                .then_some(crate::input_inventory::render_key(&key))
        })
        .collect::<Vec<_>>();
    let expected: [String; 0] = [];
    assert_eq!(gaps, expected);
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

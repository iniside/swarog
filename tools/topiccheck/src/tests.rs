use super::*;
use std::path::Path;

/// Helper: a defined contract from a topic + version (history irrelevant to most diffs).
fn contract(topic: &str, version: u32) -> Contract {
    Contract {
        topic: topic.to_string(),
        version,
        history: HistoryPolicy::MinRetention { days: 7 },
    }
}

fn defs(pairs: &[(&str, u32)]) -> Vec<Contract> {
    pairs.iter().map(|(t, v)| contract(t, *v)).collect()
}

fn sub(id: &str, topic: &str, version: u32, process: &'static str) -> Sub {
    Sub {
        id: id.to_string(),
        topic: topic.to_string(),
        version,
        history: None,
        process,
    }
}

fn topics(ts: &[&str]) -> BTreeSet<String> {
    ts.iter().map(|t| t.to_string()).collect()
}

fn contracts(pairs: &[(&str, u32)]) -> BTreeSet<(String, u32)> {
    pairs.iter().map(|(t, v)| (t.to_string(), *v)).collect()
}

// --- Check 1: version match --------------------------------------------------

#[test]
fn matching_topic_and_version_is_clean() {
    let d = defs(&[("a", 1), ("b", 2)]);
    let s = vec![sub("x.a.v1", "a", 1, "p"), sub("x.b.v2", "b", 2, "p")];
    assert!(version_findings(&d, &s).is_empty());
}

#[test]
fn undefined_topic_is_flagged() {
    let d = defs(&[("a", 1)]);
    let s = vec![sub("x.z.v1", "z", 1, "p")];
    let v = version_findings(&d, &s);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("UNDEFINED"), "{v:?}");
}

#[test]
fn version_mismatch_is_flagged() {
    let d = defs(&[("a", 2)]);
    let s = vec![sub("x.a.v1", "a", 1, "p")];
    let v = version_findings(&d, &s);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("v1") && v[0].contains("v2"), "{v:?}");
    assert!(!v[0].contains("UNDEFINED"), "{v:?}");
}

#[test]
fn coexisting_versions_of_one_topic_are_clean() {
    // The documented v1+v2 coexistence model: one topic defined at two versions,
    // each with its own subscriber. A topic-only key would collide the contracts
    // and misreport; the (topic, version) key sees both as distinct and clean.
    let d = defs(&[("t", 1), ("t", 2)]);
    let s = vec![sub("c.t.v1", "t", 1, "p"), sub("c.t.v2", "t", 2, "p")];
    let v = version_findings(&d, &s);
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn subscribing_a_version_a_topic_does_not_define_is_drift_not_undefined() {
    // A v2 sub against a v1-only contract: the topic IS defined, just not at v2,
    // so this is a version-drift finding, distinct from an UNDEFINED topic.
    let d = defs(&[("t", 1)]);
    let s = vec![sub("c.t.v2", "t", 2, "p")];
    let v = version_findings(&d, &s);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("v2") && v[0].contains("v1"), "{v:?}");
    assert!(!v[0].contains("UNDEFINED"), "{v:?}");
}

#[test]
fn carried_history_mismatch_is_flagged() {
    let d = defs(&[("a", 1)]); // contract history = MinRetention{7}
    let mut s = sub("x.a.v1", "a", 1, "p");
    s.history = Some(HistoryPolicy::KeepForever);
    let v = version_findings(&d, &[s]);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("history"), "{v:?}");
}

// --- Checks 2+3: single host -------------------------------------------------

#[test]
fn one_process_per_id_is_clean() {
    let s = vec![sub("audit.a.v1", "a", 1, "audit-svc"), sub("audit.b.v1", "b", 1, "audit-svc")];
    assert!(host_findings(&s).is_empty());
}

#[test]
fn cross_process_duplicate_host_is_flagged() {
    // Same subscription id wired into two different processes.
    let s = vec![sub("x.a.v1", "a", 1, "p1"), sub("x.a.v1", "a", 1, "p2")];
    let v = host_findings(&s);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("p1") && v[0].contains("p2"), "{v:?}");
}

// --- Check 3: planeless processes host nothing -------------------------------

#[test]
fn planeless_process_hosting_a_sub_is_flagged() {
    let s = vec![sub("x.a.v1", "a", 1, "gateway-svc")];
    let v = planeless_findings(&s, &["gateway-svc"]);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("gateway-svc"), "{v:?}");
}

#[test]
fn plane_hosting_process_is_clean_for_planeless_check() {
    let s = vec![sub("x.a.v1", "a", 1, "audit-svc")];
    assert!(planeless_findings(&s, &["gateway-svc"]).is_empty());
}

// --- Check 4: durability -----------------------------------------------------

#[test]
fn inprocess_subscribed_defined_topic_is_flagged() {
    let d = defs(&[("a", 1), ("b", 1)]);
    let inproc = topics(&["a"]); // "a" has an in-process sub; "b" is durable-only
    assert_eq!(inprocess_defined(&d, &inproc, &[]), vec!["a".to_string()]);
}

#[test]
fn allowlist_suppresses_a_durability_finding() {
    let d = defs(&[("a", 1)]);
    let inproc = topics(&["a"]);
    assert!(inprocess_defined(&d, &inproc, &["a"]).is_empty());
}

#[test]
fn no_inprocess_subscriber_is_clean() {
    let d = defs(&[("a", 1)]);
    assert!(inprocess_defined(&d, &topics(&[]), &[]).is_empty());
}

#[test]
fn inprocess_contracts_pass_flags_matching_topic_version() {
    // The tuple-aware pass over `on()` registrations: only the exact defined
    // (topic, version) subscribed in-process is a violation. A different version
    // of the same topic in-process is NOT this contract's problem.
    let d = defs(&[("a", 2)]);
    assert_eq!(
        inprocess_defined_contracts(&d, &contracts(&[("a", 2)]), &[]),
        vec!["a".to_string()]
    );
    assert!(inprocess_defined_contracts(&d, &contracts(&[("a", 1)]), &[]).is_empty());
    assert!(inprocess_defined_contracts(&d, &contracts(&[("a", 2)]), &["a"]).is_empty());
}

// --- Check 5: unsubscribed (advisory) ----------------------------------------

#[test]
fn all_subscribed_yields_no_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert!(unsubscribed(&d, &contracts(&[("a", 1), ("b", 1), ("extra", 1)]), &[]).is_empty());
}

#[test]
fn missing_subscriber_is_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert_eq!(unsubscribed(&d, &contracts(&[("a", 1)]), &[]), vec!["b".to_string()]);
}

#[test]
fn unsubscribed_is_version_specific() {
    // A topic subscribed only at v1 leaves its v2 contract unsubscribed.
    let d = defs(&[("t", 1), ("t", 2)]);
    assert_eq!(unsubscribed(&d, &contracts(&[("t", 1)]), &[]), vec!["t".to_string()]);
}

#[test]
fn allowlist_suppresses_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert!(unsubscribed(&d, &contracts(&[("a", 1)]), &["b"]).is_empty());
}

/// Integration-shaped: drive the REAL wiring (`observe`) for both deployment
/// profiles and assert the current tree has ZERO unsubscribed defined topics —
/// i.e. `ALLOW_UNSUBSCRIBED` is legitimately empty because every defined contract
/// has a live durable subscriber in both Monolith and Split. Now that
/// unsubscribed folds into `any_seam`, this is the assertion that
/// `--durability-strict` (the fortress gate) exits 0 on this tree. Mirrors
/// `main`'s harness setup: no auth env (Admin::init no longer reads any), and a
/// tokio runtime for the in-process `Bus::on` spawns during `init`.
#[tokio::test]
async fn current_tree_has_zero_unsubscribed_in_both_profiles() {
    let defined = defined_topics();
    for (label, profile) in [
        ("Monolith", DeploymentProfile::Monolith),
        ("Split", DeploymentProfile::Split),
    ] {
        let obs = observe(&profile).expect("observe should build every process with no I/O");
        let subscribed: BTreeSet<(String, u32)> =
            obs.subs.iter().map(|s| (s.topic.clone(), s.version)).collect();
        let unsub = unsubscribed(&defined, &subscribed, ALLOW_UNSUBSCRIBED);
        assert!(unsub.is_empty(), "profile {label}: unexpected unsubscribed topics {unsub:?}");
    }
}

// --- Define-site self-check: defined_topics() matches every `bus::define(` call ----

/// Step 6b: `defined_topics()` (main.rs) is a hand-maintained list of statics -- this
/// mirrors `checkmodules::split_fleet_matches_cmd_dirs`'s pattern (drift tripwire
/// against the filesystem) so a NEW `api/<domain>/events/src/lib.rs` define site that
/// nobody added to `defined_topics()` fails loudly here instead of silently never being
/// checked by any profile. Comment-filtered text scan (skip lines whose trimmed content
/// starts with `//`), same tolerance level as archcheck's text tripwires -- this is not
/// a Rust parser, just a drift detector for the `(topic, version)` pair that follows each
/// `define(` call.
///
/// Keyed on `(topic, version)`, NOT topic alone: a legal ADDITIVE v2 `define(...)` site
/// living beside its v1 (CLAUDE.md hard constraint 6 -- evolve events additively, never
/// mutate a published shape) is two distinct, valid define-sites for the same topic
/// string. A topic-only key would treat the v2 site as a duplicate of v1 and panic on
/// every additive event evolution -- exactly the dead-gate failure mode this scan must
/// not repeat.
///
/// Tolerance (deliberate, no lexer): this is a text scan, not Rust parsing. A `define(`
/// occurrence inside a string literal or a block comment WILL be picked up as a phantom
/// site. That is acceptable because the failure mode is loud, never silent: a phantom
/// site either fails version parsing (panic here) or surfaces as a drift-assert mismatch
/// against the compiled `EventContract` values in `defined_topics()` -- it can never make
/// the gate pass something it should have failed.
fn parse_define_sites(file_label: &str, text: &str) -> BTreeSet<(String, u32)> {
    let mut sites: BTreeSet<(String, u32)> = BTreeSet::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        // Scan EVERY `define(` occurrence on the line, not only the first -- two defines
        // packed onto one line must both be recorded, or a real site would be silently
        // skipped.
        let mut remaining = t;
        while let Some((_, rest)) = remaining.split_once("define(") {
            remaining = rest;
            let Some(start) = rest.find('"') else {
                panic!(
                    "{file_label}: a `define(` call has no string-literal first argument on \
                     the same line -- the scan assumes `define(\"topic\", <version>, ...)` \
                     on one line: {line:?}"
                );
            };
            let after_quote = &rest[start + 1..];
            let end = after_quote.find('"').unwrap_or_else(|| {
                panic!(
                    "{file_label}: unterminated string literal after `define(` in line {line:?}"
                )
            });
            let topic = &after_quote[..end];

            // Parse the version argument: skip whitespace, expect exactly one comma, skip
            // whitespace, then take a run of ASCII digits as the u32 literal. A version
            // token that is NOT a plain decimal integer literal -- a const name, a line
            // wrap that puts the version on the next line, nothing at all, or a digit run
            // glued to more token characters (`1_0`, `0x10`) -- MUST panic here, never
            // silently skip or truncate the site. A silent skip is the same dead-gate
            // class Step 1 of this remediation just closed (a check that looks like it
            // validates something but quietly excludes the case that would fail it); this
            // scan does not get to reintroduce that pattern for the version field.
            let after_topic = &after_quote[end + 1..];
            let after_comma = after_topic.trim_start().strip_prefix(',').unwrap_or_else(|| {
                panic!(
                    "{file_label}: `define(\"{topic}\", ...)` is not followed by a `,` and a \
                     version literal on the same line -- cannot parse the version: {line:?}"
                )
            });
            let version_field = after_comma.trim_start();
            let digits: String =
                version_field.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                panic!(
                    "{file_label}: `define(\"{topic}\", ...)` version argument is not an \
                     integer literal on the same line (found {version_field:?}) -- a const \
                     name or a version wrapped onto the next line cannot be scanned by this \
                     text-based drift check; keep `define(\"topic\", <u32 literal>, ...)` on \
                     one line: {line:?}"
                );
            }
            // Boundary check: the digit run must END the token. `take_while` alone would
            // silently TRUNCATE `1_0` to 1 or `0x10` to 0 -- a wrong version recorded
            // without a peep. The compiled-contract drift assert would still catch the
            // mismatch loudly downstream, but this scan's own contract is loud failure at
            // the site, same as the non-literal case.
            if let Some(next) = version_field[digits.len()..].chars().next() {
                if next == '_' || next.is_ascii_alphanumeric() {
                    panic!(
                        "{file_label}: `define(\"{topic}\", ...)` version token is not a \
                         plain decimal integer literal (found {version_field:?}) -- \
                         separators (`1_0`) and radix prefixes (`0x10`) are rejected, not \
                         truncated: {line:?}"
                    );
                }
            }
            let version: u32 = digits.parse().unwrap_or_else(|e| {
                panic!("{file_label}: version literal {digits:?} failed to parse as u32: {e}")
            });

            if !sites.insert((topic.to_string(), version)) {
                panic!(
                    "{file_label}: (topic, version) = ({topic:?}, {version}) is defined more \
                     than once -- topiccheck assumes one define per (topic, version) pair"
                );
            }
        }
    }
    sites
}

/// Union of define-sites across files, panicking on a `(topic, version)` pair defined in
/// two different files -- the cross-file half of the duplicate-define tripwire, extracted
/// from the #[test] body so the panic path is directly testable. Names BOTH files.
fn union_define_sites<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeSet<(String, u32)> {
    let mut origin: std::collections::BTreeMap<(String, u32), String> =
        std::collections::BTreeMap::new();
    for (label, text) in files {
        for site in parse_define_sites(label, text) {
            if let Some(first) = origin.get(&site) {
                panic!(
                    "(topic, version) = {site:?} is defined in both {first} and {label} \
                     (duplicate define-site across files) -- topiccheck assumes one define \
                     per (topic, version) pair"
                );
            }
            origin.insert(site, label.to_string());
        }
    }
    origin.into_keys().collect()
}

#[test]
fn defined_topics_matches_every_define_site_on_disk() {
    let api_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api");
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(&api_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", api_dir.display()))
    {
        let entry = entry.expect("readable dir entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        let lib = entry.path().join("events").join("src").join("lib.rs");
        let Ok(text) = std::fs::read_to_string(&lib) else {
            continue; // domain has no events crate -- nothing to scan
        };
        files.push((lib.display().to_string(), text));
    }
    assert!(!files.is_empty(), "expected at least one api/*/events/src/lib.rs to scan");
    let from_fs = union_define_sites(files.iter().map(|(l, t)| (l.as_str(), t.as_str())));

    let from_defined: BTreeSet<(String, u32)> =
        defined_topics().into_iter().map(|c| (c.topic, c.version)).collect();

    assert_eq!(
        from_fs, from_defined,
        "tools/topiccheck::defined_topics() has drifted from the real `bus::define(` call \
         sites under api/*/events/src/lib.rs (filesystem scan found {from_fs:?}, \
         defined_topics() returns {from_defined:?}) -- add/remove the missing/orphaned \
         Contract in defined_topics() (tools/topiccheck/src/main.rs)"
    );
}

// --- parse_define_sites: direct unit coverage of the extracted scan helper --------

#[test]
fn parse_define_sites_treats_additive_v2_beside_v1_as_two_clean_sites() {
    // The point of this whole fix: a v1 define beside a legal additive v2 define for the
    // SAME topic must NOT panic. Under the old topic-only dedup this fixture panicked.
    let text = "define(\"x\", 1, HistoryPolicy::KeepForever);\ndefine(\"x\", 2, HistoryPolicy::KeepForever);\n";
    let sites = parse_define_sites("fixture", text);
    assert_eq!(
        sites,
        BTreeSet::from([("x".to_string(), 1), ("x".to_string(), 2)])
    );
}

#[test]
#[should_panic(expected = "defined more than once")]
fn parse_define_sites_panics_on_a_genuine_duplicate_topic_version_pair() {
    let text = "define(\"x\", 1, HistoryPolicy::KeepForever);\ndefine(\"x\", 1, HistoryPolicy::KeepForever);\n";
    parse_define_sites("fixture", text);
}

#[test]
#[should_panic(expected = "is not an integer literal")]
fn parse_define_sites_panics_on_a_non_literal_version() {
    // A const token (or any non-integer version argument) must fail loudly, never be
    // silently skipped -- this scan is a gate, not a best-effort hint.
    let text = "define(\"x\", VERSION, HistoryPolicy::KeepForever);\n";
    parse_define_sites("fixture", text);
}

#[test]
fn parse_define_sites_non_literal_panic_names_the_file_label() {
    // The label pin, separate from the branch pin above: the panic must say WHICH file.
    let text = "define(\"x\", VERSION, HistoryPolicy::KeepForever);\n";
    let err = std::panic::catch_unwind(|| parse_define_sites("api/x/events/src/lib.rs", text))
        .expect_err("non-literal version must panic");
    let msg = err.downcast_ref::<String>().expect("panic payload is a String");
    assert!(msg.contains("api/x/events/src/lib.rs"), "{msg}");
    assert!(msg.contains("is not an integer literal"), "{msg}");
}

#[test]
#[should_panic(expected = "not a plain decimal integer literal")]
fn parse_define_sites_rejects_underscore_separated_version_instead_of_truncating() {
    // `take_while(is_ascii_digit)` alone would read `1_0` as version 1 -- silently
    // recording the wrong version. The boundary check must reject, not truncate.
    let text = "define(\"x\", 1_0, HistoryPolicy::KeepForever);\n";
    parse_define_sites("fixture", text);
}

#[test]
#[should_panic(expected = "not a plain decimal integer literal")]
fn parse_define_sites_rejects_hex_version_instead_of_truncating() {
    // `0x10` would truncate to version 0 under take_while alone.
    let text = "define(\"x\", 0x10, HistoryPolicy::KeepForever);\n";
    parse_define_sites("fixture", text);
}

#[test]
fn parse_define_sites_scans_every_define_on_one_line() {
    // Two defines packed onto one physical line: a first-occurrence-only scan would
    // silently drop the second site.
    let text = "define(\"x\", 1, H::K); define(\"y\", 2, H::K);\n";
    let sites = parse_define_sites("fixture", text);
    assert_eq!(
        sites,
        BTreeSet::from([("x".to_string(), 1), ("y".to_string(), 2)])
    );
}

#[test]
fn union_define_sites_merges_distinct_sites_across_files() {
    let got = union_define_sites([
        ("file-a", "define(\"x\", 1, H::K);\n"),
        ("file-b", "define(\"y\", 1, H::K);\ndefine(\"x\", 2, H::K);\n"),
    ]);
    assert_eq!(
        got,
        BTreeSet::from([
            ("x".to_string(), 1),
            ("x".to_string(), 2),
            ("y".to_string(), 1),
        ])
    );
}

#[test]
fn union_define_sites_panics_naming_both_files_on_a_cross_file_duplicate() {
    // The cross-file duplicate path: same (topic, version) defined in two files must
    // panic and name BOTH file labels.
    let err = std::panic::catch_unwind(|| {
        union_define_sites([
            ("file-a", "define(\"x\", 1, H::K);\n"),
            ("file-b", "define(\"x\", 1, H::K);\n"),
        ])
    })
    .expect_err("cross-file duplicate (topic, version) must panic");
    let msg = err.downcast_ref::<String>().expect("panic payload is a String");
    assert!(msg.contains("file-a") && msg.contains("file-b"), "{msg}");
    assert!(msg.contains("duplicate define-site across files"), "{msg}");
}

// --- The DEFINE set is exactly the seven domain contract topics ---------------

#[test]
fn defined_topics_are_the_seven_domain_topics_at_v1() {
    let mut got: Vec<(String, u32)> = defined_topics()
        .into_iter()
        .map(|c| (c.topic, c.version))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("admin.action".to_string(), 1),
            ("character.created".to_string(), 1),
            ("character.deleted".to_string(), 1),
            ("config.changed".to_string(), 1),
            ("match.finished".to_string(), 1),
            ("player.registered".to_string(), 1),
            ("scheduler.fired".to_string(), 1),
        ]
    );
}

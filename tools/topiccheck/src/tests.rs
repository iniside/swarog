use super::*;

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

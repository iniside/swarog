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

// --- Check 5: unsubscribed (advisory) ----------------------------------------

#[test]
fn all_subscribed_yields_no_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert!(unsubscribed(&d, &topics(&["a", "b", "extra"]), &[]).is_empty());
}

#[test]
fn missing_subscriber_is_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert_eq!(unsubscribed(&d, &topics(&["a"]), &[]), vec!["b".to_string()]);
}

#[test]
fn allowlist_suppresses_unsubscribed() {
    let d = defs(&[("a", 1), ("b", 1)]);
    assert!(unsubscribed(&d, &topics(&["a"]), &["b"]).is_empty());
}

// --- The DEFINE set is exactly the six domain contract topics ----------------

#[test]
fn defined_topics_are_the_six_domain_topics_at_v1() {
    let mut got: Vec<(String, u32)> = defined_topics()
        .into_iter()
        .map(|c| (c.topic, c.version))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("character.created".to_string(), 1),
            ("character.deleted".to_string(), 1),
            ("config.changed".to_string(), 1),
            ("match.finished".to_string(), 1),
            ("player.registered".to_string(), 1),
            ("scheduler.fired".to_string(), 1),
        ]
    );
}

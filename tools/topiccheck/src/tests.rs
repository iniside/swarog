use super::*;

/// Helper: a defined set from plain topic strings (labels are irrelevant to the diff).
fn defs(topics: &[&str]) -> Vec<(String, &'static str)> {
    topics.iter().map(|t| (t.to_string(), "site")).collect()
}

fn subs(topics: &[&str]) -> BTreeSet<String> {
    topics.iter().map(|t| t.to_string()).collect()
}

#[test]
fn all_subscribed_yields_no_findings() {
    let d = defs(&["a", "b", "c"]);
    let s = subs(&["a", "b", "c", "extra"]);
    assert!(unsubscribed(&d, &s, &[]).is_empty());
}

#[test]
fn missing_subscriber_is_flagged() {
    let d = defs(&["a", "b", "c"]);
    let s = subs(&["a", "c"]);
    assert_eq!(unsubscribed(&d, &s, &[]), vec!["b".to_string()]);
}

#[test]
fn allowlist_suppresses_a_finding() {
    let d = defs(&["a", "b"]);
    let s = subs(&["a"]);
    // "b" is unsubscribed but allowlisted, so no finding remains.
    assert!(unsubscribed(&d, &s, &["b"]).is_empty());
}

#[test]
fn allowlist_only_covers_named_topics() {
    let d = defs(&["a", "b"]);
    let s = subs(&[]);
    // "a" is allowlisted; "b" is not -> only "b" is a finding.
    assert_eq!(unsubscribed(&d, &s, &["a"]), vec!["b".to_string()]);
}

/// The real DEFINE set is exactly the six domain topics — a guard against a topic
/// being added to an events crate without being wired into `defined_topics()`.
#[test]
fn defined_topics_are_the_six_domain_topics() {
    let mut got: Vec<String> = defined_topics().into_iter().map(|(t, _)| t).collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            "character.created".to_string(),
            "character.deleted".to_string(),
            "config.changed".to_string(),
            "match.finished".to_string(),
            "player.registered".to_string(),
            "scheduler.fired".to_string(),
        ]
    );
}

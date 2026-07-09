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

/// Helper: build a `by_topic` map from `(topic, subscribers)` pairs.
fn by_topic(entries: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    entries
        .iter()
        .map(|(t, ss)| {
            (
                t.to_string(),
                ss.iter().map(|s| s.to_string()).collect::<BTreeSet<String>>(),
            )
        })
        .collect()
}

#[test]
fn inprocess_subscribed_defined_topic_is_flagged() {
    let d = defs(&["a", "b"]);
    // "a" has an in-process subscriber -> durability violation; "b" is durable-only.
    let bt = by_topic(&[
        ("a", &[IN_PROCESS_SENTINEL]),
        ("b", &["some.durable.subscriber"]),
    ]);
    assert_eq!(
        inprocess_defined(&d, &bt, &[]),
        vec!["a".to_string()]
    );
}

#[test]
fn allowlist_suppresses_a_durability_finding() {
    let d = defs(&["a"]);
    let bt = by_topic(&[("a", &[IN_PROCESS_SENTINEL])]);
    // "a" is in-process-subscribed but allowlisted -> no finding.
    assert!(inprocess_defined(&d, &bt, &["a"]).is_empty());
}

#[test]
fn durable_only_subscriber_is_not_a_durability_finding() {
    let d = defs(&["a"]);
    // A real durable subscriber (not the sentinel) is exactly what's required -> clean.
    let bt = by_topic(&[("a", &["leaderboard"])]);
    assert!(inprocess_defined(&d, &bt, &[]).is_empty());
}

#[test]
fn mixed_durable_and_inprocess_still_flags() {
    let d = defs(&["a"]);
    // Even with a durable subscriber present, ANY in-process sub to a defined topic
    // is a violation.
    let bt = by_topic(&[("a", &["leaderboard", IN_PROCESS_SENTINEL])]);
    assert_eq!(inprocess_defined(&d, &bt, &[]), vec!["a".to_string()]);
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

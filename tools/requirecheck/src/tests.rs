use super::*;

/// Build a `declared` map from `(module, [providers])` pairs.
fn declared(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    entries
        .iter()
        .map(|(m, ps)| (m.to_string(), ps.iter().map(|p| p.to_string()).collect()))
        .collect()
}

/// Build a hits vec from `(module, kind, key)` triples.
fn hits(entries: &[(&str, RequireKind, &str)]) -> Vec<(String, RequireKind, String)> {
    entries
        .iter()
        .map(|(m, k, key)| (m.to_string(), *k, key.to_string()))
        .collect()
}

#[test]
fn declared_mandatory_require_is_clean() {
    // inventory requires characters+config and calls both — no violation.
    let h = hits(&[
        ("inventory", RequireKind::Mandatory, "characters.ownership"),
        ("inventory", RequireKind::Mandatory, "config.reader"),
    ]);
    let d = declared(&[("inventory", &["characters", "config", "messaging"])]);
    assert!(undeclared(&h, &d, ALLOWLIST).is_empty());
}

#[test]
fn undeclared_mandatory_require_is_flagged() {
    // inventory calls require("characters.ownership") but forgot to declare it.
    let h = hits(&[("inventory", RequireKind::Mandatory, "characters.ownership")]);
    let d = declared(&[("inventory", &["config", "messaging"])]);
    assert_eq!(
        undeclared(&h, &d, ALLOWLIST),
        vec![("inventory".to_string(), "characters".to_string())]
    );
}

#[test]
fn optional_try_require_is_never_flagged() {
    // gateway's try_require::<Sessions> is Optional and deliberately undeclared.
    let h = hits(&[("gateway", RequireKind::Optional, "accounts.sessions")]);
    let d = declared(&[("gateway", &[])]);
    assert!(undeclared(&h, &d, ALLOWLIST).is_empty());
}

#[test]
fn messaging_is_allowlisted_as_provider_and_declaration() {
    // A (hypothetical) mandatory require whose provider is messaging is never flagged,
    // even when messaging is somehow absent from requires().
    let h = hits(&[("audit", RequireKind::Mandatory, "messaging.marker")]);
    let d = declared(&[("audit", &[])]);
    assert!(undeclared(&h, &d, &["messaging"]).is_empty());
}

#[test]
fn provider_prefix_maps_key_to_module() {
    assert_eq!(provider_of("rating.mmr_reader"), "rating");
    assert_eq!(provider_of("characters.ownership"), "characters");
    // A key with no dot maps to the whole key.
    assert_eq!(provider_of("messaging"), "messaging");
}

#[test]
fn observed_mandatory_dedups_and_ignores_optional() {
    let h = hits(&[
        ("match", RequireKind::Mandatory, "rating.mmr_reader"),
        ("match", RequireKind::Mandatory, "rating.other_cap"),
        ("match", RequireKind::Optional, "config.reader"),
    ]);
    let obs = observed_mandatory(&h, "match");
    // rating deduped to one entry; config (optional) excluded.
    assert_eq!(obs, BTreeSet::from(["rating".to_string()]));
}

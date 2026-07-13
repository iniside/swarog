//! The contract-golden check IS its own test (the archcheck/checkmodules idiom): this
//! runs the live-vs-committed diff over the real workspace during `cargo test`, so a
//! contract-value edit without a re-bless fails the test suite too, not only the
//! verify stage. DB-free: `defined_topics()` reads statics and `route_bindings()` is
//! impl-free.

use super::*;

/// The committed golden must exist and match the live `define(...)` +
/// `route_bindings()` values exactly. On drift, either revert the value change or
/// re-bless intentionally (`cargo run -p verifyctl -- --bless-contract-golden`).
#[test]
fn committed_golden_matches_live_contract_values() {
    let findings = check().expect("contract-golden check must run");
    assert!(
        findings.is_empty(),
        "live contract values differ from {GOLDEN_REL}:\n  {}\n(if intentional, re-bless \
         via cargo run -p verifyctl -- --bless-contract-golden)",
        findings.join("\n  ")
    );
}

/// The rpc-module hand-list self-check must agree with the `#[rpc]` traits actually
/// present under `api/*/api/` (house rule: hand-maintained lists self-verify).
#[test]
fn rpc_module_hand_list_matches_filesystem() {
    // `live_lines` runs the self-check internally; surface it directly so a drift
    // failure names this test rather than the golden diff.
    let modules = rpc_modules();
    let labels: Vec<&'static str> = modules.iter().map(|(l, _, _, _)| *l).collect();
    self_check_rpc_list(&labels).expect("rpc_modules() hand-list must match api/*/api");
}

/// The golden must cover all five kinds: at least one `event` line (seven topics
/// today), one `rpc` line (the HTTP-bound operations), one `wire` line (every method's
/// retry semantics, incl. wire-only), one `payload` line (a populated durable-event
/// wire shape), and one `rpc-body` line (an http-bound request body shape), so an
/// accidentally emptied source can't silently produce a trivially-matching golden.
#[test]
fn live_lines_cover_events_and_rpc() {
    let lines = live_lines().expect("live lines");
    assert!(lines.iter().any(|l| l.starts_with("event ")), "no event lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("rpc ")), "no rpc lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("wire ")), "no wire lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("payload ")), "no payload lines: {lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("rpc-body ")), "no rpc-body lines: {lines:?}");
}

/// `flatten_shape` renders the serde WIRE key (post-`#[serde(rename)]`), not the Rust
/// field name — the whole point of Step 5. A struct with `#[serde(rename = "someKey")]`
/// must flatten to `payload.someKey:...`, and crucially NOT to the un-renamed
/// `payload.some_field:...` — so a silent rename produces a genuine golden diff.
#[test]
fn flatten_shape_uses_serde_renamed_key_not_rust_field() {
    #[derive(serde::Serialize)]
    struct Sample {
        #[serde(rename = "someKey")]
        some_field: String,
        count: i64,
        maybe: Option<String>,
        tags: Vec<String>,
    }
    let value = serde_json::to_value(Sample {
        some_field: "v".to_string(),
        count: 7,
        maybe: Some("m".to_string()),
        tags: vec!["a".to_string()],
    })
    .expect("sample serializes");
    let mut out = BTreeSet::new();
    flatten_shape("payload", &value, &mut out);
    assert!(out.contains("payload.someKey:string"), "renamed key missing: {out:?}");
    assert!(
        !out.contains("payload.some_field:string"),
        "un-renamed Rust field name leaked into the golden: {out:?}"
    );
    assert!(out.contains("payload.count:number"), "number leaf missing: {out:?}");
    assert!(out.contains("payload.maybe:string"), "populated Option leaf missing: {out:?}");
    assert!(out.contains("payload.tags[]:string"), "array-element leaf missing: {out:?}");
}

/// The empty container encodings are distinct from a scalar and from each other, so an
/// object/array reshape can't collapse into an ambiguous line.
#[test]
fn flatten_shape_empty_containers_are_distinct() {
    let mut out = BTreeSet::new();
    flatten_shape("body", &serde_json::json!({}), &mut out);
    flatten_shape("body", &serde_json::json!([]), &mut out);
    assert!(out.contains("body:object{}"), "empty object encoding missing: {out:?}");
    assert!(out.contains("body:array[]"), "empty array encoding missing: {out:?}");
}

/// The didn't-forget check FAILS (naming the topic) when a defined topic has no
/// populated `golden_samples()` entry — the failing branch, executed directly rather
/// than asserted by its absence. Built as a unit over the helper's set logic so the
/// test doesn't require doctoring the real crate hand-list.
#[test]
fn event_sample_drift_names_a_defined_topic_with_no_sample() {
    // Execute the real MISSING branch: a defined topic with no sample must produce a
    // drift message that NAMES the topic (proving the failing branch, not asserting its
    // absence).
    let defined: BTreeSet<(String, u32)> =
        BTreeSet::from([("a.topic".to_string(), 1), ("b.topic".to_string(), 1)]);
    let sampled: BTreeSet<(String, u32)> = BTreeSet::from([("a.topic".to_string(), 1)]);
    let drift = event_sample_drift(&defined, &sampled);
    assert_eq!(drift.len(), 1, "exactly one missing entry: {drift:?}");
    assert!(
        drift[0].contains("MISSING") && drift[0].contains("b.topic v1"),
        "drift message must name the un-sampled topic: {drift:?}"
    );

    // The reverse STALE branch: a sample with no defined topic is flagged too.
    let stale = event_sample_drift(
        &BTreeSet::from([("a.topic".to_string(), 1)]),
        &BTreeSet::from([("a.topic".to_string(), 1), ("ghost.topic".to_string(), 2)]),
    );
    assert_eq!(stale.len(), 1, "exactly one stale entry: {stale:?}");
    assert!(
        stale[0].contains("STALE") && stale[0].contains("ghost.topic v2"),
        "drift message must name the orphaned sample: {stale:?}"
    );
}

/// The live check passes on the real crate — every defined topic IS sampled and no
/// sample is orphaned, proving the two hand-lists are in sync.
#[test]
fn self_check_event_samples_passes_on_real_crate() {
    self_check_event_samples().expect("every defined topic must have a populated sample");
}

/// Step 5b, item 2 — body_shapes() excludes PATH-WILDCARD args (they travel in the
/// route path, never the HTTP body) while keeping genuine body keys. Asserted over the
/// real generated modules: match.report keeps its Go-parity body keys
/// ReportId/Winner/Loser; inventory.listCharacter (whose only arg `character_id` is the
/// `{id}` path wildcard) has an EMPTY body shape.
#[test]
fn body_shapes_exclude_path_wildcards_but_keep_body_keys() {
    let report = matchapi::match_rpc::body_shapes()
        .into_iter()
        .find(|(m, _)| *m == "match.report")
        .expect("match.report body shape present")
        .1;
    let obj = report.as_object().expect("match.report body is an object");
    for key in ["ReportId", "Winner", "Loser"] {
        assert!(obj.contains_key(key), "match.report body must keep key {key}: {obj:?}");
    }

    let list_character = inventoryapi::holdings_rpc::body_shapes()
        .into_iter()
        .find(|(m, _)| *m == "inventory.listCharacter")
        .expect("inventory.listCharacter body shape present")
        .1;
    let obj = list_character
        .as_object()
        .expect("inventory.listCharacter body is an object");
    assert!(
        !obj.contains_key("character_id"),
        "path-wildcard character_id must not appear under body.: {obj:?}"
    );
    assert!(
        obj.is_empty(),
        "listCharacter's only arg is the path wildcard, so its body shape is empty: {obj:?}"
    );
}

/// The Option-None tripwire's text scan: a real `pub <name>: Option<` field is found;
/// an `Option<` inside a doc comment or a non-Option field is not (the same
/// comment-filtered no-lexer tolerance as parse_define_sites).
#[test]
fn scan_option_fields_finds_real_fields_and_skips_comments() {
    let text = "\
/// This doc mentions Option<String> in prose -- a phantom, must be skipped.
// pub commented_out: Option<String>,
pub struct Payload {
    pub required: String,
    pub maybe: Option<String>,
    pub also_maybe: Option<i64>,
}
";
    let fields = scan_option_fields(text);
    assert_eq!(
        fields,
        BTreeSet::from(["maybe".to_string(), "also_maybe".to_string()]),
        "exactly the two real Option fields, no comment phantoms"
    );
}

/// The tripwire's failing branch, executed on a fixture: an Option field whose crate
/// samples never carry it as None yields a per-field message naming crate and field;
/// adding a None sample clears it.
#[test]
fn option_none_gaps_flags_an_option_field_with_no_null_sample() {
    let fields = BTreeSet::from(["maybe".to_string()]);
    // Only a populated sample -- the None case is uncovered.
    let populated = vec![("x.topic", 1u32, serde_json::json!({ "maybe": "v", "req": "r" }))];
    let gaps = option_none_gaps("fixture", &fields, &populated);
    assert_eq!(gaps.len(), 1, "exactly one gap: {gaps:?}");
    assert!(
        gaps[0].contains("api/fixture/events") && gaps[0].contains("`maybe`"),
        "gap message must name the crate and field: {gaps:?}"
    );

    // With a second None-carrying sample the gap closes.
    let with_null = vec![
        ("x.topic", 1u32, serde_json::json!({ "maybe": "v", "req": "r" })),
        ("x.topic", 1u32, serde_json::json!({ "maybe": null, "req": "r" })),
    ];
    assert!(option_none_gaps("fixture", &fields, &with_null).is_empty());
}

/// The gate-path tripwire passes on the real tree: the only Option field across all
/// events crates (configevents::Changed.value) has its None sample.
#[test]
fn self_check_option_none_samples_passes_on_real_crate() {
    self_check_option_none_samples()
        .expect("every Option payload field must have a None-covering golden sample");
}

/// Step 5b, item 1 — the Option-None convention materializes: config.changed carries a
/// delete-shaped sample with `value: None`, so BOTH the Some-shape (`value:string`) and
/// the None-shape (`value:null`) are pinned lines. (The compile-coupling half — the
/// `value: None` literal failing to compile if the field became `String` — is enforced
/// by the compiler itself.)
#[test]
fn config_changed_pins_both_option_value_shapes() {
    let lines = live_lines().expect("live lines");
    assert!(
        lines.contains("payload topic=config.changed version=1 payload.value:string"),
        "Some-shape line missing"
    );
    assert!(
        lines.contains("payload topic=config.changed version=1 payload.value:null"),
        "None-shape line missing (the Option-None sample convention)"
    );
}

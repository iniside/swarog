use super::*;

/// Wraps a request-argument type in a minimal `#[rpc]` contract, so every case below
/// runs the REAL traversal (`discover_sources`) rather than calling `collect_type`
/// with a hand-built type index.
fn contract(arg_ty: &str, extra_items: &str) -> String {
    format!(
        r#"
            {extra_items}
            #[rpc(prefix = "demo")]
            pub trait Demo {{ async fn send(&self, request: {arg_ty}) -> Result<(), Error>; }}
        "#
    )
}

fn keys(source: &str) -> Vec<String> {
    discover_sources(&[source.to_owned()])
        .unwrap()
        .into_iter()
        .map(|key| render_key(&key))
        .collect()
}

fn rejection(source: &str) -> String {
    format!(
        "{:#}",
        discover_sources(&[source.to_owned()]).expect_err("must fail closed")
    )
}

#[test]
fn traverses_request_dtos_but_not_outputs() {
    let source = r#"
            #[derive(serde::Serialize)]
            pub struct Request { #[serde(rename = "displayName")] pub display_name: String, pub tags: Vec<Option<String>> }
            pub struct Output { pub secret: String }
            #[rpc(prefix = "demo")]
            pub trait Demo { #[http(verb="POST", path="/", auth="none", success=200)] async fn send(&self, request: Request) -> Result<Output, Error>; }
        "#;
    assert_eq!(
        keys(source),
        [
            "demo.send\trequest.displayName\texternal",
            "demo.send\trequest.tags\texternal"
        ]
    );
}

// ---- Fail-closed traversal ---------------------------------------------------
//
// Each case below is a shape the traversal previously dropped through a bare
// `return Ok(())`, leaving an unbounded caller string invisible to every gate.

#[test]
fn an_unresolvable_named_type_is_rejected_naming_type_method_field_and_remedy() {
    let message = rejection(&contract("Foreign", ""));
    assert!(message.contains("demo.send"), "{message}");
    assert!(message.contains("\"request\""), "{message}");
    assert!(message.contains("\"Foreign\""), "{message}");
    assert!(message.contains("OPAQUE_REQUEST_TYPES"), "{message}");
}

#[test]
fn an_enum_request_argument_is_rejected() {
    let message = rejection(&contract("Mode", "pub enum Mode { A, B }"));
    assert!(message.contains("\"Mode\""), "{message}");
}

#[test]
fn a_tuple_struct_is_rejected_because_it_has_no_wire_field_names() {
    let message = rejection(&contract("Wrapper", "pub struct Wrapper(pub String);"));
    assert!(message.contains("tuple or unit struct"), "{message}");
    assert!(message.contains("\"Wrapper\""), "{message}");
}

#[test]
fn a_nested_unresolvable_field_is_rejected_under_its_field_path() {
    let message = rejection(&contract(
        "Request",
        "pub struct Request { pub inner: Foreign }",
    ));
    assert!(message.contains("\"request.inner\""), "{message}");
    assert!(message.contains("\"Foreign\""), "{message}");
}

#[test]
fn unnamed_types_are_rejected_with_what_the_traversal_hit() {
    for (arg_ty, expected) in [
        ("(String, String)", "a tuple type"),
        ("[u8; 32]", "an array type"),
        ("impl Into<String>", "an `impl Trait` type"),
    ] {
        let message = rejection(&contract(arg_ty, ""));
        assert!(message.contains(expected), "{arg_ty}: {message}");
    }
}

#[test]
fn primitive_scalars_and_listed_opaque_types_traverse_clean() {
    assert!(keys(&contract("i64", "")).is_empty());
    assert!(keys(&contract("bool", "")).is_empty());
    // The one entry in OPAQUE_REQUEST_TYPES, exercised through the real traversal.
    assert!(keys(&contract("Identity", "")).is_empty());
}

#[test]
fn a_reference_to_str_is_a_string_input() {
    assert_eq!(keys(&contract("&str", "")), ["demo.send\trequest\twire"]);
}

#[test]
fn a_string_map_records_both_its_key_and_value_legs() {
    assert_eq!(
        keys(&contract("HashMap<String, String>", "")),
        [
            "demo.send\trequest.<key>\twire",
            "demo.send\trequest.<value>\twire"
        ]
    );
}

/// `adminapi::Params` is a type alias, not a struct — the shape that made the whole
/// admin surface invisible. The alias must resolve before any other decision.
#[test]
fn a_type_alias_resolves_to_its_target_before_being_classified() {
    assert_eq!(
        keys(&contract(
            "Params",
            "pub type Params = HashMap<String, String>;"
        )),
        [
            "demo.send\trequest.<key>\twire",
            "demo.send\trequest.<value>\twire"
        ]
    );
}

#[test]
fn a_map_whose_value_is_a_dto_traverses_into_that_dto() {
    assert_eq!(
        keys(&contract(
            "BTreeMap<String, Inner>",
            "pub struct Inner { pub note: String }"
        )),
        [
            "demo.send\trequest.<key>\twire",
            "demo.send\trequest.<value>.note\twire"
        ]
    );
}

#[test]
fn a_map_whose_value_is_unresolvable_is_rejected() {
    let message = rejection(&contract("HashMap<String, Foreign>", ""));
    assert!(message.contains("\"request.<value>\""), "{message}");
    assert!(message.contains("\"Foreign\""), "{message}");
}

#[test]
fn a_duplicate_type_declaration_is_ambiguous() {
    let message = rejection(&contract(
        "Request",
        "pub struct Request { pub a: String } pub type Request = String;",
    ));
    assert!(message.contains("declared more than once"), "{message}");
}

// ---- Golden + policy diffing ---------------------------------------------------

#[test]
fn golden_omission_is_a_finding() {
    let findings = golden_findings("header\na\n", "header\n");
    assert_eq!(findings.len(), 1);
    // The finding must name the writer, not just the outcome.
    assert!(findings[0].contains("--bless-input-golden"), "{findings:?}");
}

#[test]
fn missing_or_orphan_or_duplicate_policy_is_a_finding() {
    let a = InputKey {
        wire_method: "demo.send".into(),
        wire_field_name: "a".into(),
        exposure: Exposure::External,
    };
    let b = InputKey {
        wire_field_name: "b".into(),
        ..a.clone()
    };
    let discovered = BTreeSet::from([a]);
    let findings = policy_key_findings(&discovered, &[b.clone(), b]);
    assert!(findings
        .iter()
        .any(|finding| finding.contains("missing input policy")));
    assert!(findings
        .iter()
        .any(|finding| finding.contains("orphan input policy")));
    assert!(findings
        .iter()
        .any(|finding| finding.contains("duplicate input policy")));
}

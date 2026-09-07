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
fn primitive_scalars_traverse_clean() {
    assert!(keys(&contract("i64", "")).is_empty());
    assert!(keys(&contract("bool", "")).is_empty());
}

/// A LEADING `Identity` never reaches `collect_type` at all — `build_method` strips it
/// into `MethodModel::has_identity`. Pinned so the next reader does not mistake this for
/// coverage of the `OPAQUE_REQUEST_TYPES` arm.
#[test]
fn a_leading_identity_is_stripped_before_the_traversal_sees_it() {
    assert!(keys(&contract("Identity", "")).is_empty());
}

/// The shape that actually executes the `OPAQUE_REQUEST_TYPES` arm: `Identity` in a
/// NON-leading position, where nothing strips it. Without the arm this bails.
#[test]
fn a_non_leading_listed_opaque_type_traverses_clean() {
    let source = r#"
            #[rpc(prefix = "demo")]
            pub trait Demo { async fn send(&self, note: String, actor: Identity) -> Result<(), Error>; }
        "#;
    assert_eq!(keys(source), ["demo.send\tnote\twire"]);
}

/// The allowlist matches the WRITTEN path, so a foreign type that merely shares the last
/// segment with a listed one is rejected rather than silently waved through.
#[test]
fn a_foreign_type_sharing_the_opaque_name_is_still_rejected() {
    let source = r#"
            #[rpc(prefix = "demo")]
            pub trait Demo { async fn send(&self, note: String, actor: somecrate::Identity) -> Result<(), Error>; }
        "#;
    let message = rejection(source);
    assert!(message.contains("somecrate::Identity"), "{message}");
    assert!(message.contains("OPAQUE_REQUEST_TYPES"), "{message}");
}

/// The opaque list self-checks against its real source of truth: each entry must name a
/// type actually declared in the file it points at, whose every field is PRIVATE. That
/// encapsulation — not "holds no String" — is the property the `why` rests on:
/// `opsapi::Identity` wraps an `Option<String>`, and what makes it safe is that no
/// caller-supplied request field can ever be assigned into it.
#[test]
fn every_opaque_request_type_names_a_real_encapsulated_type() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for opaque in OPAQUE_REQUEST_TYPES {
        assert!(!opaque.why.trim().is_empty(), "{}: blank why", opaque.name);
        let path = root.join(opaque.declared_in);
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "OPAQUE_REQUEST_TYPES entry {:?} points at {} which cannot be read: {error}",
                opaque.name,
                path.display()
            )
        });
        let file = syn::parse_file(&source).expect("parse the declaring file");
        let ident = opaque.name.rsplit("::").next().unwrap();
        let declared = file.items.iter().find_map(|item| match item {
            Item::Struct(item) if item.ident == ident => Some(item),
            _ => None,
        });
        let Some(declared) = declared else {
            panic!(
                "OPAQUE_REQUEST_TYPES entry {:?} names no struct in {} — the list allowlists a \
                 type that was renamed or deleted, so the arm is dead and its reason is a lie",
                opaque.name, opaque.declared_in
            );
        };
        let public: Vec<String> = declared
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| matches!(field.vis, syn::Visibility::Public(_)))
            .map(|(index, field)| match &field.ident {
                Some(ident) => ident.to_string(),
                None => index.to_string(),
            })
            .collect();
        assert!(
            public.is_empty(),
            "OPAQUE_REQUEST_TYPES entry {:?} now has public field(s) {public:?} — a request \
             DTO can carry caller text through them, so the entry's reason no longer holds",
            opaque.name
        );
    }
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

// ---- Total discovery ---------------------------------------------------------
//
// `discover_sources` used to walk `file.items` at the TOP LEVEL only, so an `#[rpc]`
// trait or a request DTO inside an inline `mod` was invisible: no key, no golden row,
// every gate green. Contract crates already declare inline modules (`pub mod admin` in
// api/accounts and api/characters).

#[test]
fn a_contract_declared_in_an_inline_mod_is_discovered() {
    let source = r#"
            mod inner {
                pub struct Request { pub note: String }
                #[rpc(prefix = "demo")]
                pub trait Demo { async fn send(&self, request: Request) -> Result<(), Error>; }
            }
        "#;
    assert_eq!(keys(source), ["demo.send\trequest.note\twire"]);
}

/// The DTO and the trait need not share a module — the walk builds ONE type index across
/// every nesting level before any trait is traversed.
#[test]
fn a_dto_nested_beside_a_top_level_trait_is_discovered() {
    let source = r#"
            pub mod dtos { pub struct Request { pub note: String } }
            #[rpc(prefix = "demo")]
            pub trait Demo { async fn send(&self, request: Request) -> Result<(), Error>; }
        "#;
    assert_eq!(keys(source), ["demo.send\trequest.note\twire"]);
}

/// A `#[cfg(test)]` module is skipped, mirroring `contract_sources`' exclusion of
/// `tests.rs` — a fixture contract in a test module is not a real contract.
#[test]
fn a_cfg_test_inline_module_is_not_a_contract_source() {
    let source = r#"
            #[cfg(test)]
            mod tests {
                #[rpc(prefix = "fixture")]
                pub trait Fixture { async fn send(&self, note: String) -> Result<(), Error>; }
            }
        "#;
    assert!(keys(source).is_empty());
}

#[test]
fn an_item_position_macro_invocation_is_rejected() {
    let message = rejection("declare_contract! { Demo }");
    assert!(message.contains("declare_contract!"), "{message}");
    assert!(message.contains("macro invocation"), "{message}");
}

#[test]
fn a_macro_invocation_inside_an_rpc_trait_body_is_rejected() {
    let source = r#"
            #[rpc(prefix = "demo")]
            pub trait Demo { extra_methods!(); }
        "#;
    let message = rejection(source);
    assert!(message.contains("extra_methods!"), "{message}");
    assert!(message.contains("\"Demo\""), "{message}");
}

/// A `macro_rules!` DEFINITION declares no contract by itself, and every use of it is an
/// invocation the check above already rejects.
#[test]
fn a_macro_rules_definition_alone_is_not_a_rejection() {
    assert!(keys("macro_rules! helper { () => {}; }").is_empty());
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

// --- Which domains contribute input keys ------------------------------------

fn scratch_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "conformance-served-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("modules")).unwrap();
    root
}

fn write_domain(root: &Path, domain: &str, prefix: &str, served: bool) {
    let src = root.join("api").join(domain).join("api/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        format!(
            r#"
                #[rpc(prefix = "{prefix}")]
                pub trait Demo {{ async fn send(&self, note: String) -> Result<(), Error>; }}
            "#
        ),
    )
    .unwrap();
    if served {
        let module = root.join("modules").join(domain);
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("Cargo.toml"), "").unwrap();
    }
}

/// The retarget's branch: a served domain's request fields still demand an input policy;
/// a domain whose contracts landed ahead of its module contributes none yet.
#[test]
fn only_a_served_domain_contributes_input_keys() {
    let root = scratch_root("split");
    write_domain(&root, "served", "served", true);
    write_domain(&root, "contractonly", "contractonly", false);

    let discovered = discover(&root).unwrap();
    let methods: BTreeSet<&str> = discovered
        .iter()
        .map(|key| key.wire_method.as_str())
        .collect();
    assert_eq!(methods, BTreeSet::from(["served.send"]));
    let _ = std::fs::remove_dir_all(root);
}

/// An unscannable `modules/` must fail the discovery, never yield an empty inventory that
/// would match an empty policy and pass green.
#[test]
fn an_unscannable_modules_dir_fails_discovery() {
    let root = scratch_root("vacuous");
    write_domain(&root, "served", "served", true);
    std::fs::remove_dir_all(root.join("modules")).unwrap();
    assert!(discover(&root).is_err());
    let _ = std::fs::remove_dir_all(root);
}

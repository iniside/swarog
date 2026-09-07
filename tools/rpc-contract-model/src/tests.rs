use super::*;

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rpc-contract-model-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The branch the four contract scanners used to be blind to: a declaration split
/// out of `lib.rs` into a sibling module, at any depth. It must be returned.
#[test]
fn a_module_split_out_of_lib_is_still_a_contract_source() {
    let root = temp_dir("split");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    for path in ["lib.rs", "topics.rs", "nested/more.rs"] {
        std::fs::write(root.join(path), "").unwrap();
    }
    let found = contract_sources(&root).unwrap();
    assert_eq!(
        found,
        vec![
            root.join("lib.rs"),
            root.join("nested/more.rs"),
            root.join("topics.rs"),
        ]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_modules_and_non_rust_files_are_excluded() {
    let root = temp_dir("excluded");
    for path in ["lib.rs", "tests.rs", "store_tests.rs", "notes.md", "tests"] {
        std::fs::write(root.join(path), "").unwrap();
    }
    assert_eq!(contract_sources(&root).unwrap(), vec![root.join("lib.rs")]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_source_directory_is_an_error_not_an_empty_set() {
    assert!(contract_sources(&temp_dir("absent").join("nope")).is_err());
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The property every served-surface gate rests on: the predicate names EVERY module
/// crate directory. An under-reporting predicate would make all five gates silently
/// vacuous for the domain it dropped, which is the whole risk of keying them on
/// `modules/` — so the expectation is recomputed here by an independent, dumber walk
/// rather than a hand-list that could drift with it.
#[test]
fn served_domains_names_every_module_crate_directory() {
    let root = workspace_root();
    let mut expected = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("modules")).unwrap() {
        let path = entry.unwrap().path();
        if path.join("Cargo.toml").is_file() {
            expected.insert(path.file_name().unwrap().to_str().unwrap().to_owned());
        }
    }
    assert!(!expected.is_empty(), "modules/ must hold module crates");
    assert_eq!(served_domains(&root).unwrap(), expected);
}

/// The retarget's one new hole, pinned: a domain whose contracts exist without a module
/// is NOT served, so a served-surface gate is not allowed to demand a gateway stub, a
/// catalog row, a client provider or an input policy for it yet.
#[test]
fn a_contract_only_domain_is_not_served() {
    let root = workspace_root();
    let served = served_domains(&root).unwrap();
    for entry in std::fs::read_dir(root.join("api")).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let domain = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(
            served.contains(&domain),
            root.join("modules").join(&domain).join("Cargo.toml").is_file(),
            "api/{domain} served-ness must follow modules/{domain}"
        );
    }
}

/// Without the emptiness check, a scan of the wrong root returns `Ok({})` and every
/// caller's gate passes over every domain — the failure this fix must not introduce.
#[test]
fn an_empty_modules_directory_is_an_error_not_an_empty_set() {
    let root = temp_dir("empty-modules");
    std::fs::create_dir_all(root.join("modules")).unwrap();
    let error = served_domains(&root).expect_err("an empty scan must be loud");
    assert!(
        error.to_string().contains("vacuous"),
        "error must say why an empty set is refused: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_modules_directory_is_an_error() {
    let root = temp_dir("no-modules");
    assert!(served_domains(&root).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_directory_without_a_manifest_is_not_a_module() {
    let root = temp_dir("stray-dir");
    std::fs::create_dir_all(root.join("modules/real")).unwrap();
    std::fs::create_dir_all(root.join("modules/stray/src")).unwrap();
    std::fs::write(root.join("modules/real/Cargo.toml"), "").unwrap();
    assert_eq!(
        served_domains(&root).unwrap(),
        BTreeSet::from(["real".to_owned()])
    );
    let _ = std::fs::remove_dir_all(root);
}

// --- Rule 21: the CONTRACT_ONLY exemption set is explicit and self-checking ---------

/// `api/<domain>/api/src/lib.rs` with one `#[http(` op — the shape `http_op_domains`
/// keys on.
fn write_http_domain(root: &Path, domain: &str) {
    let src = root.join("api").join(domain).join("api/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "#[rpc(prefix = \"x\")]\npub trait X {\n    \
         #[http(verb = \"POST\", path = \"/x\", auth = \"none\", success = 200)]\n    \
         async fn go(&self) -> Result<(), Error>;\n}\n",
    )
    .unwrap();
}

/// `modules/<name>/Cargo.toml` — the filesystem answer `served_domains` reads.
fn write_module(root: &Path, name: &str) {
    let dir = root.join("modules").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
}

/// A synthetic workspace with NO rule-21 finding: one served domain, plus every
/// [`CONTRACT_ONLY`] entry in the exact shape its exemption claims (`api/<entry>` with an
/// `#[http(` op and no `modules/<entry>`). Each test then introduces exactly one defect,
/// so the asserted violation can only come from that defect.
fn clean_root(label: &str) -> PathBuf {
    let root = temp_dir(label);
    write_module(&root, "served");
    write_http_domain(&root, "served");
    for entry in CONTRACT_ONLY {
        write_http_domain(&root, entry);
    }
    let violations = contract_only_violations(&root);
    assert!(violations.is_empty(), "baseline must be clean: {violations:?}");
    root
}

/// The positive control for every test below: a tree where each `#[http(` domain either
/// has its module or is a live CONTRACT_ONLY entry produces no violation at all.
#[test]
fn a_consistent_tree_produces_no_contract_only_violation() {
    let root = clean_root("rule21-clean");
    let _ = std::fs::remove_dir_all(root);
}

/// Leg (a): an `api/<domain>` with `#[http(` ops, no `modules/<domain>` and no
/// CONTRACT_ONLY entry. Every served-surface gate skips it, so rule 21 is the only thing
/// standing between that skip and a domain that 404s through the gateway in the split.
#[test]
fn an_unlisted_unserved_http_domain_is_a_violation() {
    let root = clean_root("rule21-unlisted");
    write_http_domain(&root, "quests");

    let violations = contract_only_violations(&root);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("`quests`"), "{violations:?}");
    assert!(
        violations[0].contains("no modules/quests/Cargo.toml"),
        "the message must name the missing module dir: {violations:?}"
    );
    assert!(
        violations[0].contains("CONTRACT_ONLY"),
        "the message must name the two remedies: {violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Leg (b): a CONTRACT_ONLY entry whose module has landed. The exemption is now a lie —
/// the served-surface gates already cover the domain, and leaving the entry in place would
/// let a future skip inherit a reason that expired.
#[test]
fn a_contract_only_entry_whose_module_landed_is_stale() {
    let Some(entry) = CONTRACT_ONLY.first().copied() else {
        return; // no exemption exists to go stale
    };
    let root = clean_root("rule21-module-landed");
    write_module(&root, entry);

    let violations = contract_only_violations(&root);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("stale") && violations[0].contains(entry),
        "{violations:?}"
    );
    assert!(
        violations[0].contains(&format!("modules/{entry} now exists")),
        "the message must say WHY it is stale: {violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Leg (c) — the one never exercised before this test: a CONTRACT_ONLY entry naming a
/// domain that has no `#[http(` contract at all (never had one, or lost it). The exemption
/// silently covers nothing, and the next reader reads it as a reviewed decision about a
/// live domain.
#[test]
fn a_contract_only_entry_with_no_http_contract_is_stale() {
    let Some(entry) = CONTRACT_ONLY.first().copied() else {
        return; // no exemption exists to go stale
    };
    let root = clean_root("rule21-no-contract");
    std::fs::remove_dir_all(root.join("api").join(entry)).unwrap();

    let violations = contract_only_violations(&root);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("stale") && violations[0].contains(entry),
        "{violations:?}"
    );
    assert!(
        violations[0].contains(HTTP_OP_MARKER) && violations[0].contains("does not exist"),
        "the message must distinguish this from the module-landed leg: {violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The hole rule 21 exists to close: `api/social` implemented as `modules/socialgraph`.
/// The dir names diverge, so `served_domains` never contains "social", every
/// served-surface gate skips it (no gateway stub demanded, no catalog row, no C# provider,
/// no input policy) and it 404s through the gateway in the split while working in the
/// monolith. Between a5c8398 and 6c09bea this was a silent PASS in all five gates.
#[test]
fn a_divergently_named_module_is_a_violation_naming_the_remedy() {
    let root = clean_root("rule21-divergent");
    write_http_domain(&root, "social");
    write_module(&root, "socialgraph");

    // The divergent module IS a module — the tree is not merely module-less.
    assert!(served_domains(&root).unwrap().contains("socialgraph"));

    let violations = contract_only_violations(&root);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("`social`"), "{violations:?}");
    assert!(
        violations[0].contains("Name the module directory `modules/social`"),
        "the message must name the rename remedy: {violations:?}"
    );
    assert!(
        violations[0].contains("404"),
        "the message must name the split-only consequence: {violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The empty-scan guard reaches rule 21 too: a root with no `modules/` must produce a
/// violation line, never an empty (clean) verdict that would pass the blocking gate green.
#[test]
fn contract_only_violations_on_an_unscannable_root_is_loud() {
    let root = temp_dir("rule21-no-modules");
    write_http_domain(&root, "served");

    let violations = contract_only_violations(&root);
    assert!(
        violations.iter().any(|v| v.contains("cannot list served domains")),
        "{violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

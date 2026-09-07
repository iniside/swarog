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

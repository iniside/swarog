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

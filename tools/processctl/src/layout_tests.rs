use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::WorkspaceLayout;

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn absent_cargo_target_dir_defaults_to_root_target() {
    let root = PathBuf::from("workspace-root");
    let layout = WorkspaceLayout::from_root(root.clone(), &BTreeMap::new());
    assert_eq!(layout.target_dir, root.join("target"));
}

#[test]
fn empty_cargo_target_dir_defaults_to_root_target() {
    let root = PathBuf::from("workspace-root");
    let layout = WorkspaceLayout::from_root(root.clone(), &env(&[("CARGO_TARGET_DIR", "")]));
    assert_eq!(layout.target_dir, root.join("target"));
}

#[test]
fn relative_cargo_target_dir_resolves_against_root() {
    let root = PathBuf::from("workspace-root");
    let layout =
        WorkspaceLayout::from_root(root.clone(), &env(&[("CARGO_TARGET_DIR", "frozen-target")]));
    assert_eq!(layout.target_dir, root.join("frozen-target"));
}

#[test]
fn absolute_cargo_target_dir_is_verbatim() {
    let root = PathBuf::from("workspace-root");
    let absolute = if cfg!(windows) {
        r"C:\out\build"
    } else {
        "/out/build"
    };
    let layout = WorkspaceLayout::from_root(root, &env(&[("CARGO_TARGET_DIR", absolute)]));
    assert_eq!(layout.target_dir, PathBuf::from(absolute));
    assert!(layout.target_dir.is_absolute());
}

#[test]
fn binary_path_joins_profile_and_exe_suffix() {
    let root = PathBuf::from("workspace-root");
    let layout =
        WorkspaceLayout::from_root(root.clone(), &env(&[("CARGO_TARGET_DIR", "frozen-target")]));
    assert_eq!(
        layout.binary("debug", "splitproof"),
        root.join("frozen-target")
            .join("debug")
            .join(format!("splitproof{}", std::env::consts::EXE_SUFFIX))
    );
}

#[test]
fn binary_under_default_target_matches_conventional_layout() {
    let root = Path::new("workspace-root");
    let layout = WorkspaceLayout::from_root(root.to_path_buf(), &BTreeMap::new());
    assert_eq!(
        layout.binary("debug", "adminctl"),
        root.join("target")
            .join("debug")
            .join(format!("adminctl{}", std::env::consts::EXE_SUFFIX))
    );
}

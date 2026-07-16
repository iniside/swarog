//! Integration pin for prep's helper-timeout branch (Step-4 review fold):
//! a transient helper that outlives its deadline is force-stopped and the
//! resulting error names BOTH log paths. Uses the hidden `__test-child`
//! fixture (which runs for 60s by construction) as the timed-out helper, so
//! the tiny deadline bounds a guaranteed condition — it never races a clock.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use weles::platform::{spawn, SpawnSpec};
use weles::prep::{
    deploy, deploy_packages, helper_timeout_failure, wait_for_helper, GenerationManifest, Layout,
};

use sha2::{Digest, Sha256};

/// A fresh temp workspace root with an empty `deploy/` dir, plus a source dir
/// staged with every deployable package carrying `bytes`.
fn workspace_with_source(tag: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "weles-prep-int-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = base.join("root");
    let src = base.join("src");
    std::fs::create_dir_all(root.join("deploy")).expect("create deploy dir");
    std::fs::create_dir_all(&src).expect("create src dir");
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src.join(&file), bytes).expect("write source binary");
    }
    (root, src)
}

#[test]
fn discover_pins_a_generation_that_survives_a_later_deploy() {
    // AUTHORITY proof: a running `up` pins its generation ONCE at discover. A
    // concurrent `deploy` that flips `current` to gen-2 must NOT change the
    // already-pinned Layout — the live fleet keeps resolving gen-1 (no
    // mixed-generation fleet across a respawn).
    let (root, src_v1) = workspace_with_source("pin-v1", b"gen-1 bytes");
    let deploy_layout = Layout::discover_for_deploy(root.clone()).expect("deploy layout");
    deploy(&deploy_layout, &src_v1).expect("deploy gen-1");

    // The "running up" pins gen-1 here.
    let pinned = Layout::discover(root.clone()).expect("pin gen-1");
    assert_eq!(pinned.active_bin_dir, root.join("deploy").join("gen-1"));

    // A later deploy flips current -> gen-2 AFTER the pin.
    let src_v2 = root.parent().unwrap().join("src-v2");
    std::fs::create_dir_all(&src_v2).expect("create v2 src");
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src_v2.join(&file), b"gen-2 bytes").expect("write v2 source");
    }
    deploy(&deploy_layout, &src_v2).expect("deploy gen-2");
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current"))
            .expect("read current")
            .trim(),
        "gen-2",
        "the deploy flipped current to gen-2"
    );

    // The pinned layout STILL resolves gen-1 bytes — it never re-read current.
    assert_eq!(pinned.active_bin_dir, root.join("deploy").join("gen-1"));
    assert_eq!(
        std::fs::read(pinned.binary("edgeca")).expect("read pinned edgeca"),
        b"gen-1 bytes",
        "a pinned layout must keep executing its own generation after a redeploy"
    );

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn manifest_records_the_sha256_of_each_staged_artifact() {
    let (root, src) = workspace_with_source("hash", b"artifact bytes to hash");
    let deploy_layout = Layout::discover_for_deploy(root.clone()).expect("deploy layout");
    deploy(&deploy_layout, &src).expect("deploy gen-1");

    let manifest_path = root.join("deploy").join("gen-1").join("manifest.json");
    let manifest: GenerationManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(manifest.gen, 1);
    assert_eq!(manifest.artifacts.len(), deploy_packages().len());

    for artifact in &manifest.artifacts {
        let staged = root.join("deploy").join("gen-1").join(&artifact.file);
        let bytes = std::fs::read(&staged).expect("read staged artifact");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let recomputed: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            artifact.sha256, recomputed,
            "manifest sha256 for {} must match a fresh recompute",
            artifact.pkg
        );
        assert_eq!(artifact.bytes, bytes.len() as u64, "byte length must match");
    }

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn helper_timeout_branch_kills_the_child_and_names_both_logs() {
    let dir = std::env::temp_dir().join(format!("weles-prep-timeout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create test temp dir");
    let out_path = dir.join("helper.out.log");
    let err_path = dir.join("helper.err.log");
    let stdout = File::create(&out_path).expect("create out log");
    let stderr = File::create(&err_path).expect("create err log");

    // Minimal deliberate pass-through (same shape as tests/platform.rs).
    let mut env = BTreeMap::new();
    for key in ["SystemRoot", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }

    let mut proc = spawn(SpawnSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_weles")),
        args: vec!["__test-child".into(), "--ignore-graceful".into()],
        env,
        cwd: Some(dir.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .expect("spawn __test-child fixture");

    // The fixture runs for 60s by construction, so this deadline is a
    // guaranteed timeout, not a race.
    let waited = wait_for_helper(&mut proc, Duration::from_millis(300)).expect("poll helper");
    assert!(waited.is_none(), "fixture must still be running at the deadline");

    let error = helper_timeout_failure(
        &mut proc,
        "fixture helper",
        Duration::from_millis(300),
        &out_path,
        &err_path,
    );
    let message = format!("{error:#}");
    assert!(
        message.contains("fixture helper did not finish"),
        "error must name the helper and the timeout, got: {message}"
    );
    assert!(
        message.contains(&out_path.display().to_string()),
        "error must name the stdout log, got: {message}"
    );
    assert!(
        message.contains(&err_path.display().to_string()),
        "error must name the stderr log, got: {message}"
    );

    // The timeout branch must leave NO live child behind.
    assert!(
        proc.try_wait().expect("try_wait after timeout branch").is_some(),
        "child must be dead after the timeout branch"
    );
    drop(proc);
    let _ = std::fs::remove_dir_all(&dir);
}

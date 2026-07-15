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
use weles::prep::{helper_timeout_failure, wait_for_helper};

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

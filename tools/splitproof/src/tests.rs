use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{extract_form_fields, fleet_liveness, Running};
use processctl::{OutputDestination, OwnedChild, ProcessGroupPolicy, SpawnSpec};

#[test]
fn form_extractor_decodes_minijinja_attributes_once() {
    let html = r#"<form><input type="hidden" name="_expected_state" value="{&quot;name&quot;:&quot;dev&amp;ops&quot;,&quot;literal&quot;:&quot;&amp;quot;&quot;,&quot;path&quot;:&quot;a&#x2f;b&quot;,&quot;quote&quot;:&quot;&#x27;&quot;,&quot;tag&quot;:&quot;&lt;&gt;&quot;}"></form>"#;

    assert_eq!(
        extract_form_fields(html),
        vec![(
            "_expected_state".to_string(),
            r#"{"name":"dev&ops","literal":"&quot;","path":"a/b","quote":"'","tag":"<>"}"#
                .to_string(),
        )],
    );
}

/// A trivial process that exits almost immediately, so `try_wait` observes it as dead
/// well within the test's own polling budget. Cross-platform per-OS command (no
/// dependency on any GameBackend binary being built).
#[cfg(windows)]
fn exit_soon_spec(cwd: &Path) -> SpawnSpec {
    let comspec =
        std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("C:/Windows/System32/cmd.exe"));
    SpawnSpec {
        label: "liveness-fixture".into(),
        executable: PathBuf::from(comspec),
        args: vec![OsString::from("/C"), OsString::from("exit 3")],
        env: BTreeMap::new(),
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

#[cfg(not(windows))]
fn exit_soon_spec(cwd: &Path) -> SpawnSpec {
    SpawnSpec {
        label: "liveness-fixture".into(),
        executable: PathBuf::from("/bin/sh"),
        args: vec![OsString::from("-c"), OsString::from("exit 3")],
        env: BTreeMap::new(),
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

/// The failing branch `fleet_liveness` exists to catch: a fleet child that has already
/// exited (a dead/finished process, standing in for a service that died after clearing
/// its health gate) must be reported by name, not silently treated as alive.
#[test]
fn fleet_liveness_reports_a_child_that_already_exited() {
    let cwd = std::env::temp_dir();
    let mut child = OwnedChild::spawn(exit_soon_spec(&cwd)).expect("spawn liveness fixture");

    // Wait for the fixture to actually finish before handing it to fleet_liveness —
    // the assertion under test is that fleet_liveness DETECTS a dead child, not that
    // it blocks until one dies.
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "liveness fixture did not exit in time");
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut fleet = vec![Running { name: "fixture-svc", child }];
    let dead = fleet_liveness(&mut fleet);

    assert_eq!(dead.len(), 1, "expected exactly one dead entry: {dead:?}");
    assert!(
        dead[0].contains("fixture-svc"),
        "detail must name the dead service: {dead:?}"
    );
}

/// A still-running child must NOT be reported dead (the positive control for the
/// negative-path test above).
#[test]
fn fleet_liveness_ignores_a_still_running_child() {
    let cwd = std::env::temp_dir();
    let sleep_spec = {
        let mut spec = exit_soon_spec(&cwd);
        #[cfg(windows)]
        {
            spec.args = vec![OsString::from("/C"), OsString::from("timeout /T 30 /NOBREAK >NUL")];
        }
        #[cfg(not(windows))]
        {
            spec.args = vec![OsString::from("-c"), OsString::from("sleep 30")];
        }
        spec
    };
    let child = OwnedChild::spawn(sleep_spec).expect("spawn sleeping fixture");
    let mut fleet = vec![Running { name: "sleeping-svc", child }];

    let dead = fleet_liveness(&mut fleet);
    assert!(dead.is_empty(), "still-running child reported dead: {dead:?}");

    // `fleet` drops here: OwnedChild's Drop force-kills the still-running fixture, so
    // the test leaves no orphaned process behind.
}

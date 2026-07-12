use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::state::StateFailurePoint;
use crate::{
    FleetState, FleetStatus, ManagedProcess, ManagedStatus, ProcessIdentity, StartMarker,
    StateStore, STATE_VERSION,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

#[test]
fn state_round_trip_is_versioned_and_contains_only_the_allowlisted_shape() {
    let dir = test_dir("roundtrip");
    let store = StateStore::new(dir.join("fleet.json"));
    let state = sample_state("run-1", FleetStatus::Running);
    store.write_atomic(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));

    let serialized = std::fs::read_to_string(store.path()).unwrap();
    assert!(serialized.contains(&format!("\"version\": {STATE_VERSION}")));
    for forbidden in [
        "DATABASE_URL",
        "environment",
        "password",
        "bearer",
        "token",
        "private_key",
        "secret-value",
    ] {
        assert!(!serialized.contains(forbidden), "state leaked {forbidden}");
    }
}

#[test]
fn invalid_or_secret_shaped_identifiers_are_rejected() {
    assert!(FleetState::new("postgres://secret", "split").is_err());
    assert!(FleetState::new("run-1", "split with password").is_err());
    assert!(ManagedProcess::new(
        "bad/label",
        identity(1),
        PathBuf::from("out"),
        PathBuf::from("err")
    )
    .is_err());
}

#[test]
fn unsupported_state_version_is_never_accepted() {
    let dir = test_dir("version");
    let path = dir.join("fleet.json");
    let mut value = serde_json::to_value(sample_state("run-1", FleetStatus::Running)).unwrap();
    value["version"] = serde_json::json!(STATE_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(StateStore::new(path).load().is_err());
}

#[test]
fn unknown_fields_and_oversized_state_are_rejected() {
    let dir = test_dir("closed-schema");
    let path = dir.join("fleet.json");
    let mut value = serde_json::to_value(sample_state("run-1", FleetStatus::Running)).unwrap();
    value["unexpected"] = serde_json::json!(true);
    write_private_test_file(&path, &serde_json::to_vec(&value).unwrap());
    assert!(StateStore::new(&path).load().is_err());

    write_private_test_file(
        &path,
        &vec![b' '; crate::state::MAX_STATE_BYTES as usize + 1],
    );
    assert!(StateStore::new(path).load().is_err());
}

#[test]
fn every_injected_atomic_write_failure_leaves_a_complete_old_or_new_state() {
    let dir = test_dir("failures");
    let path = dir.join("fleet.json");
    let old = sample_state("old-run", FleetStatus::Running);
    let new = sample_state("new-run", FleetStatus::Stopping);
    StateStore::new(&path).write_atomic(&old).unwrap();

    for point in [
        StateFailurePoint::CreateTemp,
        StateFailurePoint::SecureTemp,
        StateFailurePoint::Write,
        StateFailurePoint::Flush,
        StateFailurePoint::Replace,
        StateFailurePoint::SyncParent,
    ] {
        StateStore::new(&path).write_atomic(&old).unwrap();
        let failing = StateStore::failing(&path, point);
        assert!(
            failing.write_atomic(&new).is_err(),
            "{point:?} did not fail"
        );
        let recovered = StateStore::new(&path).load().unwrap().unwrap();
        if point == StateFailurePoint::SyncParent {
            assert_eq!(recovered, new);
        } else {
            assert_eq!(recovered, old);
        }
        let temp_count = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temp_count, 0, "{point:?} left a temp file");
    }
}

#[test]
fn initial_write_failure_never_exposes_partial_state() {
    for point in [
        StateFailurePoint::CreateTemp,
        StateFailurePoint::SecureTemp,
        StateFailurePoint::Write,
        StateFailurePoint::Flush,
        StateFailurePoint::Replace,
        StateFailurePoint::SyncParent,
    ] {
        let dir = test_dir(&format!("initial-{point:?}"));
        let path = dir.join("fleet.json");
        let state = sample_state("new-run", FleetStatus::Starting);
        assert!(StateStore::failing(&path, point)
            .write_atomic(&state)
            .is_err());
        if point == StateFailurePoint::SyncParent {
            assert_eq!(StateStore::new(&path).load().unwrap(), Some(state));
        } else {
            assert!(!path.exists(), "{point:?} exposed a partial destination");
        }
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0,
            "{point:?} left a temp file"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn state_file_is_owner_read_write_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = test_dir("mode");
    let store = StateStore::new(dir.join("fleet.json"));
    store
        .write_atomic(&sample_state("run-1", FleetStatus::Running))
        .unwrap();
    assert_eq!(
        std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(target_os = "linux")]
#[test]
fn state_load_and_replace_reject_symlinks_directories_and_insecure_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = test_dir("path-hardening");
    let target = dir.join("target.json");
    write_private_test_file(
        &target,
        &serde_json::to_vec(&sample_state("target", FleetStatus::Running)).unwrap(),
    );
    let link = dir.join("fleet.json");
    symlink(&target, &link).unwrap();
    let store = StateStore::new(&link);
    assert!(store.load().is_err());
    assert!(store
        .write_atomic(&sample_state("replacement", FleetStatus::Running))
        .is_err());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&target).unwrap()).unwrap()
            ["run_id"],
        "target"
    );

    std::fs::remove_file(&link).unwrap();
    std::fs::create_dir(&link).unwrap();
    assert!(store.load().is_err());
    std::fs::remove_dir(&link).unwrap();
    write_private_test_file(
        &link,
        &serde_json::to_vec(&sample_state("insecure", FleetStatus::Running)).unwrap(),
    );
    std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().is_err());
    assert!(store
        .write_atomic(&sample_state("replacement", FleetStatus::Running))
        .is_err());
}

#[cfg(windows)]
#[test]
fn state_file_has_one_protected_owner_only_dacl_entry() {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAclInformation, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
    };

    let dir = test_dir("acl");
    let store = StateStore::new(dir.join("fleet.json"));
    store
        .write_atomic(&sample_state("run-1", FleetStatus::Running))
        .unwrap();
    let path: Vec<u16> = store
        .path()
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        },
        0
    );
    let mut info: ACL_SIZE_INFORMATION = unsafe { zeroed() };
    assert_ne!(
        unsafe {
            GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        },
        0
    );
    unsafe { LocalFree(descriptor as _) };
    assert_eq!(info.AceCount, 1);
}

fn sample_state(run_id: &str, status: FleetStatus) -> FleetState {
    let mut state = FleetState::new(run_id, "split").unwrap();
    state.set_status(status);
    state.set_control_endpoint(Some(PathBuf::from("run/control.sock")));
    let mut process = ManagedProcess::new(
        "accounts-svc",
        identity(42),
        PathBuf::from("run/accounts.out.log"),
        PathBuf::from("run/accounts.err.log"),
    )
    .unwrap();
    process.set_status(ManagedStatus::Healthy);
    state.push_process(process);
    state
}

fn identity(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        executable: PathBuf::from("target/debug/fake-service"),
        started: StartMarker(1234),
    }
}

fn write_private_test_file(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn test_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "processctl-state-{name}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

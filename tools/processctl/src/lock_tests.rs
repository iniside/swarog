use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::lock::validate_credential;
use crate::{rollout_lock_path, LeaseError, RolloutLock};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

#[test]
fn concurrent_owner_is_rejected_and_release_allows_next_owner() {
    let path = lock_path("concurrent");
    let owner = RolloutLock::acquire(&path, "run-1", ["splitproof"]).unwrap();
    assert!(matches!(
        RolloutLock::acquire(&path, "run-2", ["splitproof"]),
        Err(LeaseError::AlreadyOwned)
    ));
    drop(owner);
    RolloutLock::acquire(&path, "run-2", ["splitproof"]).unwrap();
}

#[test]
fn devctl_exclusive_and_splitproof_lendable_contend_on_canonical_path() {
    let dir = lock_path("cross-tool")
        .parent()
        .expect("lock fixture parent")
        .to_path_buf();
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let path = rollout_lock_path(&dir);
    assert_eq!(path, run.join("rollout.lock"));

    let exclusive = RolloutLock::acquire_exclusive(&path, "devctl-run").unwrap();
    assert!(exclusive.allowed_borrower_roles().is_empty());
    assert!(matches!(
        RolloutLock::acquire(&path, "splitproof-run", ["splitproof"]),
        Err(LeaseError::AlreadyOwned)
    ));
    drop(exclusive);

    // verifyctl's real lease shape (runner.rs:57): one lease, two roles.
    let lendable = RolloutLock::acquire(&path, "verify-run", ["splitproof", "weles"]).unwrap();
    assert_eq!(
        lendable
            .allowed_borrower_roles()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["splitproof", "weles"]
    );
    assert!(matches!(
        RolloutLock::acquire_exclusive(&path, "devctl-run"),
        Err(LeaseError::AlreadyOwned)
    ));
}

#[test]
fn one_lease_serves_each_of_its_roles_in_turn() {
    // THE property this lease exists to have, and the one it lacked: verifyctl
    // holds ONE lease for its whole manifest and must lend it to splitproof AND
    // (later) to weles. Before the role set + per-role marker, the second
    // borrow was refused outright.
    let path = lock_path("two-roles");
    let owner = RolloutLock::acquire(&path, "verify-run", ["splitproof", "weles"]).unwrap();

    let first = validate_credential(owner.credential_for_test(), "splitproof").unwrap();
    assert_eq!(first.run_id(), "verify-run");
    drop(first);

    // Same lease, same credential, a DIFFERENT role — must not be a replay.
    let second = validate_credential(owner.credential_for_test(), "weles").unwrap();
    assert_eq!(second.run_id(), "verify-run");
    assert_eq!(second.owner(), owner.owner());
    drop(second);

    // Each role burned its OWN one-shot, and only its own.
    for role in ["splitproof", "weles"] {
        assert!(matches!(
            validate_credential(owner.credential_for_test(), role),
            Err(LeaseError::BorrowerReplay)
        ));
    }

    // A role the lease never named is refused, even though borrowing is on.
    assert!(matches!(
        validate_credential(owner.credential_for_test(), "devctl"),
        Err(LeaseError::WrongRole { expected, received })
            if expected == "splitproof, weles" && received == "devctl"
    ));

    // And the owner reaps EVERY role's marker — no litter, whatever the
    // borrowers did or did not do on their way out.
    let markers = || borrowed_markers(&path);
    assert_eq!(markers().len(), 2, "both roles' markers exist while the lease lives");
    drop(owner);
    assert!(
        markers().is_empty(),
        "OwnedLease::drop must reap every role's marker, not just one"
    );
}

#[test]
fn a_foreign_version_is_refused_by_version_not_by_field_shape() {
    // `weles deploy` stages binaries that may lag the tree, so a v1 credential
    // meeting this v2 processctl is real. Both the credential and the metadata
    // are `deny_unknown_fields`, so a typed parse first would report whichever
    // field v2 renamed and the version gate would never run. The gate therefore
    // reads `version` off the raw bytes BEFORE the typed parse — this pins that
    // ordering, on the v1 shape as it actually was (singular
    // `allowed_borrower_role`, `nonce`).
    let v1 = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "lock_path": "/tmp/run/rollout.lock",
        "metadata": {
            "version": 1,
            "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
            "run_id": "abc",
            "lease_started_unix_nanos": 5,
            "allowed_borrower_role": "splitproof"
        },
        "nonce": vec![0u8; 32]
    }))
    .unwrap();
    assert!(
        matches!(
            crate::lock::credential_from_bytes(&v1, "splitproof"),
            Err(LeaseError::UnsupportedVersion(1))
        ),
        "a v1 credential must refuse by VERSION — a Serialize error would mean the gate is dead"
    );

    // A future version is the same story from the other side.
    let v3 = serde_json::to_vec(&serde_json::json!({
        "version": 3,
        "lock_path": "/tmp/run/rollout.lock",
        "metadata": {
            "version": 3,
            "a_field_a_later_processctl_grew": true
        }
    }))
    .unwrap();
    assert!(matches!(
        crate::lock::credential_from_bytes(&v3, "splitproof"),
        Err(LeaseError::UnsupportedVersion(3))
    ));
}

#[test]
fn a_lease_with_no_roles_lends_to_nobody() {
    let path = lock_path("no-roles");
    let owner = RolloutLock::acquire(&path, "run-1", Vec::<String>::new()).unwrap();
    assert!(owner.allowed_borrower_roles().is_empty());
    assert!(matches!(
        validate_credential(owner.credential_for_test(), "splitproof"),
        Err(LeaseError::WrongRole { expected, .. }) if expected == "<borrowing-disabled>"
    ));
    assert!(borrowed_markers(&path).is_empty(), "a refused borrow claims nothing");
}

/// The live lease's start stamp, read back from the metadata it wrote — the
/// half of the marker name a test cannot hardcode.
fn lease_nanos(lock: &std::path::Path) -> u64 {
    let contents = std::fs::read_to_string(lock).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&contents).unwrap();
    metadata["lease_started_unix_nanos"].as_u64().unwrap()
}

/// Every one-shot marker sitting beside `lock` right now.
fn borrowed_markers(lock: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(lock.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "borrowed")
        })
        .collect()
}

#[test]
fn borrower_role_one_shot_replay_and_dead_owner_are_fail_closed() {
    let path = lock_path("borrow");
    let owner = RolloutLock::acquire(&path, "run-1", ["splitproof"]).unwrap();
    let credential = owner.credential_for_test();
    assert!(matches!(
        validate_credential(credential.clone(), "wrong-role"),
        Err(LeaseError::WrongRole { expected, received })
            if expected == "splitproof" && received == "wrong-role"
    ));
    let borrowed = validate_credential(credential.clone(), "splitproof").unwrap();
    assert_eq!(borrowed.run_id(), "run-1");
    assert_eq!(borrowed.owner(), owner.owner());
    let marker = borrowed_markers(&path).pop().expect("borrow marker");
    assert_eq!(
        marker.file_name().unwrap().to_string_lossy(),
        format!(".rollout.lock.run-1.{}.splitproof.borrowed", lease_nanos(&path)),
        "the marker is keyed by role — the owner deletes exactly this name"
    );
    // The owner's cleanup only deletes a marker carrying exactly these bytes.
    assert_eq!(std::fs::read(&marker).unwrap(), b"processctl-borrowed-v1\n");
    assert!(matches!(
        validate_credential(credential, "splitproof"),
        Err(LeaseError::BorrowerReplay)
    ));
    drop(borrowed);
    let observed_closed_before_cleanup = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&observed_closed_before_cleanup);
    let successor_path = path.clone();
    let marker_during_hook = marker.clone();
    crate::lock::install_owner_drop_hook(path.clone(), move || {
        assert!(marker_during_hook.exists());
        let successor = RolloutLock::acquire(&successor_path, "successor", ["splitproof"]).unwrap();
        observed.store(true, Ordering::SeqCst);
        drop(successor);
    });
    drop(owner);
    assert!(observed_closed_before_cleanup.load(Ordering::SeqCst));
    assert!(!marker.exists());

    let dead_path = lock_path("dead");
    let dead_owner = RolloutLock::acquire(&dead_path, "run-dead", ["splitproof"]).unwrap();
    let dead_credential = dead_owner.credential_for_test();
    drop(dead_owner);
    assert!(matches!(
        validate_credential(dead_credential, "splitproof"),
        Err(LeaseError::OwnerNotLive)
    ));
}

#[test]
fn metadata_is_private_and_contains_no_credential_or_secret_fields() {
    let path = lock_path("metadata");
    let _owner = RolloutLock::acquire(&path, "run-public", ["splitproof"]).unwrap();
    let metadata = std::fs::read_to_string(&path).unwrap();
    for forbidden in [
        "nonce",
        "credential",
        "environment",
        "DATABASE_URL",
        "password",
        "token",
        "private_key",
    ] {
        assert!(
            !metadata.contains(forbidden),
            "lock metadata leaked {forbidden}"
        );
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[cfg(windows)]
    assert_owner_only_dacl(&path);
}

#[cfg(target_os = "linux")]
#[test]
fn lock_rejects_symlinks_directories_and_insecure_existing_files() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let path = lock_path("path-hardening");
    let target = path.with_file_name("target.lock");
    std::fs::write(&target, b"do-not-touch").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &path).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", ["splitproof"]).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do-not-touch");

    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", ["splitproof"]).is_err());
    std::fs::remove_dir(&path).unwrap();

    std::fs::write(&path, b"insecure").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", ["splitproof"]).is_err());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[cfg(windows)]
#[test]
fn lock_rejects_reparse_directories_insecure_acl_and_delete_sharing() {
    let path = lock_path("windows-path-hardening");
    std::fs::create_dir(&path).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", ["splitproof"]).is_err());
    std::fs::remove_dir(&path).unwrap();

    std::fs::write(&path, b"insecure-lock").unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", ["splitproof"]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"insecure-lock");
    std::fs::remove_file(&path).unwrap();

    let owner = RolloutLock::acquire(&path, "run-1", ["splitproof"]).unwrap();
    assert!(std::fs::remove_file(&path).is_err());
    drop(owner);
    std::fs::remove_file(&path).unwrap();

    let target = path.with_file_name("target.lock");
    let target_owner = RolloutLock::acquire(&target, "target", ["splitproof"]).unwrap();
    drop(target_owner);
    if std::os::windows::fs::symlink_file(&target, &path).is_ok() {
        assert!(RolloutLock::acquire(&path, "replacement", ["splitproof"]).is_err());
    }
}

#[cfg(windows)]
fn assert_owner_only_dacl(path: &std::path::Path) {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAclInformation, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
    };
    crate::state::validate_private_test_path(path).unwrap();
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
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

fn lock_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "processctl-lock-{name}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("rollout.lock")
}

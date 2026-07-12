use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::lock::validate_credential;
use crate::{LeaseError, RolloutLock};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

#[test]
fn concurrent_owner_is_rejected_and_release_allows_next_owner() {
    let path = lock_path("concurrent");
    let owner = RolloutLock::acquire(&path, "run-1", "splitproof").unwrap();
    assert!(matches!(
        RolloutLock::acquire(&path, "run-2", "splitproof"),
        Err(LeaseError::AlreadyOwned)
    ));
    drop(owner);
    RolloutLock::acquire(&path, "run-2", "splitproof").unwrap();
}

#[test]
fn borrower_role_one_shot_replay_and_dead_owner_are_fail_closed() {
    let path = lock_path("borrow");
    let owner = RolloutLock::acquire(&path, "run-1", "splitproof").unwrap();
    let credential = owner.credential_for_test();
    assert!(matches!(
        validate_credential(credential.clone(), "wrong-role"),
        Err(LeaseError::WrongRole { expected, received })
            if expected == "splitproof" && received == "wrong-role"
    ));
    let borrowed = validate_credential(credential.clone(), "splitproof").unwrap();
    assert_eq!(borrowed.run_id(), "run-1");
    assert_eq!(borrowed.owner(), owner.owner());
    let marker = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "borrowed")
        })
        .expect("borrow marker");
    assert_ne!(std::fs::read(&marker).unwrap(), vec![7; 32]);
    assert!(matches!(
        validate_credential(credential, "splitproof"),
        Err(LeaseError::BorrowerReplay)
    ));
    drop(borrowed);
    drop(owner);
    assert!(!marker.exists());

    let dead_path = lock_path("dead");
    let dead_owner = RolloutLock::acquire(&dead_path, "run-dead", "splitproof").unwrap();
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
    let _owner = RolloutLock::acquire(&path, "run-public", "splitproof").unwrap();
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
    assert!(RolloutLock::acquire(&path, "run-1", "splitproof").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do-not-touch");

    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", "splitproof").is_err());
    std::fs::remove_dir(&path).unwrap();

    std::fs::write(&path, b"insecure").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", "splitproof").is_err());
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
    assert!(RolloutLock::acquire(&path, "run-1", "splitproof").is_err());
    std::fs::remove_dir(&path).unwrap();

    std::fs::write(&path, b"insecure-lock").unwrap();
    assert!(RolloutLock::acquire(&path, "run-1", "splitproof").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"insecure-lock");
    std::fs::remove_file(&path).unwrap();

    let owner = RolloutLock::acquire(&path, "run-1", "splitproof").unwrap();
    assert!(std::fs::remove_file(&path).is_err());
    drop(owner);
    std::fs::remove_file(&path).unwrap();

    let target = path.with_file_name("target.lock");
    let target_owner = RolloutLock::acquire(&target, "target", "splitproof").unwrap();
    drop(target_owner);
    if std::os::windows::fs::symlink_file(&target, &path).is_ok() {
        assert!(RolloutLock::acquire(&path, "replacement", "splitproof").is_err());
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

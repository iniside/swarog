use super::*;

fn temp_root(name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "weles-lock-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create test temp dir");
    dir
}

#[test]
fn acquire_writes_metadata_and_excludes_a_second_acquire() {
    let root = temp_root("exclusive");
    let lock = acquire(&root, "run-a").expect("first acquire");
    assert_eq!(lock.path(), root.join("run").join("rollout.lock"));

    let contents =
        std::fs::read_to_string(lock.path()).expect("read lock metadata while holding the lock");
    let metadata: serde_json::Value =
        serde_json::from_str(&contents).expect("lock metadata is valid JSON");
    assert_eq!(metadata["version"], 1);
    assert_eq!(metadata["tool"], "weles");
    assert_eq!(metadata["pid"], std::process::id());
    assert_eq!(metadata["run_id"], "run-a");
    assert!(metadata["started_unix"].as_u64().expect("started_unix is a number") > 0);

    // A second acquire opens a SECOND handle in the same process: flock is
    // per open-file-description and LockFileEx per handle, so this contends
    // exactly like a foreign process would (cross-process compatibility with
    // devctl itself is proven live in M0 Step 7).
    let error = acquire(&root, "run-b").expect_err("second acquire must fail while held");
    let message = format!("{error:#}");
    assert!(
        message.contains("another rollout owns"),
        "AlreadyLocked must be loud and name the owner situation, got: {message}"
    );
    assert!(
        message.contains("rollout.lock"),
        "AlreadyLocked must name the lock path, got: {message}"
    );

    // The failed acquire must NOT have clobbered the holder's metadata.
    let contents_after = std::fs::read_to_string(lock.path()).expect("re-read lock metadata");
    assert_eq!(contents, contents_after, "a losing acquire must not touch the metadata");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn drop_releases_the_lock_for_reacquire() {
    let root = temp_root("release");
    let first = acquire(&root, "one").expect("first acquire");
    drop(first);
    let second = acquire(&root, "two").expect("re-acquire after drop must succeed");
    let contents = std::fs::read_to_string(second.path()).expect("read metadata");
    let metadata: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    assert_eq!(metadata["run_id"], "two", "re-acquire rewrites the metadata");
    drop(second);
    let _ = std::fs::remove_dir_all(&root);
}

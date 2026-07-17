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

// ---------------------------------------------------------------------------
// The borrowed half.
//
// A staged owner is a REAL owner: this test process observes its own identity
// and holds the lock through a second handle. flock is per open-file-description
// and LockFileEx per handle, so `is_locked_by_other` sees it exactly as it would
// see verifyctl's (the same property `acquire_writes_metadata_and_excludes_a_
// second_acquire` above rests on). Everything below therefore exercises
// `validate_credential` against a live world, not a mock of one.
//
// `consume_inherited`'s stdin/argv plumbing is NOT reachable from a unit test
// (stdin is a process-global, and cargo owns this process's argv) — its two
// decisions that ARE reachable are covered: no argv marker => Ok(None), and the
// role charset. The rest is pinned live by the Step 6 stage.
// ---------------------------------------------------------------------------

/// A parent lease, staged the way processctl writes one.
struct StagedOwner {
    root: PathBuf,
    lock_path: PathBuf,
    held: File,
    metadata: OwnerLease,
}

impl StagedOwner {
    /// `roles` is the lease's permitted-role SET, exactly as processctl v2
    /// writes it; empty means borrowing is disabled.
    fn new(name: &str, roles: &[&str]) -> Self {
        let root = temp_root(name);
        std::fs::create_dir_all(root.join("run")).expect("create run dir");
        let lock_path = root.join("run").join("rollout.lock");

        let metadata = OwnerLease {
            version: OWNER_LEASE_VERSION,
            owner: imp::observe_process_identity(std::process::id())
                .expect("observe this process's own identity"),
            run_id: "parent-run".into(),
            lease_started_unix_nanos: 42,
            allowed_borrower_roles: roles.iter().map(|role| role.to_string()).collect(),
        };

        let mut held = imp::open_lock_file(&lock_path).expect("open the staged lock");
        assert!(
            imp::try_lock_exclusive(&held).expect("lock the staged lock"),
            "the staged owner must actually hold the lock"
        );
        let bytes = serde_json::to_vec_pretty(&metadata).expect("serialize owner metadata");
        held.write_all(&bytes).expect("write owner metadata");
        held.flush().expect("flush owner metadata");

        Self {
            root,
            lock_path,
            held,
            metadata,
        }
    }

    fn credential(&self) -> BorrowCredential {
        BorrowCredential {
            version: OWNER_LEASE_VERSION,
            lock_path: self.lock_path.clone(),
            metadata: self.metadata.clone(),
        }
    }

    /// Byte-for-byte what is on disk right now — the thing `acquire` would
    /// destroy (it truncates and rewrites with weles's own schema).
    fn on_disk(&self) -> String {
        std::fs::read_to_string(&self.lock_path).expect("read staged lock metadata")
    }
}

impl Drop for StagedOwner {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn acquire_calls() -> usize {
    ACQUIRE_CALLS.with(|calls| calls.get())
}

#[test]
fn a_valid_borrow_never_re_acquires_the_lock() {
    let owner = StagedOwner::new("borrow-valid", &[BORROWER_ROLE]);
    let before = owner.on_disk();

    let borrowed = validate_credential(owner.credential(), BORROWER_ROLE).expect("valid borrow");
    assert_eq!(borrowed.run_id(), "parent-run", "the borrow carries the OWNER's run id");
    assert_eq!(borrowed.owner_pid(), std::process::id());

    // (1) The acquire path was never entered. A counter, not an absence of
    // errors: `acquire` is the only thing that could take the lock or wipe the
    // parent's metadata, and it was not called.
    assert_eq!(
        acquire_calls(),
        0,
        "a borrow must not fall through to acquire — that is the deadlock against the parent"
    );
    // (2) The parent's metadata survives untouched. `acquire` truncates and
    // rewrites this with {tool:"weles",...}; this content proves it did not run.
    assert_eq!(owner.on_disk(), before, "a borrower must not rewrite the owner's metadata");
    assert!(
        !before.contains("\"tool\""),
        "sanity: `tool` is the discriminator of weles's OWN acquire schema — the staged parent \
         metadata must not already carry it, or assertion (2) would prove nothing"
    );
    // (3) The parent still holds the lock — the borrower took nothing, and
    // `is_locked_by_other`'s probe (which momentarily TRIES the lock) left no
    // trace.
    let contender = imp::open_lock_file(&owner.lock_path).expect("open a third handle");
    assert!(
        !imp::try_lock_exclusive(&contender).expect("probe the lock"),
        "the parent's lock must still be held after a borrow"
    );
}

#[test]
fn a_borrow_naming_a_different_role_is_refused() {
    // A lease that permits borrowing, but not by US: membership in the set is
    // the authority, so a non-empty set that omits `weles` is still a refusal.
    let owner = StagedOwner::new("borrow-role", &["splitproof"]);
    let error = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect_err("a lease minted for another role must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("role mismatch") && message.contains("splitproof"),
        "the refusal must name both roles, got: {message}"
    );
    assert_eq!(acquire_calls(), 0, "a refused borrow must not fall through to acquire");
}

#[test]
fn weles_borrows_the_lease_verifyctl_actually_mints() {
    // verifyctl's live lease (runner.rs:57) names BOTH roles. This is the
    // shape weles meets in the Step 6 stage: `splitproof` is present alongside
    // `weles`, and weles's claim must succeed anyway — set membership, not
    // equality with a single permitted role (which is what v1 required, and
    // why this borrow was impossible).
    let owner = StagedOwner::new("borrow-verifyctl-shape", &["splitproof", "weles"]);
    let borrowed = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect("weles must borrow a lease that also permits splitproof");
    assert_eq!(borrowed.run_id(), "parent-run");
    assert_eq!(acquire_calls(), 0, "a borrow must never reach acquire");

    // weles claimed only ITS OWN one-shot: splitproof's borrow of the same
    // lease is untouched, which is exactly what the per-role key buys.
    assert!(borrow_marker_path(&owner.lock_path, &owner.metadata, BORROWER_ROLE).exists());
    assert!(
        !borrow_marker_path(&owner.lock_path, &owner.metadata, "splitproof").exists(),
        "weles's borrow must not consume splitproof's one-shot on the same lease"
    );
}

#[test]
fn a_borrow_from_a_lease_that_forbids_borrowing_is_refused() {
    let owner = StagedOwner::new("borrow-disabled", &[]);
    let error = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect_err("a lease that permits no borrower must be refused");
    assert!(
        format!("{error:#}").contains("borrowing-disabled"),
        "got: {error:#}"
    );
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn a_borrow_whose_parent_identity_is_absent_is_refused() {
    let owner = StagedOwner::new("borrow-dead", &[BORROWER_ROLE]);
    let mut credential = owner.credential();
    // A pid above every OS's pid_max: observably not a live process, on either
    // platform, without racing a real one.
    credential.metadata.owner.pid = u32::MAX - 1;
    // The lock file must agree, or the metadata check would fire first and we
    // would not reach the identity branch at all.
    std::fs::write(
        &owner.lock_path,
        serde_json::to_vec_pretty(&credential.metadata).expect("serialize"),
    )
    .expect("restage owner metadata");

    let error = validate_credential(credential, BORROWER_ROLE)
        .expect_err("a borrow from an absent parent must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("is not live"),
        "the refusal must say the owner is gone, got: {message}"
    );
    assert_eq!(acquire_calls(), 0, "a borrower that cannot validate its parent must die, not acquire");
    // A refusal must not have BURNED the one-shot on its way out: the marker is
    // claimed last, strictly after every check. Were that order reversed, a
    // transient refusal would permanently poison a lease that is otherwise fine.
    assert!(
        !borrow_marker_path(&owner.lock_path, &owner.metadata, BORROWER_ROLE).exists(),
        "a refused borrow must not consume the lease's one-shot marker"
    );
}

#[test]
fn a_borrow_whose_parent_identity_is_wrong_is_refused() {
    let owner = StagedOwner::new("borrow-recycled", &[BORROWER_ROLE]);
    let mut credential = owner.credential();
    // The pid IS live (it is us) but it is NOT the process that took the lease:
    // the recycled-pid case, which pid alone could never catch.
    credential.metadata.owner.started = StartMarker(credential.metadata.owner.started.0 ^ 1);
    std::fs::write(
        &owner.lock_path,
        serde_json::to_vec_pretty(&credential.metadata).expect("serialize"),
    )
    .expect("restage owner metadata");

    let error = validate_credential(credential, BORROWER_ROLE)
        .expect_err("a borrow naming a different process must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("DIFFERENT process") && message.contains("recycled"),
        "got: {message}"
    );
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn a_borrow_whose_credential_disagrees_with_the_lock_file_is_refused() {
    let owner = StagedOwner::new("borrow-stale", &[BORROWER_ROLE]);
    let mut credential = owner.credential();
    credential.metadata.run_id = "a-lease-that-has-since-been-replaced".into();

    let error = validate_credential(credential, BORROWER_ROLE)
        .expect_err("a credential the lock file no longer backs must be refused");
    assert!(
        format!("{error:#}").contains("no longer carries the lease"),
        "got: {error:#}"
    );
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn a_borrow_is_refused_once_the_parent_has_released_the_lock() {
    // The parent identity is live and the metadata matches — only the LEASE is
    // over. Identity alone is not the lease; this is the branch that says so.
    let owner = StagedOwner::new("borrow-unlocked", &[BORROWER_ROLE]);
    imp::unlock(&owner.held).expect("release the staged owner's lock");

    let error = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect_err("a borrow of a lease nobody holds must be refused");
    assert!(
        format!("{error:#}").contains("no longer holds"),
        "got: {error:#}"
    );
    assert_eq!(acquire_calls(), 0, "an expired lease must not become an acquire");
}

#[test]
fn a_borrow_is_one_shot_within_its_own_role() {
    let owner = StagedOwner::new("borrow-once", &[BORROWER_ROLE]);
    let first = validate_credential(owner.credential(), BORROWER_ROLE).expect("first borrow");

    let error = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect_err("the same lease must not be borrowable twice as the same role");
    assert!(
        format!("{error:#}").contains("already been borrowed once"),
        "got: {error:#}"
    );

    // The marker's NAME is processctl's, because the owner deletes exactly this
    // path (for every role in its set) when its lease drops; the CONTENTS are
    // processctl's, because the owner re-reads and compares them before
    // deleting. The `.weles.` segment is the per-role key.
    let marker = borrow_marker_path(&owner.lock_path, &owner.metadata, BORROWER_ROLE);
    assert_eq!(
        marker.file_name().expect("marker file name").to_string_lossy(),
        ".rollout.lock.parent-run.42.weles.borrowed"
    );
    assert_eq!(
        std::fs::read(&marker).expect("read the one-shot marker"),
        b"processctl-borrowed-v1\n",
        "processctl's owner-side cleanup deletes the marker ONLY if it reads back exactly this"
    );
    drop(first);
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn a_credential_of_an_unknown_version_is_refused() {
    let owner = StagedOwner::new("borrow-version", &[BORROWER_ROLE]);
    for version in [OWNER_LEASE_VERSION - 1, OWNER_LEASE_VERSION + 1] {
        let mut credential = owner.credential();
        credential.version = version;
        let error = validate_credential(credential, BORROWER_ROLE)
            .expect_err("an unknown wire version must be refused, never guessed at");
        assert!(
            format!("{error:#}").contains(&format!("unsupported rollout lease version {version}")),
            "got: {error:#}"
        );
    }
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn a_v1_credential_meeting_this_v2_weles_refuses_by_version_not_by_parse_error() {
    // `weles deploy` stages binaries that may lag the tree, so a v1 credential
    // meeting a v2 weles is a real pairing, not a hypothetical. The refusal must
    // name the VERSION — that is the whole justification for bumping it. Both
    // structs are `deny_unknown_fields`, so a typed parse first would report
    // whichever field v2 renamed and the version gate would never run; the gate
    // therefore reads `version` off the raw bytes BEFORE the typed parse.
    //
    // The v1 shape (singular `allowed_borrower_role`, `nonce`) is spelled out
    // literally: weles deliberately no longer has a type for it.
    let v1_credential = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "lock_path": "/tmp/run/rollout.lock",
        "metadata": {
            "version": 1,
            "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
            "run_id": "abc",
            "lease_started_unix_nanos": 5,
            "allowed_borrower_role": "weles"
        },
        "nonce": vec![0u8; 32]
    }))
    .expect("serialize a v1 credential");

    let error = credential_from_bytes(&v1_credential, BORROWER_ROLE)
        .expect_err("a v1 credential must not be borrowed by a v2 weles");
    let message = format!("{error:#}");
    assert!(
        message.contains("unsupported rollout lease version 1"),
        "the refusal must name the VERSION, not a field shape — that is what the bump buys. \
         got: {message}"
    );

    // And the owner-side twin: a v1 lease sitting in the lock file, met by a
    // credential this build CAN hold. Same gate, other entry point.
    let owner = StagedOwner::new("borrow-v1-lease", &[BORROWER_ROLE]);
    std::fs::write(
        &owner.lock_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
            "run_id": "abc",
            "lease_started_unix_nanos": 5,
            "allowed_borrower_role": "weles"
        }))
        .expect("serialize a v1 lease"),
    )
    .expect("stage a v1 lease");
    let error = validate_credential(owner.credential(), BORROWER_ROLE)
        .expect_err("a v1 lease must not be borrowed by a v2 weles");
    let message = format!("{error:#}");
    assert!(
        message.contains("unsupported rollout lease version 1"),
        "got: {message}"
    );
    assert_eq!(acquire_calls(), 0);
}

#[test]
fn the_credential_wire_shape_is_processctls() {
    // Zero-sharing means this shape exists twice, hand-copied. Pin the exact
    // JSON weles must be able to read; the producer is
    // `processctl::lock::BorrowCredential` + its `LockMetadata`/`ProcessIdentity`
    // (deny_unknown_fields on both sides, so any drift is a loud refusal).
    // This is verifyctl's real lease on the wire: version 2, a role SET holding
    // both borrowers, and no `nonce`.
    let json = serde_json::json!({
        "version": 2,
        "lock_path": "/tmp/run/rollout.lock",
        "metadata": {
            "version": 2,
            "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
            "run_id": "abc",
            "lease_started_unix_nanos": 5,
            "allowed_borrower_roles": ["splitproof", "weles"]
        }
    });
    let credential: BorrowCredential =
        serde_json::from_value(json).expect("weles must parse processctl's credential shape");
    assert_eq!(credential.metadata.owner.started, StartMarker(99));
    assert!(credential.metadata.allowed_borrower_roles.contains("weles"));
    assert!(credential.metadata.allowed_borrower_roles.contains("splitproof"));

    // A lease that permits no borrower is an EMPTY array, not `null` and not an
    // absent field (processctl's `acquire_exclusive` serializes a BTreeSet).
    let disabled: OwnerLease = serde_json::from_value(serde_json::json!({
        "version": 2,
        "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
        "run_id": "abc",
        "lease_started_unix_nanos": 5,
        "allowed_borrower_roles": []
    }))
    .expect("parse a borrowing-disabled lease");
    assert!(disabled.allowed_borrower_roles.is_empty());

    // Fail-closed on drift rather than guess.
    let drifted = serde_json::from_value::<OwnerLease>(serde_json::json!({
        "version": 2,
        "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
        "run_id": "abc",
        "lease_started_unix_nanos": 5,
        "allowed_borrower_roles": ["weles"],
        "a_field_processctl_grew_later": true
    }));
    assert!(drifted.is_err(), "an unknown field must refuse, not be ignored");

    // The v1 field name is now drift like any other — not a silently tolerated
    // legacy alias.
    let v1_shape = serde_json::from_value::<OwnerLease>(serde_json::json!({
        "version": 2,
        "owner": { "pid": 17, "executable": "/tmp/verifyctl", "started": 99 },
        "run_id": "abc",
        "lease_started_unix_nanos": 5,
        "allowed_borrower_role": "weles"
    }));
    assert!(v1_shape.is_err(), "the v1 singular field must not parse as v2");
}

#[test]
fn without_a_borrow_in_the_environment_weles_acquires_exactly_as_before() {
    // cargo's argv carries no `--processctl-borrowed-lease-v1`, so this is the
    // real operator path, not a simulation of it.
    assert!(
        borrow_inherited_if_present(BORROWER_ROLE)
            .expect("no marker in argv must not be an error")
            .is_none(),
        "an operator-launched weles must find no borrow — and must not touch stdin looking for one"
    );

    let root = temp_root("no-borrow");
    let before = acquire_calls();
    let lease = acquire_or_borrow(&root, "run-a").expect("acquire_or_borrow with no borrow");
    assert!(matches!(lease, Lease::Owned(_)), "no borrow => the operator path");
    assert_eq!(acquire_calls(), before + 1, "acquire_or_borrow must reach acquire exactly once");

    let Lease::Owned(lock) = &lease else { unreachable!() };
    let contents = std::fs::read_to_string(lock.path()).expect("read metadata");
    let metadata: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    assert_eq!(metadata["tool"], "weles", "unchanged from today: weles's own schema");
    assert_eq!(metadata["run_id"], "run-a");
    assert!(
        acquire(&root, "run-b").is_err(),
        "unchanged from today: the lock is really held"
    );

    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_borrow_that_failed_to_validate_never_becomes_an_acquire() {
    // THE fall-through this whole step exists to forbid. `acquire_or_borrow`
    // cannot be driven here (cargo owns this process's argv and stdin), so the
    // decision is exercised through `lease_from` — which production calls
    // unconditionally — with the outcome production would have handed it.
    let root = temp_root("no-fallthrough");
    let before = acquire_calls();

    let error = lease_from(
        Err(anyhow::anyhow!("the parent could not be validated")),
        &root,
        "run-a",
    )
    .expect_err("a borrow that failed to validate must not yield a lease");
    assert!(format!("{error:#}").contains("could not be validated"), "got: {error:#}");
    assert_eq!(
        acquire_calls(),
        before,
        "a borrower that cannot validate its parent must DIE, not acquire — acquiring here would \
         deadlock against the very lease it failed to borrow"
    );
    assert!(
        !root.join("run").join("rollout.lock").exists(),
        "and it must not have touched the lock file at all"
    );

    // Same function, `Ok(None)`: this is the arm that MAY acquire, so the
    // assertion above is about the Err arm specifically and not about
    // `lease_from` being inert.
    let lease = lease_from(Ok(None), &root, "run-a").expect("no borrow => acquire");
    assert!(matches!(lease, Lease::Owned(_)));
    assert_eq!(acquire_calls(), before + 1);
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_validated_borrow_yields_a_borrowed_lease_without_acquiring() {
    let owner = StagedOwner::new("lease-from-borrowed", &[BORROWER_ROLE]);
    let borrowed = validate_credential(owner.credential(), BORROWER_ROLE).expect("valid borrow");
    let before = acquire_calls();

    // `root` here is a directory `lease_from` must never reach for.
    let root = temp_root("lease-from-unused");
    let lease = lease_from(Ok(Some(borrowed)), &root, "run-a").expect("a validated borrow");
    assert!(matches!(lease, Lease::Borrowed(_)), "a borrow must not be upgraded to an acquire");
    assert_eq!(acquire_calls(), before, "a valid borrow must not reach acquire");
    assert!(
        !root.join("run").exists(),
        "a borrowed weles must not create its own rollout lock"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_role_processctl_could_never_issue_is_refused() {
    // processctl validates the role charset when MINTING the lease
    // (state.rs:967); weles validates the role it CLAIMS by the same rule, so a
    // typo is a loud refusal here rather than a mismatch that reads as "the
    // parent is wrong".
    for bad in ["", "weles rollout", "weles/../splitproof", &"w".repeat(129)] {
        assert!(
            validate_identifier("borrower role", bad).is_err(),
            "{bad:?} must be refused as a role"
        );
    }
    assert!(validate_identifier("borrower role", BORROWER_ROLE).is_ok());
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

//! Store tests. The headline is [`two_writers_disjoint_rows_both_commit`]: two
//! threads sharing ONE `redb` store, each opening its OWN write transaction to
//! write DISJOINT rows concurrently — both must land with no loss and no error.
//! That is the exact race whole-file JSON loses (a second whole-document rewrite
//! clobbers the first), and it is the entire reason this transactional store
//! replaces the JSON checkpoint for these facts. redb serializes write
//! transactions (a second `begin_write` blocks until the first commits), so the
//! losing writer waits its turn and commits rather than being lost.

use std::sync::{Arc, Barrier};
use std::thread;

use super::{DeployRecord, PortAssignment, Store};

/// A throwaway on-disk DB path under the OS temp dir. redb keeps a single file
/// per database and takes a file lock on it, so a real path (not memory) is what
/// the durable-store contract operates on. Unique per test.
struct TempDb {
    dir: std::path::PathBuf,
}

impl TempDb {
    fn new(tag: &str) -> Self {
        let unique = format!(
            "weles-store-test-{tag}-{}-{:?}",
            std::process::id(),
            thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).expect("create temp db dir");
        TempDb { dir }
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.join("state.db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn deploy_record_round_trips() {
    let db = TempDb::new("deploy-roundtrip");
    let store = Store::open(&db.path()).expect("open store");

    let record = DeployRecord {
        generation: "gen-7".to_string(),
        sha_root: "abc123".to_string(),
        deployed_unix: 1_700_000_000,
    };
    store.record_deploy(&record).expect("record deploy");

    let read = store.deploy_record("gen-7").expect("read deploy");
    assert_eq!(read, Some(record));

    // Unrecorded generation reads back as None, distinct from an error — and,
    // critically for redb, does NOT error on the (never-written) table because
    // `open` creates it up front.
    assert_eq!(store.deploy_record("gen-99").expect("read missing"), None);
}

#[test]
fn deploy_record_upserts_on_same_generation() {
    let db = TempDb::new("deploy-upsert");
    let store = Store::open(&db.path()).expect("open store");

    store
        .record_deploy(&DeployRecord {
            generation: "gen-1".to_string(),
            sha_root: "first".to_string(),
            deployed_unix: 1,
        })
        .expect("first record");
    store
        .record_deploy(&DeployRecord {
            generation: "gen-1".to_string(),
            sha_root: "second".to_string(),
            deployed_unix: 2,
        })
        .expect("re-record same generation");

    let read = store.deploy_record("gen-1").expect("read").expect("present");
    assert_eq!(read.sha_root, "second");
    assert_eq!(read.deployed_unix, 2);
}

/// A3 defines `port_assignment` for A4's writer. Even with no production caller,
/// the typed API must round-trip so A4 lands on a proven table, not an untested
/// schema.
#[test]
fn port_assignment_round_trips_even_without_a_production_writer() {
    let db = TempDb::new("port-roundtrip");
    let store = Store::open(&db.path()).expect("open store");

    let assignment = PortAssignment {
        instance_id: "characters#1".to_string(),
        provider: "characters".to_string(),
        port: 54321,
        alive: true,
    };
    store
        .record_port_assignment(&assignment)
        .expect("record port assignment");

    let read = store
        .port_assignment("characters#1")
        .expect("read port assignment");
    assert_eq!(read, Some(assignment));

    assert_eq!(
        store.port_assignment("nope#9").expect("read missing"),
        None
    );

    // The bool round-trips both ways.
    store
        .record_port_assignment(&PortAssignment {
            instance_id: "characters#1".to_string(),
            provider: "characters".to_string(),
            port: 54321,
            alive: false,
        })
        .expect("re-record dead");
    let read = store
        .port_assignment("characters#1")
        .expect("read")
        .expect("present");
    assert!(!read.alive);
}

/// THE headline contract test. Two threads share ONE store (redb's `Database` is
/// `Send + Sync`) and, released together off a barrier, write DISJOINT rows
/// CONCURRENTLY — each in its own write transaction. redb serializes write
/// transactions, so the second `begin_write` blocks on the first and then
/// commits: both rows must be present, and NEITHER thread may have seen an error
/// (any error fails the test). This is precisely what whole-file JSON could not
/// arbitrate: it had no lock to wait on, so one of two concurrent whole-document
/// rewrites was lost. (redb requires a single `Database` handle per file — this
/// shared-handle model is the redb analogue of the old SQLite
/// connection-per-writer + WAL contract.)
#[test]
fn two_writers_disjoint_rows_both_commit() {
    let db = TempDb::new("two-writers");
    let store = Arc::new(Store::open(&db.path()).expect("open shared store"));

    // A barrier so both threads issue their write at (as near as possible) the
    // same instant — maximizing genuine overlap on the write lock. Each does
    // several writes in a tight loop to widen the contention window.
    let barrier = Arc::new(Barrier::new(2));
    let writes_each = 50u32;

    let spawn_writer = |tag: &'static str, provider: &'static str| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            for i in 0..writes_each {
                store
                    .record_port_assignment(&PortAssignment {
                        instance_id: format!("{tag}#{i}"),
                        provider: provider.to_string(),
                        port: 40000 + i as u16,
                        alive: true,
                    })
                    .unwrap_or_else(|error| {
                        panic!("writer {tag} row {i} must commit, got {error:#}")
                    });
            }
        })
    };

    let a = spawn_writer("alpha", "characters");
    let b = spawn_writer("beta", "inventory");
    a.join().expect("writer alpha");
    b.join().expect("writer beta");

    // Every disjoint row from BOTH writers is present — no loss.
    for i in 0..writes_each {
        assert_eq!(
            store
                .port_assignment(&format!("alpha#{i}"))
                .expect("read alpha")
                .map(|a| a.provider),
            Some("characters".to_string()),
            "alpha row {i} lost",
        );
        assert_eq!(
            store
                .port_assignment(&format!("beta#{i}"))
                .expect("read beta")
                .map(|a| a.provider),
            Some("inventory".to_string()),
            "beta row {i} lost",
        );
    }
}

/// Pins the DOCUMENTED cross-process contract that actually regressed vs SQLite:
/// with SQLite (WAL + `busy_timeout`) a second connection/process opening the same
/// file would BLOCK and then commit; redb takes an EXCLUSIVE file lock, so a second
/// [`Store::open`] on a path whose first `Store` is still LIVE is REJECTED outright
/// with a `DatabaseError::DatabaseAlreadyOpen`-class error. This is the
/// weaker-but-accepted behavior recorded in the module docs and the design errata
/// — the two production writers are separate PROCESSES, and both call sites
/// log-and-continue on this error rather than blocking. Asserting the reject (not
/// the easy in-process thread case above) pins what changed.
#[test]
fn second_open_on_live_path_is_rejected_not_blocked() {
    let db = TempDb::new("second-open-rejected");

    // First handle stays alive (holds redb's exclusive file lock) for the whole
    // test — this is the cross-process shape modeled in-process.
    let _first = Store::open(&db.path()).expect("first open acquires the lock");

    // `Store` is not `Debug`, so match rather than `expect_err`.
    let error = match Store::open(&db.path()) {
        Ok(_) => panic!(
            "a second open on a live redb path must be REJECTED (exclusive lock), \
             not block-and-commit like SQLite's busy_timeout"
        ),
        Err(error) => error,
    };
    // redb's DatabaseAlreadyOpen Display is "Database already open. Cannot acquire
    // lock." — assert on that stable text through the anyhow context chain so the
    // test pins the specific reject class, not merely "some error".
    let rendered = format!("{error:#}").to_ascii_lowercase();
    assert!(
        rendered.contains("already open"),
        "expected a DatabaseAlreadyOpen-class reject, got: {error:#}",
    );
}

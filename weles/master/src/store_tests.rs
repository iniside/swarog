//! Store tests. The headline is [`two_writers_disjoint_rows_both_commit`]: two
//! threads, each with its OWN connection, writing DISJOINT rows concurrently —
//! both must land with no loss and no `SQLITE_BUSY` failure. That is the exact
//! race whole-file JSON loses (a second whole-document rewrite clobbers the
//! first), and it is the entire reason SQLite replaces the JSON checkpoint for
//! these facts.

use std::sync::{Arc, Barrier};
use std::thread;

use super::{DeployRecord, PortAssignment, Store};

/// A throwaway on-disk DB path under the OS temp dir (SQLite WAL needs a real
/// file, not `:memory:`, for the multi-connection contract to mean anything —
/// each `:memory:` connection is a SEPARATE database). Unique per test.
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
        // Best-effort: also removes -wal/-shm siblings WAL leaves behind.
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

    // Unrecorded generation reads back as None, distinct from an error.
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

    // The bool round-trips both ways (INTEGER 0/1).
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

/// THE headline contract test. Two threads, each opening its OWN connection to
/// the SAME file DB, write DISJOINT rows CONCURRENTLY (released together off a
/// barrier so their writes genuinely overlap). Under WAL + busy_timeout the
/// second writer blocks on the write lock and then commits — both rows must be
/// present, and NEITHER thread may have seen a `SQLITE_BUSY` (any error fails the
/// test). This is precisely what whole-file JSON could not arbitrate: it had no
/// lock to wait on, so one of two concurrent whole-document rewrites was lost.
#[test]
fn two_writers_disjoint_rows_both_commit() {
    let db = TempDb::new("two-writers");
    let path = db.path();

    // Open once up front so the schema exists before either racing writer runs
    // (both would create it idempotently anyway; this keeps the race purely
    // about the two INSERTs contending on the write lock).
    Store::open(&path).expect("pre-create schema");

    // A barrier so both threads issue their write at (as near as possible) the
    // same instant — maximizing genuine overlap on the write lock. Each does
    // several writes in a tight loop to widen the contention window.
    let barrier = Arc::new(Barrier::new(2));
    let writes_each = 50u32;

    let spawn_writer = |tag: &'static str, provider: &'static str| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            // Connection-per-writer: each thread owns its own !Sync connection.
            let store = Store::open(&path).expect("open per-writer connection");
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
    let reader = Store::open(&path).expect("open reader");
    for i in 0..writes_each {
        assert_eq!(
            reader
                .port_assignment(&format!("alpha#{i}"))
                .expect("read alpha")
                .map(|a| a.provider),
            Some("characters".to_string()),
            "alpha row {i} lost",
        );
        assert_eq!(
            reader
                .port_assignment(&format!("beta#{i}"))
                .expect("read beta")
                .map(|a| a.provider),
            Some("inventory".to_string()),
            "beta row {i} lost",
        );
    }
}

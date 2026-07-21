//! The master's DURABLE runtime store — a small SQLite DB (WAL mode) at
//! `run/weles/state.db` for the runtime facts that are NOT reconcilable from an
//! agent's live report and therefore must survive a master restart.
//!
//! # Why SQLite and not the JSON checkpoint
//!
//! Soft live-fleet status (each service's `Status`/`Readiness`/pid) stays in
//! [`crate::state`]'s whole-document `state.json`: it has EXACTLY ONE writer
//! (the supervisor thread's `Reporter`) and is reconcilable from agent reports
//! after a restart, so an atomic tmp→rename of the whole file is enough. This
//! store holds the opposite class — facts with MORE THAN ONE writer that a
//! whole-file rewrite cannot arbitrate:
//!
//! * `deploy_history` — provenance of every generation flip (its combined SHA +
//!   wall-clock), written on the deploy path.
//! * `port_assignment` — agent-side minted ports (A4). The agent's tokio island
//!   binds a free port at spawn and reports it UP; that is a SECOND writer racing
//!   the supervisor's own checkpoint. Whole-file JSON loses one of two concurrent
//!   whole-document rewrites (last-writer-wins clobber); SQLite arbitrates them.
//!
//! # Concurrency contract — WAL + one connection per writer + busy_timeout
//!
//! Each writer opens its OWN [`Store`] (its own `rusqlite::Connection`) — a
//! connection is `!Sync` and single-threaded by design, so it is never shared
//! across threads. WAL mode ([`journal_mode=WAL`]) lets readers proceed while a
//! writer holds the write lock, and SQLite still permits AT MOST ONE writer at a
//! time. Two writers that overlap would, by default, make the second fail
//! immediately with `SQLITE_BUSY`. We set a generous [`busy_timeout`] on every
//! connection so the second writer BLOCKS until the first commits instead of
//! failing — the disjoint rows both land, no loss. That "the second writer waits
//! its turn and both commit" is the precise property whole-file JSON could not
//! give (it had no lock to wait on — it simply overwrote), and it is the whole
//! reason this store exists.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// How long a writer BLOCKS on SQLite's write lock before giving up with
/// `SQLITE_BUSY`. Generous on purpose: writes here are tiny (a single small row)
/// and rare (a deploy flip, an agent mint-report), so the only reason two would
/// contend is genuine concurrency, and the losing writer should WAIT for the
/// microseconds the winner holds the lock, never fail. This is the knob that
/// turns "connection-per-writer" from a `SQLITE_BUSY` hazard into a clean
/// serialize-and-both-commit.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// One recorded generation flip — provenance for `weles deploy`/`rollback`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployRecord {
    /// The generation name that became `current` (`gen-N`).
    pub generation: String,
    /// A single SHA over the generation's artifact hashes (the caller — which
    /// owns the hashing dep — folds the per-artifact SHAs into one root hash).
    pub sha_root: String,
    /// Wall-clock seconds since the Unix epoch at the moment of the flip.
    pub deployed_unix: i64,
}

/// One agent-minted port binding.
///
/// **Defined but WRITERLESS by design in A3.** The real writer is A4 (agent-side
/// port minting): the agent binds a free port at spawn and reports it UP through
/// [`Store::record_port_assignment`]. A3 lands the table + typed API so A4 has a
/// durable place to persist a minted port the instant it is bound; A3 deliberately
/// does NOT fabricate a synthetic caller to "use" it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortAssignment {
    /// The instance the port was minted for (`<provider>#<n>`).
    pub instance_id: String,
    /// The capability provider whose instance this is.
    pub provider: String,
    /// The OS-assigned port the agent bound.
    pub port: u16,
    /// Whether the instance holding this port is currently alive.
    pub alive: bool,
}

/// A single-writer handle to the master's durable store. Hold one PER WRITER
/// (per thread) — never share a `Store` across threads. Opening is cheap and the
/// schema is created idempotently on every open, so a fresh connection for a
/// short-lived writer is the intended usage.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if absent) the store at `path`, puts the DB in WAL mode,
    /// arms the [`BUSY_TIMEOUT`], and ensures the schema exists. Idempotent:
    /// every table is `CREATE TABLE IF NOT EXISTS`, so a second open over an
    /// existing DB is a no-op on the schema. The parent directory must already
    /// exist (weles's `run/weles` is scaffolded at layout discovery).
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open master store {}", path.display()))?;
        // WAL: readers never block the writer and vice versa; the write lock
        // still serializes writers, which is exactly what we want. journal_mode
        // returns the new mode as a result row, so read it via query_row rather
        // than execute (which would error on the returned row).
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .context("enable WAL journal mode")?;
        anyhow::ensure!(
            mode.eq_ignore_ascii_case("wal"),
            "master store did not enter WAL mode (got {mode:?}) — refusing a store \
             whose concurrent-writer contract is not in force",
        );
        // Block a contending writer instead of failing it with SQLITE_BUSY.
        conn.busy_timeout(BUSY_TIMEOUT)
            .context("arm busy_timeout on master store")?;
        let store = Store { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS deploy_history (
                     generation    TEXT PRIMARY KEY,
                     sha_root       TEXT NOT NULL,
                     deployed_unix  INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS port_assignment (
                     instance_id  TEXT PRIMARY KEY,
                     provider     TEXT NOT NULL,
                     port         INTEGER NOT NULL,
                     alive        INTEGER NOT NULL
                 );",
            )
            .context("create master store schema")?;
        Ok(())
    }

    /// Records a generation flip. `generation` is the primary key: re-recording
    /// the same generation (an idempotent redeploy of the same `gen-N`) replaces
    /// the row rather than erroring, via `INSERT OR REPLACE`.
    pub fn record_deploy(&self, record: &DeployRecord) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO deploy_history (generation, sha_root, deployed_unix)
                 VALUES (?1, ?2, ?3)",
                (&record.generation, &record.sha_root, record.deployed_unix),
            )
            .with_context(|| format!("record deploy history for {}", record.generation))?;
        Ok(())
    }

    /// Reads back a single generation's deploy record, or `None` if that
    /// generation was never recorded.
    pub fn deploy_record(&self, generation: &str) -> Result<Option<DeployRecord>> {
        self.conn
            .query_row(
                "SELECT generation, sha_root, deployed_unix
                 FROM deploy_history WHERE generation = ?1",
                [generation],
                |row| {
                    Ok(DeployRecord {
                        generation: row.get(0)?,
                        sha_root: row.get(1)?,
                        deployed_unix: row.get(2)?,
                    })
                },
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .with_context(|| format!("read deploy history for {generation}"))
    }

    /// Records (upserts) an agent-minted port binding, keyed by `instance_id`.
    ///
    /// **A4 writes this; A3 only defines it** (see [`PortAssignment`]). There is
    /// no production caller yet by design — do not invent one.
    pub fn record_port_assignment(&self, assignment: &PortAssignment) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO port_assignment (instance_id, provider, port, alive)
                 VALUES (?1, ?2, ?3, ?4)",
                (
                    &assignment.instance_id,
                    &assignment.provider,
                    assignment.port,
                    assignment.alive as i64,
                ),
            )
            .with_context(|| {
                format!("record port assignment for {}", assignment.instance_id)
            })?;
        Ok(())
    }

    /// Reads back one instance's port assignment, or `None` if unrecorded.
    pub fn port_assignment(&self, instance_id: &str) -> Result<Option<PortAssignment>> {
        self.conn
            .query_row(
                "SELECT instance_id, provider, port, alive
                 FROM port_assignment WHERE instance_id = ?1",
                [instance_id],
                |row| {
                    let port: i64 = row.get(2)?;
                    let alive: i64 = row.get(3)?;
                    Ok(PortAssignment {
                        instance_id: row.get(0)?,
                        provider: row.get(1)?,
                        port: port as u16,
                        alive: alive != 0,
                    })
                },
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .with_context(|| format!("read port assignment for {instance_id}"))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;

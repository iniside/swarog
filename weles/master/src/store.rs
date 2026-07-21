//! The master's DURABLE runtime store — a small embedded key/value DB (`redb`)
//! at `run/weles/state.db` for the runtime facts that are NOT reconcilable from
//! an agent's live report and therefore must survive a master restart.
//!
//! # Why an embedded store and not the JSON checkpoint
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
//!   whole-document rewrites (last-writer-wins clobber); a transactional embedded
//!   store arbitrates them.
//!
//! # Concurrency contract — redb serializes write transactions
//!
//! `redb` (pure Rust — no C dependency, see the crate's `Cargo.toml` comment for
//! why that matters to weles's cross-compile requirement) holds ONE
//! [`redb::Database`] per file, and that handle is `Send + Sync`: it is shared
//! across the process's writer threads rather than opened once per writer. redb
//! SERIALIZES write transactions internally — a second [`Database::begin_write`]
//! BLOCKS until the first commits — so two threads that write DISJOINT rows
//! concurrently both commit with no loss. That "the second writer waits its turn
//! and both commit" is the precise property whole-file JSON could not give (it
//! had no lock to wait on — it simply overwrote), and it is the whole reason this
//! store exists. It is the same guarantee the previous SQLite (WAL +
//! `busy_timeout`) store gave, restated in redb's terms — but ONLY for in-process
//! threads sharing this one `Database`.
//!
//! ## Cross-process: rejected, not blocked (weaker than SQLite, on purpose)
//!
//! redb holds an EXCLUSIVE file lock, so a concurrent open of a SECOND `Store`
//! (a second `Database`) on the same path — the cross-process shape, `weles
//! deploy` writing `deploy_history` while `weles up`'s agent writes
//! `port_assignment` — is REJECTED immediately at [`Store::open`] with a
//! `redb::DatabaseError::DatabaseAlreadyOpen`-class error, NOT blocked-then-
//! committed the way SQLite's `busy_timeout` would. Both production call sites
//! open the store briefly and treat an open/write failure as log-and-continue
//! (never `?`), so this degrades to a WARN and the provenance row is lost for
//! that narrow boot/mint overlap window — an accepted trade for the pure-Rust
//! cross-compile (see the crate `Cargo.toml`), documented so nothing relies on
//! the old cross-process block-and-commit. Pinned by
//! `store_tests::second_open_on_live_path_is_rejected_not_blocked`.

use std::path::Path;

use anyhow::{Context, Result};
use redb::{Database, TableDefinition};

/// The two persisted tables. Keys are the natural string identifier; values are
/// the remaining fields serialized as a JSON tuple (redb stores opaque bytes, so
/// serde_json is the seam that turns a typed row into a `&[u8]` value and back).
const DEPLOY_HISTORY: TableDefinition<&str, &[u8]> = TableDefinition::new("deploy_history");
const PORT_ASSIGNMENT: TableDefinition<&str, &[u8]> = TableDefinition::new("port_assignment");

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

/// A handle to the master's durable store. `redb`'s [`Database`] is `Send + Sync`
/// and serializes writers internally, so ONE `Store` is shared across the
/// process's writer threads (wrap it in an [`std::sync::Arc`]) — unlike the old
/// SQLite `!Sync` connection-per-writer model. Opening is cheap and the schema
/// (both tables) is created idempotently on every open.
pub struct Store {
    db: Database,
}

impl Store {
    /// Opens (creating if absent) the store at `path` and ensures both tables
    /// exist. redb creates a table lazily on its first write-txn open, so this
    /// opens+commits both tables up front: a subsequent READ against a fresh DB
    /// then returns `None` rather than erroring on an absent table, and the
    /// write+commit round-trip proves the store is writable at open — redb's
    /// analogue of the old WAL-mode assertion. The parent directory must already
    /// exist (weles's `run/weles` is scaffolded at layout discovery).
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("open master store {}", path.display()))?;
        let write = db.begin_write().context("begin master store init txn")?;
        {
            write
                .open_table(DEPLOY_HISTORY)
                .context("initialize deploy_history table")?;
            write
                .open_table(PORT_ASSIGNMENT)
                .context("initialize port_assignment table")?;
        }
        write
            .commit()
            .context("commit master store init txn (store not writable)")?;
        Ok(Store { db })
    }

    /// Records a generation flip. `generation` is the key: re-recording the same
    /// generation (an idempotent redeploy of the same `gen-N`) overwrites the row
    /// rather than erroring — redb's `insert` is an upsert.
    pub fn record_deploy(&self, record: &DeployRecord) -> Result<()> {
        let value = serde_json::to_vec(&(&record.sha_root, record.deployed_unix))
            .with_context(|| format!("encode deploy record for {}", record.generation))?;
        let write = self.db.begin_write().context("begin deploy-history write")?;
        {
            let mut table = write
                .open_table(DEPLOY_HISTORY)
                .context("open deploy_history for write")?;
            table
                .insert(record.generation.as_str(), value.as_slice())
                .with_context(|| format!("record deploy history for {}", record.generation))?;
        }
        write
            .commit()
            .with_context(|| format!("commit deploy history for {}", record.generation))?;
        Ok(())
    }

    /// Reads back a single generation's deploy record, or `None` if that
    /// generation was never recorded.
    pub fn deploy_record(&self, generation: &str) -> Result<Option<DeployRecord>> {
        let read = self.db.begin_read().context("begin deploy-history read")?;
        let table = read
            .open_table(DEPLOY_HISTORY)
            .context("open deploy_history for read")?;
        let Some(guard) = table
            .get(generation)
            .with_context(|| format!("read deploy history for {generation}"))?
        else {
            return Ok(None);
        };
        let (sha_root, deployed_unix): (String, i64) = serde_json::from_slice(guard.value())
            .with_context(|| format!("decode deploy record for {generation}"))?;
        Ok(Some(DeployRecord {
            generation: generation.to_string(),
            sha_root,
            deployed_unix,
        }))
    }

    /// Records (upserts) an agent-minted port binding, keyed by `instance_id`.
    ///
    /// **A4 writes this; A3 only defines it** (see [`PortAssignment`]). There is
    /// no production caller yet by design — do not invent one.
    pub fn record_port_assignment(&self, assignment: &PortAssignment) -> Result<()> {
        let value =
            serde_json::to_vec(&(&assignment.provider, assignment.port, assignment.alive))
                .with_context(|| {
                    format!("encode port assignment for {}", assignment.instance_id)
                })?;
        let write = self.db.begin_write().context("begin port-assignment write")?;
        {
            let mut table = write
                .open_table(PORT_ASSIGNMENT)
                .context("open port_assignment for write")?;
            table
                .insert(assignment.instance_id.as_str(), value.as_slice())
                .with_context(|| {
                    format!("record port assignment for {}", assignment.instance_id)
                })?;
        }
        write.commit().with_context(|| {
            format!("commit port assignment for {}", assignment.instance_id)
        })?;
        Ok(())
    }

    /// Reads back one instance's port assignment, or `None` if unrecorded.
    pub fn port_assignment(&self, instance_id: &str) -> Result<Option<PortAssignment>> {
        let read = self.db.begin_read().context("begin port-assignment read")?;
        let table = read
            .open_table(PORT_ASSIGNMENT)
            .context("open port_assignment for read")?;
        let Some(guard) = table
            .get(instance_id)
            .with_context(|| format!("read port assignment for {instance_id}"))?
        else {
            return Ok(None);
        };
        let (provider, port, alive): (String, u16, bool) = serde_json::from_slice(guard.value())
            .with_context(|| format!("decode port assignment for {instance_id}"))?;
        Ok(Some(PortAssignment {
            instance_id: instance_id.to_string(),
            provider,
            port,
            alive,
        }))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;

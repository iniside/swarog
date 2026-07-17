//! `run/rollout.lock` participation — the ONE convention weles shares with
//! devctl/verifyctl/processctl: at most one rollout-bearing command may run
//! against the shared local Postgres at a time. The locking protocol is
//! copied (never imported — zero-sharing) from `tools/processctl/src/lock.rs`
//! and must stay bit-compatible with it:
//!
//! * Unix: `flock(LOCK_EX | LOCK_NB)` on the whole file.
//! * Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)`
//!   on EXACTLY 1 byte at offset `1 << 63` (`tools/processctl/src/lock.rs`,
//!   `try_lock_exclusive`/`lock_overlapped`), with the file opened
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE`. Locking any other range would let
//!   weles and a devctl/verifyctl rollout both "acquire" the lock at once.
//! * If weles CREATES the file on Windows, it must carry the owner-only,
//!   `SE_DACL_PROTECTED` DACL (`tools/processctl/src/state.rs`,
//!   `OwnerOnlySecurity`) — a plain `std::fs`-created lock file would make
//!   every later devctl/verifyctl run fail its lock-security validation
//!   permanently. On Unix the file is created mode 0600.
//!
//! After acquiring, the file is truncated and rewritten with weles's OWN
//! metadata schema (`{version, tool, pid, run_id, started_unix}`); devctl
//! never reads foreign metadata — it truncates on its own acquire the same
//! way.
//!
//! # The borrowed half
//!
//! [`acquire`] is the operator path: weles owns the rollout. But verifyctl
//! holds ONE `processctl::OwnedLease` for its whole manifest and lends it to
//! each of the roles it named at acquire — `["splitproof", "weles"]`
//! (`tools/verifyctl/src/runner.rs:57`) — one borrower alive at a time. A
//! verifyctl stage that booted weles would otherwise deadlock on
//! `run/rollout.lock` against verifyctl's own lease — so [`acquire_or_borrow`]
//! extends weles's existing bit-compatibility claim to that borrow protocol.
//! Same rule as the lock itself: the shape is COPIED from
//! `tools/processctl/src/lock.rs`, never imported (zero-sharing), and must
//! match what processctl actually writes:
//!
//! * The parent appends [`BORROWED_LEASE_ARG`] to the child's argv and writes
//!   the JSON [`BorrowCredential`] into a private pipe on the child's stdin
//!   (`OwnedLease::spawn_borrower` → `credential_pipe`/`deliver_credential`).
//! * The child validates the credential AGAINST THE LIVE WORLD
//!   (`validate_credential`), then keeps the lock file open WITHOUT locking it.
//!   A borrower never takes the byte-range lock — the parent still holds it.
//! * The one-shot marker beside the lock (`borrow_marker_path`) is keyed PER
//!   ROLE, so weles's borrow and splitproof's borrow of the same lease never
//!   collide; the parent deletes every role's marker when its own lease drops.
//!   Within one role it is still one-shot: a re-lent credential is refused.
//!
//! Anything that fails to validate REFUSES — a borrow never falls back to
//! [`acquire`] and never proceeds unlocked.
//!
//! **Deliberate non-check (recorded, not smuggled):** the credential's
//! `lock_path` is trusted as the authority for WHICH lock is being borrowed; it
//! is not required to equal `<root>/run/rollout.lock`. Validation still proves a
//! live owner holds that lock. A parent rooted in a different checkout would
//! thus lend a lock protecting a different tree — out of scope under the
//! trusted-local-operator model (CLAUDE.md, "Dev tooling scope"), and a strict
//! path equality would falsely refuse over path spelling (case/UNC/symlink).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// weles's own lock-metadata schema. Purely informational for a human (or a
/// later `weles status`) inspecting who owns the rollout.
#[derive(Serialize)]
struct LockMetadata<'a> {
    version: u32,
    tool: &'a str,
    pid: u32,
    run_id: &'a str,
    started_unix: u64,
}

/// RAII ownership of `run/rollout.lock`, held for the entire `up()` lifetime.
/// Dropping it releases the byte-range/flock lock and closes the handle.
#[derive(Debug)]
pub struct RolloutLock {
    file: File,
    path: PathBuf,
}

impl RolloutLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RolloutLock {
    fn drop(&mut self) {
        // Best-effort explicit unlock; closing the handle releases the lock
        // anyway on both platforms.
        let _ = imp::unlock(&self.file);
    }
}

/// Acquires `<root>/run/rollout.lock` exclusively and non-blockingly
/// (creating `run/` if missing), then truncates and rewrites the metadata.
/// A lock owned by anyone else — devctl, verifyctl, or another weles — is a
/// loud, immediate error naming the path.
pub fn acquire(root: &Path, run_id: &str) -> Result<RolloutLock> {
    #[cfg(test)]
    ACQUIRE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("create lock directory {}", run_dir.display()))?;
    let path = run_dir.join("rollout.lock");

    let mut file =
        imp::open_lock_file(&path).with_context(|| format!("open {}", path.display()))?;

    let acquired = imp::try_lock_exclusive(&file)
        .with_context(|| format!("acquire rollout lock {}", path.display()))?;
    if !acquired {
        bail!(
            "another rollout owns {} — a devctl/verifyctl/weles rollout is active on this \
             machine (one test rollout at a time on the shared Postgres); stop it or wait \
             for it to finish before running weles up",
            path.display()
        );
    }

    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let metadata = serde_json::to_vec_pretty(&LockMetadata {
        version: 1,
        tool: "weles",
        pid: std::process::id(),
        run_id,
        started_unix,
    })
    .context("serialize rollout lock metadata")?;

    // The locked byte lives at offset 1<<63; the metadata at offset 0 — the
    // regions never overlap, and the lock-owning handle may write freely.
    file.set_len(0)
        .with_context(|| format!("truncate {}", path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewind {}", path.display()))?;
    file.write_all(&metadata)
        .with_context(|| format!("write metadata to {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush metadata to {}", path.display()))?;

    Ok(RolloutLock { file, path })
}

// Counts entries into `acquire` on THIS thread. `cargo test` runs each test on
// its own thread, so the count is per-test and unaffected by tests running in
// parallel. The borrow tests assert it stays at ZERO — proving the borrow path
// never reaches `acquire`, rather than merely proving it raised no error.
#[cfg(test)]
thread_local! {
    static ACQUIRE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The borrower role weles claims. A parent must have INCLUDED this role in the
/// set it named in processctl's `RolloutLock::acquire(path, run_id, roles)`;
/// a lease whose set omits it (e.g. one naming only `"splitproof"`) is refused.
/// verifyctl's lease names `["splitproof", "weles"]`
/// (`tools/verifyctl/src/runner.rs:57`).
pub const BORROWER_ROLE: &str = "weles";

/// The argv marker processctl appends to a borrower's command line
/// (`processctl::lock::BORROWED_LEASE_ARG`, `pub(crate)` there — hence the copy).
/// Its presence is the ONLY thing that makes this process look for an
/// inherited credential at all; without it `weles up` behaves exactly as it
/// does from an operator shell.
///
/// Two readers besides [`borrow_inherited_if_present`], and each is why this is
/// not private:
///
/// * `pub(crate)` reach for [`crate::cli::parse`], which must let this argument
///   through rather than reject it as unknown. The parent APPENDS it to
///   `weles up split`'s argv (`OwnedLease::spawn_borrower`), so the CLI is the
///   first thing a borrowed run meets — and a second literal over there would be
///   a spelling free to drift from the one this module matches on.
/// * `pub` reach (`#[doc(hidden)]`) for verifyctl's `weles-wire-contract` stage,
///   the only place that may see this AND `processctl::BORROWED_LEASE_ARG`.
///   THIS const is a HAND-COPY (zero-sharing: weles imports no workspace crate),
///   so nothing in either crate can tell that it still matches. If processctl
///   renamed its marker, weles would keep spelling `…-v1`, `cli::parse` would
///   reject the real appended argument, and the borrow path would silently
///   become unreachable from the one verb that takes a lease — exactly the bug
///   this const's own tolerance arm exists to fix. A test that parses THIS value
///   cannot catch that; only a comparison against processctl's can.
#[doc(hidden)]
pub const BORROWED_LEASE_ARG: &str = "--processctl-borrowed-lease-v1";

/// The exact bytes processctl writes into its one-shot marker — and requires to
/// read back before deleting it on the owner's drop
/// (`processctl::lock::CONSUMED_MARKER`, read back by its `cleanup_consumption_marker`). Any other
/// byte string here would leave weles's marker behind forever.
const CONSUMED_MARKER: &[u8] = b"processctl-borrowed-v1\n";

/// `processctl::ROLLOUT_LOCK_VERSION`.
///
/// v2: the lease carries a SET of permitted borrower roles
/// (`allowed_borrower_roles`) and dropped the dead `nonce`. `weles deploy`
/// stages binaries that may lag the tree, so a stale weles speaking v1 is real —
/// the version check below is what makes that a legible refusal instead of a
/// `deny_unknown_fields` parse error.
const OWNER_LEASE_VERSION: u32 = 2;
/// `processctl::lock::MAX_CREDENTIAL_BYTES` / `MAX_METADATA_BYTES`.
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// Process-local one-shot guard, mirroring
/// `processctl::lock::INHERITED_CREDENTIAL_CONSUMED`: stdin is a
/// process-global, so a second consume attempt would read an already-drained
/// pipe rather than fail honestly.
static INHERITED_CREDENTIAL_CONSUMED: AtomicBool = AtomicBool::new(false);

/// Mirror of `processctl::StartMarker` (`tools/processctl/src/process.rs`)
/// — a serde newtype, so it is a bare integer on the wire.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StartMarker(u64);

/// Mirror of `processctl::ProcessIdentity` (`tools/processctl/src/process.rs`).
/// `pid` alone would be a recycled-PID hazard; `started` (Windows process
/// creation FILETIME / Linux `/proc/<pid>/stat` field 22) is what makes the
/// identity a real one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnerIdentity {
    pid: u32,
    executable: PathBuf,
    started: StartMarker,
}

/// Mirror of processctl's PRIVATE `LockMetadata` (`tools/processctl/src/lock.rs`)
/// — the JSON an `OwnedLease` writes at offset 0 of the lock file. Weles's own
/// [`LockMetadata`] is a DIFFERENT schema: each tool truncates and rewrites on
/// its own `acquire` and never reads foreign metadata, so the two only ever
/// meet here, on the borrow path, where weles is the reader of processctl's.
///
/// `deny_unknown_fields` + the `PartialEq` comparison against the credential is
/// deliberate fail-closed behaviour: if processctl's schema ever changes, weles
/// REFUSES to borrow instead of guessing.
/// `allowed_borrower_roles` is a SET (processctl v2): one verifyctl lease serves
/// splitproof AND weles over its life. An EMPTY set means borrowing is disabled
/// (processctl's `acquire_exclusive`, devctl's shape).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnerLease {
    version: u32,
    owner: OwnerIdentity,
    run_id: String,
    lease_started_unix_nanos: u64,
    allowed_borrower_roles: BTreeSet<String>,
}

/// Mirror of `processctl::lock::BorrowCredential`,
/// as delivered over the private stdin pipe.
///
/// It names no role: weles CLAIMS [`BORROWER_ROLE`], that claim is checked
/// against `metadata.allowed_borrower_roles`, and the claim is what keys the
/// per-role marker. (processctl v1 also carried a `nonce` here; it was dead on
/// the producer side — never checked, never written to the marker — and v2
/// deleted it rather than leave a field a reader would assume mattered.)
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BorrowCredential {
    version: u32,
    lock_path: PathBuf,
    metadata: OwnerLease,
}

/// Copied from `processctl::lock::VersionProbe` — reads ONLY `version` out of a
/// lease or credential document.
///
/// Deliberately NOT `deny_unknown_fields`, and deliberately parsed BEFORE the
/// typed shape: every other field is what changes between versions, so a typed
/// parse first would turn every cross-version pairing into an opaque
/// unknown-field error and leave the version check unreachable. `version` is the
/// one field that must mean the same thing in every version.
#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

/// Copied from `processctl::lock::check_version` — the version gate, run on
/// BYTES before any typed parse. This is what makes a stale `deploy/weles`
/// meeting a newer lease say "unsupported rollout lease version N" instead of
/// complaining about a field name.
fn check_version(bytes: &[u8], what: &str) -> Result<()> {
    let probe: VersionProbe = serde_json::from_slice(bytes)
        .with_context(|| format!("read the rollout lease version from {what}"))?;
    if probe.version != OWNER_LEASE_VERSION {
        bail!(
            "unsupported rollout lease version {} in {what} (weles speaks {OWNER_LEASE_VERSION}) \
             — refusing to borrow",
            probe.version
        );
    }
    Ok(())
}

/// A validated one-shot borrow of a parent's rollout lease.
///
/// It holds the lock file OPEN but deliberately UNLOCKED — the parent still
/// holds the byte-range/flock. Dropping it releases nothing (there is nothing to
/// release); the rollout ends when the PARENT's lease drops.
///
/// `PhantomData<Rc<()>>` (copied from `processctl::BorrowedLease`'s
/// `_not_transferable`) makes this — and
/// therefore [`Lease`] — `!Send`. Be precise about what that buys, because the
/// tempting claim is false: `!Send` prevents this value from being TRANSFERRED
/// TO ANOTHER THREAD (moved into a `std::thread::spawn`/`tokio::spawn` body, or
/// shared via an `Arc` that must itself be `Send`). It says NOTHING about *when*
/// the value is dropped on the thread that owns it. The lock's own lifetime does
/// not care about threads at all — flock is per open-file-description and
/// `LockFileEx` per handle, both of which survive a thread move — so this is
/// cheap defense-in-depth and faithfulness to processctl, not the thing that
/// keeps the lease alive until teardown finishes.
///
/// That ordering — `_lock` declared first in `run_up` and dropped LAST, after
/// teardown, `control`, and the agent island — is an invariant held by REVIEW
/// and by the comment at its declaration site. No type-system mechanism enforces
/// it; releasing the rollout lock while 12 services still drain would compile.
#[derive(Debug)]
pub struct BorrowedLease {
    _lock_file: File,
    metadata: OwnerLease,
    _not_transferable: PhantomData<Rc<()>>,
}

impl BorrowedLease {
    /// The OWNING run's id (verifyctl's), not weles's own.
    pub fn run_id(&self) -> &str {
        &self.metadata.run_id
    }

    /// PID of the process whose lease this is.
    pub fn owner_pid(&self) -> u32 {
        self.metadata.owner.pid
    }
}

/// Whichever way this process came to be inside the one permitted rollout.
///
/// Kept as ONE RAII value so [`crate::supervisor::run_up`] holds a single
/// `_lock` local that drops last, unchanged in either mode. `!Send` via
/// [`BorrowedLease`] — which bars a thread transfer, NOT an early drop; see
/// there.
#[derive(Debug)]
pub enum Lease {
    /// weles acquired `run/rollout.lock` itself — the operator path.
    Owned(RolloutLock),
    /// weles is running INSIDE a parent's rollout (a verifyctl stage).
    Borrowed(BorrowedLease),
}

/// The one entry point for `weles up`: consume an inherited one-shot lease if
/// this process was spawned as a borrower, otherwise [`acquire`] exactly as
/// before.
///
/// Fail-closed: a malformed credential, a dead/mismatched parent, or a wrong
/// role RETURNS THE ERROR. It never degrades into `acquire` (which would
/// deadlock against the very parent that spawned us) and never proceeds
/// unlocked.
pub fn acquire_or_borrow(root: &Path, run_id: &str) -> Result<Lease> {
    lease_from(borrow_inherited_if_present(BORROWER_ROLE), root, run_id)
}

/// [`acquire_or_borrow`]'s decision, with the borrow attempt's outcome as an
/// argument so a test can hand it the one thing it cannot stage: this process's
/// argv and stdin belong to cargo, so a REAL inherited credential — or a real
/// failure to validate one — is unreachable from a unit test.
///
/// Production calls this unconditionally through [`acquire_or_borrow`]; the seam
/// is an argument, not a `#[cfg(test)]` branch in the control flow. (Same shape
/// as [`crate::agentapi::AgentServer::bind`] → `bind_inner`.) The `Err` arm is
/// the whole point: it is what must NOT become an `acquire`.
fn lease_from(
    borrow: Result<Option<BorrowedLease>>,
    root: &Path,
    run_id: &str,
) -> Result<Lease> {
    match borrow? {
        Some(borrowed) => {
            println!(
                "weles: running inside rollout {} borrowed from pid {} — that lease, not this \
                 process, owns run/rollout.lock",
                borrowed.run_id(),
                borrowed.owner_pid()
            );
            Ok(Lease::Borrowed(borrowed))
        }
        None => acquire(root, run_id).map(Lease::Owned),
    }
}

/// Copied from `processctl::BorrowedLease::consume_inherited_if_present`
/// (`processctl::BorrowedLease::consume_inherited_if_present`); the caller shape is
/// `tools/splitproof/src/main.rs:472-480`.
///
/// `Ok(None)` — and ONLY `Ok(None)` — means "no borrow in this environment".
/// The argv marker without the private stdin pipe is an error, not a `None`: it
/// means the credential delivery this process depends on did not happen.
fn borrow_inherited_if_present(expected_role: &str) -> Result<Option<BorrowedLease>> {
    if !std::env::args_os().any(|arg| arg == BORROWED_LEASE_ARG) {
        return Ok(None);
    }
    if !imp::inherited_credential_present().context("inspect inherited borrower credential")? {
        bail!(
            "{BORROWED_LEASE_ARG} was present on the command line without its private credential \
             pipe on stdin — refusing to run: this process was told it is a borrower but was \
             never handed the lease"
        );
    }
    consume_inherited(expected_role).map(Some)
}

/// Copied from `processctl::BorrowedLease::consume_inherited`.
fn consume_inherited(expected_role: &str) -> Result<BorrowedLease> {
    validate_identifier("borrower role", expected_role)?;
    if INHERITED_CREDENTIAL_CONSUMED.swap(true, Ordering::AcqRel) {
        bail!("the inherited borrower credential was already consumed by this process");
    }
    let bytes = imp::consume_credential_stdin().context("consume inherited borrower credential")?;
    credential_from_bytes(&bytes, expected_role)
}

/// Copied from `processctl::lock::credential_from_bytes` — the wire entry point:
/// bound, then the version gate on the RAW BYTES, then the typed parse, then
/// validation against the live world.
///
/// Split out of [`consume_inherited`] so the version gate is reachable from a
/// test: `consume_inherited` can only be driven through a real inherited stdin
/// pipe, which cargo owns in a unit test.
fn credential_from_bytes(bytes: &[u8], expected_role: &str) -> Result<BorrowedLease> {
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        bail!("inherited borrower credential exceeds its {MAX_CREDENTIAL_BYTES}-byte bound");
    }
    check_version(bytes, "the inherited borrower credential")?;
    let credential: BorrowCredential =
        serde_json::from_slice(bytes).context("parse inherited borrower credential")?;
    validate_credential(credential, expected_role)
}

/// Copied from `processctl::lock::validate_credential` — the whole
/// point of the copy is that these checks are the ones processctl's own borrower
/// makes, in the same order:
///
/// 1. the wire version is one weles understands;
/// 2. the lease PERMITS THIS ROLE (its role set contains `"weles"`; a lease
///    naming only `"splitproof"`, or none at all, is refused);
/// 3. the lock file's metadata still matches the credential FIELD-FOR-FIELD
///    after parsing (processctl's own comparison — byte equality would be
///    brittle against pretty-printed JSON whitespace) — a re-acquired lock
///    rewrites this, so a stale credential dies here;
/// 4. the named owner process is STILL LIVE and still the same process
///    (pid + executable + start marker, so a recycled PID is not the owner);
/// 5. the owner still HOLDS the advisory lock — the identity being live is not
///    enough, the lease itself must be;
/// 6. THIS ROLE's borrow has not already been consumed (exclusive-create marker
///    keyed per role — another role's borrow of the same lease is irrelevant).
///
/// Only then does a `BorrowedLease` exist. Note there is no `spawn_borrower`
/// twin on this side: weles is a borrower, never a lender.
fn validate_credential(credential: BorrowCredential, expected_role: &str) -> Result<BorrowedLease> {
    if credential.version != OWNER_LEASE_VERSION {
        bail!(
            "unsupported rollout lease version {} (weles speaks {OWNER_LEASE_VERSION}) — refusing \
             to borrow",
            credential.version
        );
    }
    if !credential
        .metadata
        .allowed_borrower_roles
        .contains(expected_role)
    {
        bail!(
            "borrower role mismatch: this lease permits {}, weles claims {expected_role:?} — \
             refusing to borrow",
            describe_roles(&credential.metadata.allowed_borrower_roles)
        );
    }

    let mut lock_file = imp::open_lock_file(&credential.lock_path)
        .with_context(|| format!("open borrowed lock {}", credential.lock_path.display()))?;
    let metadata = read_owner_lease(&mut lock_file, &credential.lock_path)?;
    if metadata != credential.metadata {
        bail!(
            "rollout lock {} no longer carries the lease named by the inherited credential — \
             refusing to borrow",
            credential.lock_path.display()
        );
    }

    let observed = imp::observe_process_identity(metadata.owner.pid).with_context(|| {
        format!(
            "rollout lease owner pid {} is not live — refusing to borrow",
            metadata.owner.pid
        )
    })?;
    if observed != metadata.owner {
        bail!(
            "rollout lease owner pid {} is a DIFFERENT process than the one that took the lease \
             (a recycled pid) — refusing to borrow",
            metadata.owner.pid
        );
    }
    if !is_locked_by_other(&lock_file)? {
        bail!(
            "rollout lease owner pid {} no longer holds {} — refusing to borrow a lease that is \
             already over",
            metadata.owner.pid,
            credential.lock_path.display()
        );
    }

    let marker = borrow_marker_path(&credential.lock_path, &metadata, expected_role);
    if let Err(error) = imp::create_consumption_marker(&marker) {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            bail!(
                "rollout lease {} has already been borrowed once as {expected_role:?} (the \
                 one-shot marker {} exists) — refusing to borrow it again",
                metadata.run_id,
                marker.display()
            );
        }
        return Err(anyhow::Error::new(error).context(format!(
            "claim the one-shot borrow of rollout lease {}",
            metadata.run_id
        )));
    }

    Ok(BorrowedLease {
        _lock_file: lock_file,
        metadata,
        _not_transferable: PhantomData,
    })
}

/// Copied from `processctl::lock::read_metadata`.
fn read_owner_lease(file: &mut File, path: &Path) -> Result<OwnerLease> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewind {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read lease metadata from {}", path.display()))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!(
            "rollout lock {} carries more than {MAX_METADATA_BYTES} bytes of metadata",
            path.display()
        );
    }
    // Version BEFORE shape. `deny_unknown_fields` on `OwnerLease` would
    // otherwise fire first on any foreign version and report a field name
    // instead of the version — which is the ONE thing a stale `deploy/weles`
    // meeting a newer processctl needs to be told.
    check_version(&bytes, &format!("rollout lock {}", path.display()))?;
    let metadata: OwnerLease = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parse the owning tool's lease metadata from {} — refusing to borrow",
            path.display()
        )
    })?;
    Ok(metadata)
}

/// Copied from `processctl::lock::borrow_marker_path` — the path MUST match,
/// because the owner deletes exactly this name (for every role in its set) when
/// its lease drops. `role` is part of the name: `run_id` and
/// `lease_started_unix_nanos` are constant for a lease's whole life, so without
/// it splitproof's borrow would consume weles's one-shot too.
fn borrow_marker_path(lock: &Path, metadata: &OwnerLease, role: &str) -> PathBuf {
    let file_name = lock
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    lock.with_file_name(format!(
        ".{file_name}.{}.{}.{role}.borrowed",
        metadata.run_id, metadata.lease_started_unix_nanos
    ))
}

/// Copied from `processctl::lock::describe_roles` — renders a lease's permitted
/// role set for a refusal message; empty is the borrowing-disabled sentinel.
/// Rendered exactly as processctl renders it (bare, comma-separated), so the two
/// halves of this hand-copied pair read identically in a log.
fn describe_roles(roles: &BTreeSet<String>) -> String {
    if roles.is_empty() {
        "<borrowing-disabled>".to_string()
    } else {
        roles.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

/// Copied from `processctl::lock::is_locked_by_other`: the only
/// portable probe is to TRY the lock — success means nobody holds it, so undo it
/// at once. This is not [`acquire`]: it takes no ownership, writes no metadata,
/// and on the success path of a borrow it never keeps the lock (the parent holds
/// it, so the try fails, which is the answer we want).
fn is_locked_by_other(file: &File) -> Result<bool> {
    if imp::try_lock_exclusive(file).context("probe the rollout lock owner")? {
        imp::unlock(file).context("release the rollout-lock probe")?;
        Ok(false)
    } else {
        Ok(true)
    }
}

/// Copied from `processctl::state::validate_identifier`
/// (`processctl::state::validate_identifier`) — the same charset processctl
/// enforces on a role, so weles cannot claim a role processctl could never issue.
fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        bail!("{field} must contain 1..=128 bytes, got {value:?}");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("{field} may contain only ASCII letters, digits, dot, underscore, and dash, got {value:?}");
    }
    Ok(())
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::Path;

    /// Opens (creating mode-0600 if absent) the lock file. `mode` applies
    /// only at creation; an existing file (e.g. devctl's) keeps its own.
    pub(super) fn open_lock_file(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
    }

    /// `flock(LOCK_EX | LOCK_NB)` — `Ok(false)` when someone else holds it.
    /// flock is per open-file-description, so even a second handle in the
    /// SAME process contends (pinned by `lock_tests`).
    pub(super) fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
            _ => Err(error),
        }
    }

    pub(super) fn unlock(file: &File) -> std::io::Result<()> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Copied from `processctl::platform::linux::observe_process_identity`
    /// (`tools/processctl/src/platform/linux.rs`). Field 22 of
    /// `/proc/<pid>/stat` (index 19 AFTER the `)` that closes the comm field —
    /// the comm may itself contain spaces and parens, which is why the parse
    /// starts at the LAST `)`) is the start time in clock ticks: the half of
    /// the identity a recycled PID cannot forge.
    #[cfg(target_os = "linux")]
    pub(super) fn observe_process_identity(pid: u32) -> std::io::Result<super::OwnerIdentity> {
        let proc_dir = Path::new("/proc").join(pid.to_string());
        let executable = std::fs::read_link(proc_dir.join("exe"))?;
        let stat = std::fs::read_to_string(proc_dir.join("stat"))?;
        let close = stat.rfind(')').ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed /proc stat")
        })?;
        let started = stat[close + 1..]
            .split_whitespace()
            .nth(19)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing process starttime")
            })?
            .parse::<u64>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        Ok(super::OwnerIdentity {
            pid,
            executable,
            started: super::StartMarker(started),
        })
    }

    /// Copied from `processctl::platform::darwin::observe_process_identity` +
    /// `proc_start_marker`/`proc_pidpath` (`tools/processctl/src/platform/darwin.rs`).
    /// `proc_pidpath` yields the executable; `proc_pidinfo(PROC_PIDTBSDINFO)`
    /// yields `pbi_start_tvsec`/`pbi_start_tvusec`, packed into one `u64` of
    /// microseconds since the epoch — the half of the identity a recycled PID
    /// cannot forge, the darwin analogue of Linux's `/proc/<pid>/stat` field 22.
    ///
    /// The packing MUST stay byte-for-byte identical to processctl's: the lender
    /// (processctl on darwin) wrote `metadata.owner.started` with this exact
    /// formula, and [`super::validate_credential`] compares the two
    /// `OwnerIdentity`s field-for-field. Both libproc calls fail with `ESRCH`
    /// once the task is gone or a zombie — the fail-closed `OwnerNotLive` the
    /// borrow path expects, so a dead owner is never borrowed from.
    #[cfg(target_os = "macos")]
    pub(super) fn observe_process_identity(pid: u32) -> std::io::Result<super::OwnerIdentity> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let cpid = pid as libc::c_int;

        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(cpid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        if read != size {
            return Err(std::io::Error::last_os_error());
        }
        let started = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;

        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let written =
            unsafe { libc::proc_pidpath(cpid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if written <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        buf.truncate(written as usize);
        let executable = std::path::PathBuf::from(OsString::from_vec(buf));

        Ok(super::OwnerIdentity {
            pid,
            executable,
            started: super::StartMarker(started),
        })
    }

    /// processctl supports Windows, Linux, and macOS
    /// (`processctl::process`'s platform gate), so a lease minted anywhere
    /// else cannot exist — and an unverifiable owner must never be borrowed from.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn observe_process_identity(_pid: u32) -> std::io::Result<super::OwnerIdentity> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "rollout-lease owner identity cannot be observed on {}",
                std::env::consts::OS
            ),
        ))
    }

    /// Copied from `processctl::lock::inherited_credential_present`
    /// (`processctl::lock::inherited_credential_present`): the parent hands the credential
    /// over an anonymous pipe on stdin, so "stdin is a FIFO" is exactly the
    /// question. An operator shell leaves stdin a tty or a file.
    pub(super) fn inherited_credential_present() -> std::io::Result<bool> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(0, stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stat = unsafe { stat.assume_init() };
        Ok(stat.st_mode & libc::S_IFMT == libc::S_IFIFO)
    }

    /// Copied from `processctl::lock::consume_credential_stdin`.
    /// The pipe is drained to EOF and stdin is then REPLACED by `/dev/null`:
    /// the credential must not be re-readable, and fd 0 must not be left closed
    /// (the next `open` would silently become this process's stdin). The
    /// `null_fd != 0` arm is the case where `/dev/null` lands on fd 0 by itself.
    pub(super) fn consume_credential_stdin() -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        use std::os::fd::{FromRawFd, IntoRawFd};

        if unsafe { libc::fcntl(0, libc::F_GETFD) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut input = unsafe { File::from_raw_fd(0) };
        let mut bytes = Vec::new();
        let read = input
            .by_ref()
            .take(super::MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes);
        drop(input);
        read?;

        let null = File::open("/dev/null")?;
        let null_fd = null.into_raw_fd();
        if null_fd != 0 {
            let duplicated = unsafe { libc::dup2(null_fd, 0) };
            unsafe { libc::close(null_fd) };
            if duplicated < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(0, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(bytes)
    }

    /// The one-shot claim: `O_CREAT | O_EXCL` is the whole mechanism — the
    /// SECOND borrower of the same lease loses the create and is refused.
    /// Contents and mode are processctl's (`create_consumption_marker`,
    /// its Linux arm) because the OWNER deletes this
    /// file on its own drop and only after re-reading exactly these bytes from a
    /// file at exactly mode 0600.
    ///
    /// `.mode(0o600)` is only a REQUEST — the umask masks it, and a umask
    /// carrying owner-triad bits would land this at e.g. 0500, which the owner's
    /// `cleanup_consumption_marker` refuses to delete forever. So the mode is
    /// SET, then verified, mirroring what processctl does for the lock file
    /// itself (`open_lock_file`, `:669-676`: create, `set_permissions`,
    /// `validate_private_regular_linux`). That is a self-check on what we just
    /// wrote, not the `O_NOFOLLOW`-class same-user hardening deliberately not
    /// copied (CLAUDE.md, "Dev tooling scope": trusted local operator).
    pub(super) fn create_consumption_marker(path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mode = file.metadata()?.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(std::io::Error::other(format!(
                "one-shot borrower marker {} is mode {mode:04o}, not 0600 — its owner would never \
                 reap it",
                path.display()
            )));
        }
        file.write_all(super::CONSUMED_MARKER)?;
        file.sync_all()
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, TokenUser,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, LockFileEx, UnlockFileEx, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
        FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, OPEN_ALWAYS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Opens (or creates, carrying the owner-only protected DACL) the lock
    /// file with `FILE_SHARE_READ | FILE_SHARE_WRITE` — the same sharing mode
    /// processctl/devctl use, so concurrent opens contend only on the
    /// byte-range lock, never on the open itself. `SECURITY_ATTRIBUTES` only
    /// applies when the file is actually created; an existing file keeps the
    /// DACL its creator gave it.
    pub(super) fn open_lock_file(path: &Path) -> std::io::Result<File> {
        let security = OwnerOnlySecurity::new()?;
        let path = wide_path(path)?;
        let attributes = security.attributes();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    /// `LockFileEx` on EXACTLY 1 byte at offset `1 << 63` — bit-compatible
    /// with `tools/processctl/src/lock.rs::try_lock_exclusive`. Any other
    /// range would not contend with a devctl/verifyctl rollout at all.
    pub(super) fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        let mut overlapped = lock_overlapped();
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if result != 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        const ERROR_LOCK_VIOLATION: i32 = 33;
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn unlock(file: &File) -> std::io::Result<()> {
        let mut overlapped = lock_overlapped();
        if unsafe { UnlockFileEx(file.as_raw_handle() as _, 0, 1, 0, &mut overlapped) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Copied from `processctl::platform::windows::observe_process_identity` +
    /// `observe_process` (`tools/processctl/src/platform/windows.rs`).
    /// The process creation FILETIME is the half of the identity a recycled PID
    /// cannot forge; `PROCESS_QUERY_LIMITED_INFORMATION` is enough for both
    /// queries and is the least the parent can be opened with.
    pub(super) fn observe_process_identity(pid: u32) -> std::io::Result<super::OwnerIdentity> {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let identity = (|| {
            let mut path = vec![0u16; 32_768];
            let mut len = path.len() as u32;
            if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            path.truncate(len as usize);

            let mut created: FILETIME = unsafe { std::mem::zeroed() };
            let mut exited: FILETIME = unsafe { std::mem::zeroed() };
            let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
            let mut user: FILETIME = unsafe { std::mem::zeroed() };
            if unsafe {
                GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let marker =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            Ok(super::OwnerIdentity {
                pid,
                executable: std::path::PathBuf::from(std::ffi::OsString::from_wide(&path)),
                started: super::StartMarker(marker),
            })
        })();
        unsafe { CloseHandle(process) };
        identity
    }

    /// Copied from `processctl::lock::inherited_credential_present`
    /// (`processctl::lock::inherited_credential_present`): the parent hands the credential
    /// over an anonymous pipe on stdin, so `FILE_TYPE_PIPE` is exactly the
    /// question. An operator shell leaves stdin a console or a file.
    pub(super) fn inherited_credential_present() -> std::io::Result<bool> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileType, FILE_TYPE_PIPE, FILE_TYPE_UNKNOWN,
        };
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};

        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Ok(false);
        }
        let kind = unsafe { GetFileType(handle) };
        if kind == FILE_TYPE_UNKNOWN {
            // FILE_TYPE_UNKNOWN is ambiguous: it is also the legitimate answer
            // for an exotic-but-valid handle, which GetFileType reports with
            // GetLastError left at NO_ERROR.
            let error = std::io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(error);
            }
        }
        Ok(kind == FILE_TYPE_PIPE)
    }

    /// Copied from `processctl::lock::consume_credential_stdin` +
    /// `install_consumed_stdin`). The
    /// pipe is drained to EOF and stdin is then REPLACED by `NUL`: the
    /// credential must not be re-readable, and stdin must not be left dangling
    /// for the 12-process fleet this supervisor is about to spawn. The `NUL`
    /// file is parked in a `OnceLock` because dropping it would close the very
    /// handle just installed as `STD_INPUT_HANDLE`.
    pub(super) fn consume_credential_stdin() -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};

        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut input = unsafe { File::from_raw_handle(handle) };
        let mut bytes = Vec::new();
        let read = input
            .by_ref()
            .take(super::MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes);
        drop(input);
        read?;
        install_consumed_stdin()?;
        Ok(bytes)
    }

    fn install_consumed_stdin() -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
        use windows_sys::Win32::System::Console::{SetStdHandle, STD_INPUT_HANDLE};

        let nul: Vec<u16> = "NUL\0".encode_utf16().collect();
        let handle = unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(handle) };
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0
            || unsafe { SetStdHandle(STD_INPUT_HANDLE, handle) } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        CONSUMED_STDIN.set(file).map_err(|_| {
            std::io::Error::other("the retained borrower stdin was already installed")
        })
    }

    /// Keeps the replacement `NUL` stdin handle alive for the life of the
    /// process (`processctl::lock::CONSUMED_STDIN`).
    static CONSUMED_STDIN: std::sync::OnceLock<File> = std::sync::OnceLock::new();

    /// The one-shot claim: `CREATE_NEW` is the whole mechanism — the SECOND
    /// borrower of the same lease loses the create (`ERROR_FILE_EXISTS`, which
    /// std maps to `ErrorKind::AlreadyExists`) and is refused. Contents, sharing
    /// mode and the owner-only DACL are processctl's
    /// (`create_consumption_marker`/`super_private_create_new`,
    /// its Windows arm) because the OWNER deletes this
    /// file on its own drop and only after re-reading exactly these bytes from a
    /// file it still validates as owner-only. processctl's post-create
    /// re-validation is same-user hardening, deliberately not copied (CLAUDE.md,
    /// "Dev tooling scope": trusted local operator).
    pub(super) fn create_consumption_marker(path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, FILE_FLAG_OPEN_REPARSE_POINT};

        let security = OwnerOnlySecurity::new()?;
        let wide = wide_path(path)?;
        let attributes = security.attributes();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_handle(handle) };
        file.write_all(super::CONSUMED_MARKER)?;
        file.sync_all()
    }

    fn lock_overlapped() -> windows_sys::Win32::System::IO::OVERLAPPED {
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
            unsafe { std::mem::zeroed() };
        let offset = 1u64 << 63;
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        overlapped
    }

    /// Owner-only, protected security descriptor
    /// (`O:<sid>D:P(A;;GA;;;<sid>)`) applied when weles CREATES the lock
    /// file. Copied from `tools/processctl/src/state.rs::OwnerOnlySecurity`
    /// so a weles-created lock file passes processctl/devctl's later
    /// owner/DACL validation.
    struct OwnerOnlySecurity {
        descriptor: PSECURITY_DESCRIPTOR,
    }

    impl OwnerOnlySecurity {
        fn new() -> std::io::Result<Self> {
            use std::os::windows::ffi::OsStrExt;

            let sid = current_user_sid_string()?;
            let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
            let sddl: Vec<u16> = std::ffi::OsStr::new(&sddl)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            // Sanity: the SDDL round-trip must have produced both a DACL and
            // an owner (guards against a silent SDDL formatting mistake).
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            if unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            } == 0
                || present == 0
                || dacl.is_null()
            {
                unsafe { LocalFree(descriptor as _) };
                return Err(std::io::Error::last_os_error());
            }
            let mut owner = std::ptr::null_mut();
            let mut owner_defaulted = 0;
            if unsafe {
                GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted)
            } == 0
                || owner.is_null()
            {
                unsafe { LocalFree(descriptor as _) };
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { descriptor })
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            }
        }
    }

    impl Drop for OwnerOnlySecurity {
        fn drop(&mut self) {
            unsafe { LocalFree(self.descriptor as _) };
        }
    }

    fn current_user_sid_string() -> std::io::Result<String> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut required = 0;
            unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
            };
            if required == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let words = required.div_ceil(std::mem::size_of::<usize>() as u32) as usize;
            let mut buffer = vec![0usize; words];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
            sid_to_string(user.User.Sid)
        })();
        unsafe { CloseHandle(token) };
        result
    }

    fn sid_to_string(sid: PSID) -> std::io::Result<String> {
        let mut sid_string = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut sid_string) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let length = (0..)
            .find(|&index| unsafe { *sid_string.add(index) } == 0)
            .expect("Windows SID string is NUL terminated");
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_string, length) })
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        unsafe { LocalFree(sid_string.cast()) };
        result
    }

    fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lock path contains NUL",
            ));
        }
        wide.push(0);
        Ok(wide)
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;

//! Supervisor fleet state persisted at `run/weles/state.json` — weles's OWN
//! schema (nothing else reads it today; `weles status`/`down` join in M0
//! Step 6). Checkpoints are atomic: write to a `.tmp` sibling, then rename
//! over the real file, so a crash mid-write can never leave a torn JSON
//! document behind. No fsync — dev tooling threat model (a power cut losing
//! the last checkpoint is acceptable; a torn file is not).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Where one supervised service currently is in its lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Status {
    /// Not yet spawned (boot has not reached it).
    Starting,
    /// Spawned; waiting for `/readyz` to turn 200.
    WaitingHealthy,
    Healthy,
    /// Crashed; waiting out the exponential backoff before a respawn.
    Backoff,
    /// Backoff elapsed; the respawn is happening right now.
    Restarting,
    /// Gave up after too many consecutive failures — the rest of the fleet
    /// keeps running (weles's differentiator vs devctl).
    Failed,
    /// Teardown is stopping this service right now.
    Stopping,
    /// Was already dead (crash/backoff) when teardown reached it.
    Exited,
    /// Stopped by teardown (or never spawned when teardown ran).
    Stopped,
}

/// The whole fleet's lifecycle status (distinct from a single service's
/// [`Status`]) — lets a `weles status`/`down` client tell a running fleet from
/// one that is tearing down or already finished. Set by the supervisor:
/// `Starting` while booting, `Running` once every service is healthy,
/// `Stopping` when teardown begins, and the terminal `Stopped`/`Failed` at the
/// end (a boot failure lands `Failed`, an ordinary stop lands `Stopped`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FleetStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl FleetStatus {
    /// A terminal status means the supervisor has finished and no control
    /// endpoint is live — a `weles down` client stops polling here.
    pub fn is_terminal(self) -> bool {
        matches!(self, FleetStatus::Stopped | FleetStatus::Failed)
    }
}

/// The supervisor process's own identity, recorded so a later
/// `weles status`/`down` can tell a live supervisor from a stale file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_unix: u64,
}

/// Post-healthy freshness of a service's `/readyz`, a dimension SEPARATE from
/// [`Status`]: `Status` is the restart lifecycle (a probe never drives it),
/// `Readiness` is what the out-of-band poller last saw. A `Degraded`/`Unreachable`
/// readiness NEVER restarts the service — it is recorded and rendered only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Readiness {
    /// Not yet probed, or the service is not `Healthy` (the poller only probes
    /// `Healthy` services and blanks the rest).
    #[default]
    Unknown,
    /// Last probe answered `/readyz` 200 ([`crate::health::ProbeResult::Ready`]).
    Ready,
    /// Last probe connected but did not answer 200 — a 503 while degraded, or a
    /// torn response ([`crate::health::ProbeResult::NotReady`]).
    Degraded,
    /// Last probe could not connect at all
    /// ([`crate::health::ProbeResult::ConnectFailed`]).
    Unreachable,
}

/// One supervised service's checkpointed state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceState {
    pub name: String,
    pub status: Status,
    pub pid: Option<u32>,
    pub restarts: u32,
    /// Post-healthy `/readyz` freshness (checkpoint-only; never a restart input).
    #[serde(default)]
    pub readiness: Readiness,
}

/// The whole checkpointed fleet.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FleetState {
    pub run_id: String,
    pub supervisor: ProcessIdentity,
    pub fleet_label: String,
    /// The fleet's lifecycle status — the top-level authority a `weles down`
    /// client polls for a terminal transition.
    pub status: FleetStatus,
    /// Bounded loopback control endpoint (named pipe / UDS path). Published
    /// BEFORE boot — the supervisor binds the control server first (so `weles
    /// status`/`down` reach the fleet DURING startup, status still `Starting`),
    /// then boots. `None` only in the narrow prep window between the first
    /// `Starting` checkpoint and the post-bind checkpoint.
    pub control_endpoint: Option<String>,
    /// The deploy generation (`gen-N`) this supervisor pinned at
    /// `Layout::discover` and is executing for its whole life. Recorded so a
    /// concurrent `weles deploy`'s retention can PROTECT the live-pinned
    /// generation by NAME — the one-up-at-a-time invariant means at most one
    /// supervisor pins one generation (every `weles up` pins exactly one,
    /// whatever fleet it booted). `None` for a legacy state file written before
    /// this field existed (serde-defaulted).
    #[serde(default)]
    pub pinned_generation: Option<String>,
    pub services: Vec<ServiceState>,
}

/// Atomically checkpoints `state` to `path`: serialize, write the whole
/// document to `<path>.tmp`, rename over `path` (std's rename replaces an
/// existing destination on both platforms). A stale `.tmp` left by an earlier
/// crash is simply overwritten.
pub fn checkpoint(path: &Path, state: &FleetState) -> Result<()> {
    let file_name = path
        .file_name()
        .with_context(|| format!("state path {} has no file name", path.display()))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);

    let json = serde_json::to_vec_pretty(state).context("serialize fleet state")?;
    std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} over {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Loads and parses the checkpointed fleet state from `path`. `Ok(None)` means
/// no state file exists yet (no fleet has ever run in this workspace); `Err`
/// is reserved for an unreadable or malformed file — the caller distinguishes
/// "nothing recorded" from "recorded but broken".
pub fn load(path: &Path) -> Result<Option<FleetState>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse fleet state at {}", path.display()))?;
    Ok(Some(state))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;

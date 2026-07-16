//! Owned foreground fleet supervisor — the heart of weles and its
//! differentiator vs devctl: a crashed service is RESTARTED in place with
//! exponential backoff (devctl tears the whole fleet down on any crash), and
//! a service that keeps failing is given up on ALONE while the rest of the
//! fleet keeps running.
//!
//! `run_up` sequence (the rollout lock comes FIRST — before any validation or
//! prep — per the Step-4 review finding): (1) discover layout + acquire
//! `run/rollout.lock`, then install the Ctrl-C/SIGTERM handler immediately (P1:
//! so an interrupt during the slow prep window can't orphan a helper on the
//! unwind); (2) validate the manifest against `cmd/*-svc` on disk
//! (drift reported AS drift), then the DEPLOYED binaries in `<root>/deploy`
//! (weles never builds), then the Postgres session budget; (3) prep (mint CA,
//! seed admin); (4) boot each service in manifest order behind a
//! readyz gate; (5) a single non-blocking monitor loop, with an out-of-band
//! `weles-readiness` poller thread recording post-healthy `/readyz` freshness
//! (a checkpoint-only dimension that NEVER restarts a service); (6) teardown in
//! reverse spawn order, lock dropped last.
//!
//! Every restart DECISION is a pure function over an injected `now`
//! ([`next_restart`], [`step`]) — the loop merely executes directives — so
//! the crash → backoff → respawn / give-up policy is unit-testable without
//! real processes or a real clock (timing-tests doctrine).

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::cli::Topology;
use crate::control::ControlServer;
use crate::health::{self, ProbeResult};
use crate::lock;
use crate::manifest::{self, RuntimeInputs, ServiceDef};
use crate::platform::{self, Outcome, OwnedProc, SpawnSpec};
use crate::prep;
use crate::state::{self, FleetState, FleetStatus, ProcessIdentity, Readiness, ServiceState, Status};

/// How long a service gets to turn `/readyz` 200 after every (re)spawn.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(30);
/// The Nth consecutive failure gives up on the service (→ `Failed`; the rest
/// of the fleet keeps running).
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;
/// Continuous `Healthy` time after which the consecutive-failure counter
/// resets (a service that ran fine for a minute earned a fresh backoff).
pub const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(60);
/// Exponential-backoff schedule: 1s, 2s, 4s, … capped here.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

const BOOT_PROBE_INTERVAL: Duration = Duration::from_millis(250);
const MONITOR_TICK: Duration = Duration::from_millis(100);
/// The readiness poller issues ONE `/readyz` probe per this interval, round-robin
/// across the currently-`Healthy` services (see [`readiness_poller`]). Kept off
/// the monitor thread precisely so this (blocking, up to ~800ms) probe never
/// delays crash detection or the `down`/Ctrl-C response.
const READINESS_PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// Chunk the poller's inter-probe sleep so it observes its own shutdown flag
/// promptly (join before teardown) without a probe-length wait.
const READINESS_STOP_POLL: Duration = Duration::from_millis(50);
/// Teardown per-service shutdown budget (graceful, then force).
const STOP_GRACE: Duration = Duration::from_secs(5);
const STOP_FORCE: Duration = Duration::from_secs(5);
/// A not-yet-healthy service that blew its deadline gets no graceful
/// patience — it already proved unresponsive.
const HUNG_GRACE: Duration = Duration::from_secs(0);
const HUNG_FORCE: Duration = Duration::from_secs(5);

/// Flipped (only) by the Ctrl-C/SIGTERM handler; observed at the top of every
/// boot step and monitor tick, and inside every respawn decision. The OS
/// signal handler can touch ONLY a static atomic, so this stays a static.
static STOP: AtomicBool = AtomicBool::new(false);

/// A stop is requested when EITHER the signal handler flipped the process-wide
/// `STOP` static OR a `weles down` request flipped this run's `fleet_stop`
/// atomic. `fleet_stop` is threaded as an `Arc` (owned by the control server
/// for the life of the run) rather than a process-global `OnceLock`: a global
/// set before boot would bleed a stale stop flag into a second `run_up` in the
/// same process (the test harness), and the control thread must own the sole
/// non-signal writer of the fleet stop.
fn stop_requested(fleet_stop: &AtomicBool) -> bool {
    STOP.load(Ordering::SeqCst) || fleet_stop.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Pure restart policy (no I/O, no real clock — everything flows through `now`)
// ---------------------------------------------------------------------------

/// Crash bookkeeping for one service. Pure data: [`next_restart`]/[`step`]
/// read it, `record_crash`/`record_healthy` are the only writers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestartHistory {
    /// Failures since the last 60s-continuous-Healthy reset.
    pub consecutive_failures: u32,
    /// When the service last ENTERED `Healthy` (and has stayed there since —
    /// cleared on every crash), `None` while it is not healthy.
    pub healthy_since: Option<Instant>,
}

impl RestartHistory {
    /// The consecutive-failure count a crash AT `now` amounts to: the prior
    /// count (reset to zero if the service had been continuously healthy for
    /// [`HEALTHY_RESET_AFTER`]) plus this crash.
    fn failures_after_crash(&self, now: Instant) -> u32 {
        let prior = match self.healthy_since {
            Some(since) if now.duration_since(since) >= HEALTHY_RESET_AFTER => 0,
            _ => self.consecutive_failures,
        };
        prior + 1
    }

    /// Records a crash observed at `now` (applies the healthy-reset first).
    pub fn record_crash(&mut self, now: Instant) {
        self.consecutive_failures = self.failures_after_crash(now);
        self.healthy_since = None;
    }

    /// Records the service turning healthy at `now`.
    pub fn record_healthy(&mut self, now: Instant) {
        self.healthy_since = Some(now);
    }
}

/// What to do about a crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Respawn once `Instant` is reached.
    RespawnAt(Instant),
    /// Too many consecutive failures — mark `Failed`, keep the fleet running.
    GiveUp,
}

/// The backoff before respawn number `consecutive_failures`: 1s, 2s, 4s, 8s,
/// … capped at [`BACKOFF_CAP`]. Total, overflow-free, and monotone.
pub fn backoff_delay(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(31);
    let seconds = 1u64
        .checked_shl(exponent)
        .unwrap_or(u64::MAX)
        .min(BACKOFF_CAP.as_secs());
    Duration::from_secs(seconds)
}

/// PURE decision for a crash observed at `now`, given the history as it stood
/// BEFORE this crash: exponential backoff until the
/// [`MAX_CONSECUTIVE_FAILURES`]th consecutive failure, which gives up.
/// The caller applies the crash with [`RestartHistory::record_crash`].
pub fn next_restart(history: &RestartHistory, now: Instant) -> Decision {
    let failures = history.failures_after_crash(now);
    if failures >= MAX_CONSECUTIVE_FAILURES {
        Decision::GiveUp
    } else {
        Decision::RespawnAt(now + backoff_delay(failures))
    }
}

/// Where one supervised service is in the monitor state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Spawned; readyz must turn 200 before `deadline`.
    WaitingHealthy { deadline: Instant },
    Healthy { healthy_since: Instant },
    /// Crashed; respawn once `respawn_at` is reached (unless STOP).
    Backoff { respawn_at: Instant },
    /// Given up. Terminal until teardown.
    Failed,
}

/// What the monitor loop observed for one service this tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observed {
    /// `try_wait` returned `None` (process alive; no probe verdict).
    Alive,
    /// The process is gone (`try_wait` returned an exit, or there is no
    /// process at all — `Backoff`/`Failed` phases).
    Exited,
    /// Alive AND the readyz probe answered 200 (`WaitingHealthy` only).
    Ready,
    /// Alive but the readyz probe did not answer 200 (`WaitingHealthy` only).
    NotReady,
}

/// What the loop must DO for one service after a [`step`] decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Directive {
    /// Adopt the phase (which may equal the current one — no-op tick).
    Stay(Phase),
    /// Backoff elapsed and STOP is not set: respawn the process now.
    Respawn,
    /// The (live but hung) process must be killed, then adopt the phase.
    Kill(Phase),
}

/// PURE per-service transition: current phase + this tick's observation +
/// the STOP flag + `now` → directive. Mutates only `history` (crash/healthy
/// bookkeeping). All side effects (spawning, killing, checkpointing) are the
/// caller's job.
pub fn step(
    phase: Phase,
    observed: Observed,
    stop: bool,
    now: Instant,
    history: &mut RestartHistory,
) -> Directive {
    match phase {
        Phase::Failed => Directive::Stay(Phase::Failed),
        Phase::Backoff { respawn_at } => {
            // STOP mid-backoff must yield ZERO respawns: teardown is about to
            // run and a fresh child would race it.
            if !stop && now >= respawn_at {
                Directive::Respawn
            } else {
                Directive::Stay(phase)
            }
        }
        Phase::Healthy { .. } => match observed {
            Observed::Exited => Directive::Stay(crash_phase(history, now)),
            _ => Directive::Stay(phase),
        },
        Phase::WaitingHealthy { deadline } => match observed {
            Observed::Exited => Directive::Stay(crash_phase(history, now)),
            Observed::Ready => {
                history.record_healthy(now);
                Directive::Stay(Phase::Healthy { healthy_since: now })
            }
            Observed::Alive | Observed::NotReady => {
                if now >= deadline {
                    // Alive but never turned healthy: that counts as a
                    // failure, and the hung process must be killed first.
                    Directive::Kill(crash_phase(history, now))
                } else {
                    Directive::Stay(phase)
                }
            }
        },
    }
}

/// Applies a crash at `now` to `history` and returns the phase it lands in
/// (`Backoff` or `Failed`).
fn crash_phase(history: &mut RestartHistory, now: Instant) -> Phase {
    let decision = next_restart(history, now);
    history.record_crash(now);
    match decision {
        Decision::RespawnAt(respawn_at) => Phase::Backoff { respawn_at },
        Decision::GiveUp => Phase::Failed,
    }
}

fn status_of(phase: Phase) -> Status {
    match phase {
        Phase::WaitingHealthy { .. } => Status::WaitingHealthy,
        Phase::Healthy { .. } => Status::Healthy,
        Phase::Backoff { .. } => Status::Backoff,
        Phase::Failed => Status::Failed,
    }
}

/// PURE map from a teardown `shutdown` result to (the service's checkpoint
/// status, whether the stop was CLEAN — the process is confirmed gone). This is
/// the ONE authority deciding a teardown outcome's accuracy; `teardown` folds
/// the bool into the fleet's exit code (devctl parity: an unconfirmed stop is a
/// non-zero exit, not a silent `Stopped`).
///
/// `Forced` MUST count as clean: `Outcome::Forced` is returned only AFTER
/// `force()` + a `wait_for(force_timeout)` that CONFIRMED the exit
/// (`platform/mod.rs`), so it is a real stop, not an orphan. Critically, a
/// console-less weles on Windows cannot deliver CTRL_BREAK and therefore
/// DEGRADES EVERY shutdown to `Forced` (`platform/mod.rs`); flagging `Forced` as
/// unclean would give every such stop a false non-zero exit. Only an `Err` —
/// force could not confirm the exit — is unclean: the process may be orphaned,
/// so the service is `Failed` and the fleet's exit is non-zero.
fn stop_outcome(result: &Result<Outcome>) -> (Status, bool) {
    match result {
        Ok(Outcome::Graceful(_) | Outcome::Forced(_)) => (Status::Stopped, true),
        Err(_) => (Status::Failed, false),
    }
}

// ---------------------------------------------------------------------------
// Readiness: a post-healthy `/readyz` freshness dimension, structurally
// DISJOINT from the restart decision. The three pure functions below are the
// whole authority — none of them can touch `phase`/`status`/`history`, so a
// 503/torn/unreachable probe records `Degraded`/`Unreachable` and NOTHING else.
// The poller writes into a shared Vec; the monitor folds that Vec into each
// `Supervised.readiness` for the checkpoint; `observe`/`step` never see a probe.
// ---------------------------------------------------------------------------

/// Maps a single readiness probe to the recorded [`Readiness`]. This is the ONE
/// place a `ProbeResult` becomes a readiness verdict — and it produces nothing
/// but a `Readiness`, so no probe outcome can ever synthesize an `Observed` or a
/// restart `Directive`.
fn readiness_for(probe: ProbeResult) -> Readiness {
    match probe {
        ProbeResult::Ready => Readiness::Ready,
        ProbeResult::NotReady => Readiness::Degraded,
        ProbeResult::ConnectFailed => Readiness::Unreachable,
    }
}

/// The round-robin cursor: the first `Healthy` index strictly after `cursor`,
/// wrapping once, or `None` when no service is `Healthy` (including an empty
/// fleet — no div-by-zero, no panic). Non-`Healthy` indices are skipped so the
/// poller only ever probes a service the supervisor believes is up.
fn next_probe_index(healthy: &[bool], cursor: usize) -> Option<usize> {
    let len = healthy.len();
    if len == 0 {
        return None;
    }
    (1..=len)
        .map(|offset| (cursor + offset) % len)
        .find(|&idx| healthy[idx])
}

/// Folds the poller's latest readiness verdicts into the fleet, returning whether
/// anything changed (→ a checkpoint so `weles status` sees the freshness). This
/// is the ENTIRE effect a probe has on supervised state: it writes ONLY
/// `readiness`. It has no access to `phase`, `status`, `history`, or any
/// `Directive`, so a readiness change is provably not a restart input — the
/// authority that keeps "503 never restarts" true.
fn fold_readiness(fleet: &mut [Supervised], latest: &[Readiness]) -> bool {
    let mut changed = false;
    for (svc, &readiness) in fleet.iter_mut().zip(latest) {
        if svc.readiness != readiness {
            svc.readiness = readiness;
            changed = true;
        }
    }
    changed
}

/// Owns the `weles-readiness` poller thread; dropping it stops and joins it.
///
/// Runs on its OWN thread (never the monitor thread) so a blocking `/readyz`
/// probe (up to ~800ms for a hung service) cannot delay crash detection, the
/// respawn of another service, or the `down`/Ctrl-C response — the monitor tick
/// must stay non-blocking. Stop-authority: it flows through a PRIVATE `shutdown`
/// atomic (flipped on `Drop`), NEVER the fleet stop — a poller lifecycle event
/// must never look like an operator `down`.
struct ReadinessPoller {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ReadinessPoller {
    /// Spawns the poller. `shared` is read (never written) to learn which
    /// indices are `Healthy`; `readiness` (indexed like the fleet) is the poller's
    /// write target and the monitor's read source; `ports` maps each index to its
    /// `/readyz` port.
    fn spawn(
        shared: Arc<Mutex<FleetState>>,
        readiness: Arc<Mutex<Vec<Readiness>>>,
        ports: Vec<u16>,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name("weles-readiness".into())
            .spawn(move || readiness_poller(&shared, &readiness, &ports, &thread_shutdown))
            .ok();
        ReadinessPoller { shutdown, thread }
    }
}

impl Drop for ReadinessPoller {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The poller loop: each [`READINESS_PROBE_INTERVAL`], read the `Healthy` mask
/// from `shared`, blank every non-`Healthy` index to `Unknown`, then issue ONE
/// probe against the next `Healthy` index (round-robin via [`next_probe_index`])
/// and record its verdict. Stops promptly on the private `shutdown` flag (checked
/// between chunked sleeps, so join never waits a whole probe interval).
fn readiness_poller(
    shared: &Arc<Mutex<FleetState>>,
    readiness: &Arc<Mutex<Vec<Readiness>>>,
    ports: &[u16],
    shutdown: &AtomicBool,
) {
    // Start the cursor at the last index so the first probe lands on index 0.
    let mut cursor = ports.len().saturating_sub(1);
    while !shutdown.load(Ordering::SeqCst) {
        // Snapshot which indices the supervisor currently believes are Healthy.
        let healthy: Vec<bool> = {
            let state = shared.lock().expect("state mutex poisoned");
            (0..ports.len())
                .map(|i| {
                    state
                        .services
                        .get(i)
                        .is_some_and(|svc| svc.status == Status::Healthy)
                })
                .collect()
        };
        // Non-Healthy services have no meaningful readiness: blank them so a
        // stale `Degraded` from before a crash can't linger in `weles status`.
        {
            let mut slots = readiness.lock().expect("readiness mutex poisoned");
            for (i, is_healthy) in healthy.iter().enumerate() {
                if !is_healthy {
                    if let Some(slot) = slots.get_mut(i) {
                        *slot = Readiness::Unknown;
                    }
                }
            }
        }
        // One probe per interval, round-robin over the Healthy services.
        if let Some(index) = next_probe_index(&healthy, cursor) {
            cursor = index;
            let verdict = readiness_for(health::probe(ports[index]));
            if let Some(slot) = readiness
                .lock()
                .expect("readiness mutex poisoned")
                .get_mut(index)
            {
                *slot = verdict;
            }
        }
        sleep_until_stopped(shutdown, READINESS_PROBE_INTERVAL);
    }
}

/// Sleeps up to `total`, waking early (within [`READINESS_STOP_POLL`]) once
/// `shutdown` is set — so the poller joins promptly at teardown.
fn sleep_until_stopped(shutdown: &AtomicBool, total: Duration) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(READINESS_STOP_POLL));
    }
}

// ---------------------------------------------------------------------------
// The supervisor proper (thin execution around the pure policy)
// ---------------------------------------------------------------------------

struct Supervised {
    def: ServiceDef,
    proc: Option<OwnedProc>,
    /// `None` until boot spawns it (a boot abort leaves later services here).
    phase: Option<Phase>,
    history: RestartHistory,
    /// The checkpointed status — a superset of `phase` (adds the boot and
    /// teardown vocabulary: Starting/Restarting/Stopping/Exited/Stopped).
    status: Status,
    restarts: u32,
    /// Post-healthy `/readyz` freshness, folded in from the out-of-band
    /// readiness poller ([`fold_readiness`]). A checkpoint-only dimension —
    /// structurally disjoint from `phase`/`status`/`history`, so a `Degraded`
    /// probe can never restart the service.
    readiness: Readiness,
}

impl Supervised {
    fn new(def: ServiceDef) -> Self {
        Supervised {
            def,
            proc: None,
            phase: None,
            history: RestartHistory::default(),
            status: Status::Starting,
            restarts: 0,
            readiness: Readiness::Unknown,
        }
    }
}

/// Owns everything a checkpoint needs besides the fleet itself. Checkpoint
/// failures are reported but never take the fleet down. The mutable
/// fleet-level status and control endpoint live here (single-threaded interior
/// mutability — the `Reporter` is only ever touched by the supervisor thread);
/// `shared` is the ONE cross-thread handle: the control thread reads it to
/// render a `status` reply.
struct Reporter {
    state_path: PathBuf,
    run_id: String,
    topology: &'static str,
    supervisor: ProcessIdentity,
    /// The `gen-N` this fleet pinned at `Layout::discover`, recorded into every
    /// checkpoint so a concurrent `weles deploy` protects it from retention.
    pinned_generation: Option<String>,
    status: Cell<FleetStatus>,
    control_endpoint: RefCell<Option<String>>,
    /// The last checkpointed snapshot, republished on every `checkpoint` so a
    /// `weles status` reply reads current in-memory state without adding any
    /// blocking to the monitor tick (a brief lock to clone in / out).
    shared: Arc<Mutex<FleetState>>,
}

impl Reporter {
    fn set_status(&self, status: FleetStatus) {
        self.status.set(status);
    }

    fn set_control_endpoint(&self, endpoint: Option<String>) {
        *self.control_endpoint.borrow_mut() = endpoint;
    }

    fn shared(&self) -> Arc<Mutex<FleetState>> {
        Arc::clone(&self.shared)
    }

    fn snapshot(&self, fleet: &[Supervised]) -> FleetState {
        FleetState {
            run_id: self.run_id.clone(),
            supervisor: self.supervisor,
            topology: self.topology.to_string(),
            status: self.status.get(),
            control_endpoint: self.control_endpoint.borrow().clone(),
            pinned_generation: self.pinned_generation.clone(),
            services: fleet
                .iter()
                .map(|svc| ServiceState {
                    name: svc.def.name.to_string(),
                    status: svc.status,
                    pid: svc.proc.as_ref().map(OwnedProc::pid),
                    restarts: svc.restarts,
                    readiness: svc.readiness,
                })
                .collect(),
        }
    }

    fn checkpoint(&self, fleet: &[Supervised]) {
        let snapshot = self.snapshot(fleet);
        // Publish the in-memory snapshot for the control thread FIRST (a status
        // reply must never lag the persisted file), then persist.
        *self.shared.lock().expect("state mutex poisoned") = snapshot.clone();
        if let Err(error) = state::checkpoint(&self.state_path, &snapshot) {
            eprintln!(
                "weles: state checkpoint to {} failed: {error:#}",
                self.state_path.display()
            );
        }
    }
}

/// Discovers the workspace layout from weles's own crate location. weles's
/// Cargo.toml sits directly at the repo root (unlike tools/*), so the workspace
/// root is exactly one parent up. Shared by `up` and `deploy`.
pub fn discover_layout() -> Result<prep::Layout> {
    prep::Layout::discover(workspace_root()?)
}

/// Like [`discover_layout`] but for the `deploy` path: does NOT require a
/// pinned generation (`deploy/current`), so a fresh checkout can run its first
/// `weles deploy`.
pub fn discover_layout_for_deploy() -> Result<prep::Layout> {
    prep::Layout::discover_for_deploy(workspace_root()?)
}

fn workspace_root() -> Result<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("weles crate has no parent directory")?
        .to_path_buf())
}

/// The whole `weles up` lifecycle. Returns when the fleet has been torn down
/// (operator stop) or a boot failure was unwound.
pub fn run_up(topology: Topology) -> Result<()> {
    let layout = discover_layout()?;

    // Lock FIRST (Step-4 review finding): nothing rollout-bearing may run
    // before this process owns run/rollout.lock.
    let run_id = format!("{:016x}", rand::random::<u64>());
    let _lock = lock::acquire(&layout.root, &run_id)?;

    // Install the stop handler immediately after acquiring the lock — BEFORE the
    // slow prep helpers (mint_ca can spawn edgeca for ~30s if the CA is absent;
    // seed_admin does a Postgres round-trip). A Ctrl-C/SIGTERM in that ~60s prep
    // window would otherwise take the OS default disposition; installed here it
    // flips `STOP`, which `boot` observes at the top of every step. The benefit
    // is orphan PREVENTION on the unwind (both prep helpers ignore `STOP` and run
    // to completion, so the stop is deferred to the end of the window — this is
    // not about responsiveness), and is platform-asymmetric: on Windows the
    // helpers share the console and largely die with an unhandled Ctrl-C anyway;
    // the real orphan is a Unix helper in its own group when weles dies unreaped.
    // Reset the shared static FIRST: `STOP` is process-global, so a prior `run_up`
    // in the same process (the test harness) could have left it set (devctl resets
    // INTERRUPTED the same way before installing its handler).
    STOP.store(false, Ordering::SeqCst);
    install_ctrl_handler()?;

    // Record the pinned generation at the EARLIEST safe point — right after the
    // lock + discover, BEFORE the slow prep helpers (mint_ca can spawn edgeca
    // for ~30s if the CA is absent; seed_admin does a Postgres round-trip). The
    // pin is available from `layout` here, and this early `Starting` checkpoint
    // carries the live pid + `pinned_generation`, so a concurrent `weles deploy`
    // sees the live pin and won't prune this booting up's generation. Without
    // this, the pin was invisible across the whole helper window (state.json
    // absent or stale/terminal), and a deploy could delete the booting up's
    // gen-N out from under it (a loud spawn failure, not silent loss, but still
    // this fix's own new seam). The control endpoint is still bound later, before
    // boot (Part A) — only this state write moves ahead of the helpers.
    let topology_name = match topology {
        Topology::Split => "split",
        Topology::Monolith => "monolith",
    };
    let supervisor = ProcessIdentity {
        pid: std::process::id(),
        started_unix: unix_now(),
    };
    let pinned_generation = layout.pinned_generation();
    let reporter = Reporter {
        state_path: layout.run_dir.join("state.json"),
        run_id,
        topology: topology_name,
        supervisor,
        pinned_generation: pinned_generation.clone(),
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        // Placeholder overwritten by the early `checkpoint` below.
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: topology_name.to_string(),
            status: FleetStatus::Starting,
            control_endpoint: None,
            pinned_generation,
            services: Vec::new(),
        })),
    };
    // Empty fleet: the services aren't built yet, but the supervisor identity +
    // pin ARE — that is all `live_pinned_generation` needs to protect this gen.
    reporter.checkpoint(&[]);

    // Manifest-vs-disk drift FIRST: if the manifest disagrees with cmd/*-svc
    // (the source of truth), report it AS drift — not as a "missing binary"
    // symptom from the staged-artifact check below.
    manifest::validate_disk(&layout.root.join("cmd"))
        .context("validate fleet manifest against cmd/*-svc on disk")?;

    // Then: every binary this run needs must already be staged in
    // <root>/deploy — weles never builds. Dies here (per-line missing list)
    // before any further validation if the deploy dir is incomplete.
    let mut packages: Vec<&str> = match topology {
        Topology::Split => manifest::split_fleet().iter().map(|svc| svc.pkg).collect(),
        Topology::Monolith => vec![manifest::monolith().pkg],
    };
    packages.extend(["adminctl", "edgeca"]);
    packages.sort_unstable();
    packages.dedup();
    prep::validate_binaries(&layout, &packages)
        .context("validate deployed fleet binaries")?;

    manifest::validate_pg_budget().context("validate fleet Postgres session budget")?;

    let ca = prep::mint_ca(&layout)?;
    let database_url = prep::database_url();
    prep::seed_admin(&layout, &database_url)?;

    let inputs = RuntimeInputs {
        database_url,
        ca_cert: ca.cert,
        ca_key: ca.key,
    };
    let defs = match topology {
        Topology::Split => manifest::split_fleet(),
        Topology::Monolith => vec![manifest::monolith()],
    };
    let mut fleet: Vec<Supervised> = defs.into_iter().map(Supervised::new).collect();
    // Re-checkpoint now that the fleet exists (the pin was already recorded by
    // the early checkpoint above): status still Starting, endpoint still None,
    // services now populated.
    reporter.checkpoint(&fleet);

    // Bind the control endpoint BEFORE boot so `weles status`/`down` reach the
    // fleet DURING startup (a mid-boot `down` flips `fleet_stop`, which
    // `boot`/`monitor` observe through `stop_requested`). `fleet_stop` is the
    // ONLY fleet-stop writer besides the signal handler's `STOP`; no
    // control-plane failure path may store into it (stop-authority separation).
    let endpoint = control_endpoint_path(&layout, &reporter.run_id);
    let fleet_stop = Arc::new(AtomicBool::new(false));
    let control = match ControlServer::bind(
        endpoint.clone(),
        reporter.shared(),
        Arc::clone(&fleet_stop),
    ) {
        Ok(control) => control,
        Err(error) => {
            // Nothing has spawned yet: teardown just records the terminal
            // status (a no-op over an unspawned fleet), then fail loudly
            // (devctl parity — a fleet nobody can `status`/`down` is not the
            // tool weles promises). The early return means this path runs
            // teardown exactly once.
            let _ = teardown(&mut fleet, &reporter, FleetStatus::Failed);
            return Err(error)
                .with_context(|| format!("bind control endpoint {}", endpoint.display()));
        }
    };
    // Bind Ok = the listener accepts: publish the endpoint now (status still
    // Starting) so a client that reads the file cannot beat the listener.
    reporter.set_control_endpoint(Some(endpoint.to_string_lossy().into_owned()));
    reporter.checkpoint(&fleet);

    // Spawn the readiness poller alongside the control endpoint (before boot, so
    // it starts reflecting `/readyz` freshness as services turn Healthy). It runs
    // on its OWN thread — a blocking probe must never delay the monitor tick — and
    // reads Healthy-ness from `reporter.shared()`. `readiness` is indexed like the
    // fleet; the monitor folds it in for the checkpoint. The poller NEVER writes
    // the fleet stop (stop-authority); its own lifecycle is its private flag.
    let readiness: Arc<Mutex<Vec<Readiness>>> =
        Arc::new(Mutex::new(vec![Readiness::Unknown; fleet.len()]));
    let ports: Vec<u16> = fleet.iter().map(|svc| svc.def.http_port).collect();
    let poller = ReadinessPoller::spawn(reporter.shared(), Arc::clone(&readiness), ports);

    let run_result = boot(&layout, &inputs, &mut fleet, &reporter, &fleet_stop);
    if run_result.is_ok() && !stop_requested(&fleet_stop) {
        reporter.set_status(FleetStatus::Running);
        reporter.checkpoint(&fleet);
        println!("weles: fleet healthy — press Ctrl-C or run `weles down` to stop");
        monitor(
            &layout,
            &inputs,
            &mut fleet,
            &reporter,
            &control,
            &readiness,
            &fleet_stop,
        );
    }
    // Stop and join the poller + control threads before teardown starts (every
    // path that spawned them reaches here — the bind-failure path returned above).
    drop(poller);
    drop(control);
    // Runs for every exit path: operator stop, boot failure (unwinding exactly
    // what started, in reverse), or STOP during boot. A failure lands the fleet
    // in Failed; any clean stop lands Stopped.
    let terminal = if run_result.is_ok() {
        FleetStatus::Stopped
    } else {
        FleetStatus::Failed
    };
    let clean = teardown(&mut fleet, &reporter, terminal);
    // `_lock` drops here — strictly after teardown.
    // A run that otherwise succeeded but whose teardown could not confirm every
    // service stopped is escalated to an error so the exit code is non-zero
    // (devctl parity — a possibly-orphaned service must not report success). The
    // persisted terminal was already set to Failed by `teardown` in this case.
    if run_result.is_ok() && !clean {
        return Err(anyhow!(
            "weles: teardown could not confirm all services stopped — see logs \
             (a service may be orphaned)"
        ));
    }
    run_result
}

/// The bounded loopback control endpoint for this run: a named pipe on Windows
/// (keyed by `run_id`), a UDS under `run/weles` on Linux. Mirrors
/// `tools/devctl/src/supervisor.rs::control_endpoint`.
fn control_endpoint_path(layout: &prep::Layout, run_id: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = layout;
        PathBuf::from(format!(r"\\.\pipe\gamebackend-weles-{run_id}"))
    }
    #[cfg(target_os = "linux")]
    {
        layout.run_dir.join(format!("control-{run_id}.sock"))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = run_id;
        layout.run_dir.join("unsupported-control")
    }
}

/// Spawns each service in manifest order and gates on its readyz before
/// moving to the next. `Ok(())` with STOP set means "operator interrupted the
/// boot" — the caller goes straight to teardown of what already started.
fn boot(
    layout: &prep::Layout,
    inputs: &RuntimeInputs,
    fleet: &mut [Supervised],
    reporter: &Reporter,
    fleet_stop: &AtomicBool,
) -> Result<()> {
    for index in 0..fleet.len() {
        if stop_requested(fleet_stop) {
            return Ok(());
        }
        let name = fleet[index].def.name;
        let http_port = fleet[index].def.http_port;

        // First spawn only: a listener on the port is a stale process from a
        // previous hung run. Never re-checked on a crash respawn — the
        // just-killed incarnation's TIME_WAIT would false-positive.
        health::ensure_no_stale_listener(name, http_port)?;

        let proc = spawn_service(layout, &fleet[index].def, inputs, false)
            .with_context(|| format!("spawn {name}"))?;
        fleet[index].proc = Some(proc);
        fleet[index].phase = Some(Phase::WaitingHealthy {
            deadline: Instant::now() + HEALTH_DEADLINE,
        });
        fleet[index].status = Status::WaitingHealthy;
        reporter.checkpoint(fleet);

        let deadline = Instant::now() + HEALTH_DEADLINE;
        loop {
            if stop_requested(fleet_stop) {
                return Ok(());
            }
            // try_wait FIRST each tick: a dead child wins over whatever a
            // stale or foreign listener might answer on the port.
            if let Some(status) = fleet[index]
                .proc
                .as_mut()
                .expect("service was just spawned")
                .try_wait()
                .with_context(|| format!("query {name} status during boot"))?
            {
                bail!(
                    "{name} exited during startup with code {:?} — see {} / {}",
                    status.code(),
                    layout.run_dir.join(format!("{name}.out.log")).display(),
                    layout.run_dir.join(format!("{name}.err.log")).display()
                );
            }
            if health::probe(http_port) == ProbeResult::Ready {
                let now = Instant::now();
                fleet[index].history.record_healthy(now);
                fleet[index].phase = Some(Phase::Healthy { healthy_since: now });
                fleet[index].status = Status::Healthy;
                reporter.checkpoint(fleet);
                println!("weles: {name} healthy on :{http_port}");
                break;
            }
            if Instant::now() >= deadline {
                bail!("{name} did not become healthy on :{http_port} within {HEALTH_DEADLINE:?}");
            }
            std::thread::sleep(BOOT_PROBE_INTERVAL);
        }
    }
    Ok(())
}

/// The single non-blocking monitor loop: per 100ms tick, observe every
/// service, run the pure [`step`], execute its directive. Returns when STOP
/// is requested. Every wait in here is bounded (probe timeouts, the hung-kill
/// shutdown budget) — nothing can block a tick indefinitely.
fn monitor(
    layout: &prep::Layout,
    inputs: &RuntimeInputs,
    fleet: &mut [Supervised],
    reporter: &Reporter,
    control: &ControlServer,
    readiness: &Arc<Mutex<Vec<Readiness>>>,
    fleet_stop: &AtomicBool,
) {
    let mut control_death_reported = false;
    loop {
        if stop_requested(fleet_stop) {
            return;
        }
        // A dead control thread is a reported degradation, NEVER a fleet stop
        // (the fleet keeps running; Ctrl-C remains the stop path).
        if !control_death_reported && control.dead() {
            control_death_reported = true;
            eprintln!(
                "weles: the control endpoint is dead — `weles status`/`down` are unavailable; \
                 the fleet keeps running (stop it with Ctrl-C)"
            );
        }
        // Fold in the poller's latest readiness (a brief lock to clone — never a
        // probe on this thread). This ONLY writes `readiness`; the restart loop
        // below runs the same pure `step` over liveness, untouched by it.
        let latest = readiness.lock().expect("readiness mutex poisoned").clone();
        let readiness_changed = fold_readiness(fleet, &latest);
        for index in 0..fleet.len() {
            let Some(phase) = fleet[index].phase else {
                continue; // never started (impossible after a full boot)
            };
            let now = Instant::now();
            let observed = observe(&mut fleet[index], phase);
            let directive = step(phase, observed, stop_requested(fleet_stop), now, &mut fleet[index].history);
            if directive == Directive::Respawn {
                // Make the transient Restarting status observable in the
                // checkpoint before the (bounded, but real) spawn work.
                fleet[index].status = Status::Restarting;
                reporter.checkpoint(fleet);
            }
            let changed = apply(layout, inputs, &mut fleet[index], phase, directive, now);
            if changed {
                reporter.checkpoint(fleet);
            }
        }
        // Publish a readiness-only change (no phase transition this tick) so
        // `weles status` reflects the freshness promptly.
        if readiness_changed {
            reporter.checkpoint(fleet);
        }
        std::thread::sleep(MONITOR_TICK);
    }
}

/// Gathers this tick's observation for one service. Liveness (`try_wait`)
/// always wins over the probe; a liveness query error is treated as a crash
/// (loudly), never ignored.
fn observe(svc: &mut Supervised, phase: Phase) -> Observed {
    let liveness = match svc.proc.as_mut() {
        None => Observed::Exited,
        Some(proc) => match proc.try_wait() {
            Ok(None) => Observed::Alive,
            Ok(Some(_)) => Observed::Exited,
            Err(error) => {
                eprintln!(
                    "weles: {}: liveness query failed ({error:#}); treating as exited",
                    svc.def.name
                );
                Observed::Exited
            }
        },
    };
    match phase {
        Phase::Backoff { .. } | Phase::Failed | Phase::Healthy { .. } => liveness,
        Phase::WaitingHealthy { .. } => {
            if liveness == Observed::Exited {
                Observed::Exited
            } else if health::probe(svc.def.http_port) == ProbeResult::Ready {
                Observed::Ready
            } else {
                Observed::NotReady
            }
        }
    }
}

/// Executes one directive for one service. Returns whether anything about
/// the service changed (→ checkpoint).
fn apply(
    layout: &prep::Layout,
    inputs: &RuntimeInputs,
    svc: &mut Supervised,
    phase: Phase,
    directive: Directive,
    now: Instant,
) -> bool {
    let name = svc.def.name;
    match directive {
        Directive::Stay(new_phase) => {
            if new_phase == phase {
                return false;
            }
            match new_phase {
                Phase::Backoff { respawn_at } => {
                    // The dead OwnedProc's exit is already cached; dropping it
                    // kills nothing.
                    svc.proc = None;
                    eprintln!(
                        "weles: {name} is down — respawn in {:?} (consecutive failure {}/{}); \
                         logs: {} / {}",
                        respawn_at.saturating_duration_since(now),
                        svc.history.consecutive_failures,
                        MAX_CONSECUTIVE_FAILURES,
                        layout.run_dir.join(format!("{name}.out.log")).display(),
                        layout.run_dir.join(format!("{name}.err.log")).display()
                    );
                }
                Phase::Failed => {
                    svc.proc = None;
                    eprintln!(
                        "weles: {name} failed {MAX_CONSECUTIVE_FAILURES} consecutive times — \
                         giving up on it (the rest of the fleet keeps running)"
                    );
                }
                Phase::Healthy { .. } => {
                    println!(
                        "weles: {name} healthy on :{} (after restart #{})",
                        svc.def.http_port, svc.restarts
                    );
                }
                Phase::WaitingHealthy { .. } => {}
            }
            svc.phase = Some(new_phase);
            svc.status = status_of(new_phase);
            true
        }
        Directive::Respawn => {
            match spawn_service(layout, &svc.def, inputs, true) {
                Ok(proc) => {
                    svc.proc = Some(proc);
                    svc.restarts += 1;
                    svc.phase = Some(Phase::WaitingHealthy {
                        deadline: Instant::now() + HEALTH_DEADLINE,
                    });
                    svc.status = Status::WaitingHealthy;
                    println!("weles: respawned {name} (restart #{})", svc.restarts);
                }
                Err(error) => {
                    eprintln!("weles: respawn of {name} failed: {error:#}");
                    let next = crash_phase(&mut svc.history, Instant::now());
                    svc.phase = Some(next);
                    svc.status = status_of(next);
                }
            }
            true
        }
        Directive::Kill(new_phase) => {
            eprintln!(
                "weles: {name} did not become healthy within {HEALTH_DEADLINE:?} — stopping it"
            );
            if let Some(mut proc) = svc.proc.take() {
                if let Err(error) = proc.shutdown(HUNG_GRACE, HUNG_FORCE) {
                    eprintln!("weles: stopping hung {name} failed: {error:#}");
                }
            }
            svc.phase = Some(new_phase);
            svc.status = status_of(new_phase);
            true
        }
    }
}

/// Stops live services in REVERSE spawn order (graceful 5s, force 5s each),
/// checkpointing every transition, then records the terminal fleet status.
/// Also serves as the boot-failure unwind: services boot never reached simply
/// have no process to stop. Returns whether the teardown was CLEAN — every live
/// service was confirmed stopped ([`stop_outcome`]); an unconfirmed stop (a
/// possible orphan) makes the fleet's exit non-zero (devctl parity).
fn teardown(fleet: &mut [Supervised], reporter: &Reporter, terminal: FleetStatus) -> bool {
    reporter.set_status(FleetStatus::Stopping);
    reporter.checkpoint(fleet);
    let mut clean_all = true;
    for index in (0..fleet.len()).rev() {
        let name = fleet[index].def.name;
        match fleet[index].proc.take() {
            Some(mut proc) => {
                // A service that died during its OWN boot gate (boot bailed with
                // the process still held) is already dead here — record that as
                // Exited, never relabel it Stopped: the status table must not
                // misstate history (Step-5 review Info #3).
                let already_dead = matches!(proc.try_wait(), Ok(Some(_)));
                if already_dead {
                    // shutdown() returns immediately for an already-exited proc;
                    // it still closes/reaps the containment handle.
                    let _ = proc.shutdown(STOP_GRACE, STOP_FORCE);
                    fleet[index].status = Status::Exited;
                    reporter.checkpoint(fleet);
                    println!("weles: {name} had already exited");
                } else {
                    fleet[index].status = Status::Stopping;
                    reporter.checkpoint(fleet);
                    let result = proc.shutdown(STOP_GRACE, STOP_FORCE);
                    match &result {
                        Ok(Outcome::Graceful(_)) => println!("weles: {name} stopped"),
                        Ok(Outcome::Forced(_)) => println!("weles: {name} force-stopped"),
                        Err(error) => eprintln!(
                            "weles: stopping {name} failed: {error:#} — the process may be \
                             ORPHANED (force could not confirm it exited); check for a stray \
                             {name} before the next run"
                        ),
                    }
                    // The one authority for "was this stop clean": an unconfirmed
                    // stop records Failed (never a false Stopped) and drops the
                    // fleet-clean bit.
                    let (status, clean) = stop_outcome(&result);
                    fleet[index].status = status;
                    clean_all &= clean;
                    reporter.checkpoint(fleet);
                }
            }
            None => {
                let final_status = match fleet[index].status {
                    // Given up earlier — keep the verdict visible.
                    Status::Failed => Status::Failed,
                    // Boot never spawned it.
                    Status::Starting => Status::Stopped,
                    // Crashed earlier (Backoff/Restarting/anything else
                    // proc-less): it exited on its own.
                    _ => Status::Exited,
                };
                if fleet[index].status != final_status {
                    fleet[index].status = final_status;
                    reporter.checkpoint(fleet);
                }
            }
        }
    }
    // Persist a terminal that AGREES with the returned clean bit (and thus with
    // run_up's exit code): a nominally-Stopped run whose teardown could not
    // confirm every service stopped is persisted Failed, so `weles down`'s
    // wait_for_terminal and run_up never disagree. A run that already failed
    // stays Failed regardless.
    let final_terminal = if clean_all {
        terminal
    } else {
        FleetStatus::Failed
    };
    reporter.set_status(final_terminal);
    reporter.checkpoint(fleet);
    println!("weles: fleet stopped");
    clean_all
}

/// Spawns one service via [`platform::spawn`] (the crate's only spawn seam):
/// manifest-composed env, cwd pinned to the repo root, logs at
/// `run/weles/<name>.{out,err}.log`. The first spawn truncates any previous
/// run's logs; a respawn APPENDS so the crash evidence from the previous
/// incarnation survives the restart.
fn spawn_service(
    layout: &prep::Layout,
    def: &ServiceDef,
    inputs: &RuntimeInputs,
    append_logs: bool,
) -> Result<OwnedProc> {
    let open = |path: PathBuf| -> Result<File> {
        let file = if append_logs {
            File::options().create(true).append(true).open(&path)
        } else {
            File::create(&path)
        };
        file.with_context(|| format!("open service log {}", path.display()))
    };
    let stdout = open(layout.run_dir.join(format!("{}.out.log", def.name)))?;
    let stderr = open(layout.run_dir.join(format!("{}.err.log", def.name)))?;
    platform::spawn(SpawnSpec {
        program: layout.binary(def.pkg),
        args: Vec::new(),
        env: manifest::compose_env(def, inputs),
        cwd: Some(layout.root.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Installs the stop handler: Windows `SetConsoleCtrlHandler`
/// (CTRL_C/BREAK/CLOSE all flip STOP), Unix SIGINT/SIGTERM. Async-signal-safe
/// by construction — the handler touches ONLY the static atomic. Copied from
/// `tools/devctl/src/supervisor.rs::install_signal_handler`.
#[cfg(windows)]
fn install_ctrl_handler() -> Result<()> {
    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        STOP.store(true, Ordering::SeqCst);
        1
    }
    // SAFETY: registers a process-wide ctrl handler that touches only the atomic.
    if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1) } == 0
    {
        return Err(std::io::Error::last_os_error()).context("install Ctrl-C handler");
    }
    Ok(())
}

#[cfg(unix)]
fn install_ctrl_handler() -> Result<()> {
    unsafe extern "C" fn handler(_signal: libc::c_int) {
        // Async-signal-safe: atomic store only.
        STOP.store(true, Ordering::SeqCst);
    }
    let handler: unsafe extern "C" fn(libc::c_int) = handler;
    // SAFETY: installs handlers that perform only atomic operations.
    unsafe {
        if libc::signal(libc::SIGINT, handler as libc::sighandler_t) == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error()).context("install SIGINT handler");
        }
        if libc::signal(libc::SIGTERM, handler as libc::sighandler_t) == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error()).context("install SIGTERM handler");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;

//! Owned foreground fleet supervisor — the heart of weles and its
//! differentiator vs devctl: a crashed service is RESTARTED in place with
//! exponential backoff (devctl tears the whole fleet down on any crash), and
//! a service that keeps failing is given up on ALONE while the rest of the
//! fleet keeps running.
//!
//! `run_up` sequence (the rollout lock comes FIRST — before any validation or
//! prep — per the Step-4 review finding): (1) discover layout (which pins +
//! validates the deployed `fleet.toml`) + acquire `run/rollout.lock`, then
//! install the Ctrl-C/SIGTERM handler immediately (P1: so an interrupt during
//! the slow prepare window can't orphan a hook on the unwind); (2) validate the
//! manifest against `cmd/*-svc` on disk (drift reported AS drift), then the
//! DEPLOYED binaries in `<root>/deploy` the fleet needs (weles never builds);
//! (3) run the fleet's declared `[[prepare]]` hooks (CA mint, admin seed —
//! weles runs them domain-blind); (4) boot each service in fleet order behind a
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

use crate::agentapi;
use crate::control::ControlServer;
use crate::health::{self, ProbeResult};
use crate::lock;
use crate::manifest::{self, ServiceDef};
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

/// A hung service's kill was requested. If the stop was CONFIRMED
/// (`stop_outcome`'s clean bit), adopt the `intended` post-kill phase; if it was
/// UNCONFIRMED (an `Err` — force could not confirm the exit, the process may be
/// ORPHANED and still holding its port), give up on the service (`Failed`)
/// instead of `Backoff`, so the monitor NEVER respawns a second instance over a
/// possible orphan. This reuses the ONE authority (`stop_outcome`) for "confirmed
/// gone" and layers only the Kill-loop policy on top — no second decision site.
///
/// Deliberate availability trade-off (never smuggled): a *transient* force-timeout
/// (`shutdown` bails after force + its timeout, `platform/mod.rs`) strands the
/// service `Failed` for the whole run even though `OwnedProc::Drop`
/// (`platform/mod.rs:186-199`, plus Windows `KILL_ON_JOB_CLOSE`) usually reaps
/// that same process moments later. That is the conservative-correct choice:
/// never double-spawn over a POSSIBLE orphan. "Orphan still holds the port" is the
/// Unix-leaning worst case, not a guarantee (the Drop backstop typically clears
/// it) — but the phase decision cannot depend on a best-effort backstop it can't
/// observe, so it fails closed.
fn phase_after_kill(shutdown: &Result<Outcome>, intended: Phase) -> Phase {
    match stop_outcome(shutdown) {
        (_, true) => intended,
        (_, false) => Phase::Failed,
    }
}

// ---------------------------------------------------------------------------
// Readiness: a post-healthy `/readyz` freshness dimension, structurally
// DISJOINT from the restart decision. The three pure functions below are the
// whole authority — none of them can touch `phase`/`status`/`history`, so a
// 503/torn/unreachable POLLER probe records `Degraded`/`Unreachable` and
// NOTHING else. The poller writes into a shared Vec; the monitor folds that Vec
// into each `Supervised.readiness` for the checkpoint.
//
// A probe DOES reach `step` — but ONLY through `observe` in `WaitingHealthy`,
// where it becomes `Observed::Ready`/`NotReady` by design (a service that never
// comes up must be killed). What keeps a probe inert for a service that is
// already up is three mechanisms, each pinned by a test:
//   (a) `readiness_for` is the only poller probe → verdict map, and its output
//       type (`Readiness`) has no constructor into `Observed`/`Directive`;
//       `fold_readiness` writes only `readiness` and returns a bool;
//   (b) `step`'s `Phase::Healthy` arm restarts on `Observed::Exited` ALONE —
//       every other observation, probe-derived included, is `Stay(phase)`;
//   (c) `Observed::Exited` is unforgeable from a probe: `observe` derives it
//       from LIVENESS alone (`try_wait`, or no process at all — the
//       `Backoff`/`Failed` case, see `Observed::Exited`'s own doc), and a
//       `ConnectFailed` (nothing listening at all) becomes `NotReady`, never
//       `Exited`.
// ---------------------------------------------------------------------------

/// Maps a single readiness probe to the recorded [`Readiness`]. This is the ONE
/// place a POLLER `ProbeResult` becomes a readiness verdict — and it produces
/// nothing but a `Readiness`, so no poller probe can synthesize an `Observed` or
/// a restart `Directive`. (`observe` runs its own, separate probe in
/// `WaitingHealthy`; that boot-gate path never applies to a `Healthy` service.)
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
    /// A plain human label for the deployed fleet (weles has no split/monolith
    /// concept) — derived from the fleet, e.g. its process count. Rendered by
    /// `weles status`/`down` and recorded into [`FleetState::topology`].
    topology: String,
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
            topology: self.topology.clone(),
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

    /// Like [`Reporter::checkpoint`] but FATAL: propagates the `state::checkpoint`
    /// error instead of swallowing it. Used for the EARLY pin write only — a
    /// fleet that cannot persist its initial pin has no retention protection (a
    /// concurrent `weles deploy` would not see the pin and could prune this
    /// booting run's generation), so it must refuse to start (fail-closed) rather
    /// than run blind. Publishes the in-memory snapshot for the control thread
    /// before persisting, exactly like `checkpoint`. Residual known gap (M3): the
    /// later best-effort `checkpoint`s still swallow a state-dir that breaks
    /// MID-RUN — a separate signal, out of scope here.
    fn checkpoint_critical(&self, fleet: &[Supervised]) -> Result<()> {
        let snapshot = self.snapshot(fleet);
        *self.shared.lock().expect("state mutex poisoned") = snapshot.clone();
        state::checkpoint(&self.state_path, &snapshot)
            .with_context(|| format!("persist initial state to {}", self.state_path.display()))
    }
}

/// Discovers the workspace layout under the runtime-resolved fleet root
/// ([`prep::resolve_root`] — `--root` flag, else `WELES_ROOT`, else a walk up to
/// the repo marker). Shared by `up` and `deploy`.
pub fn discover_layout(root: Option<PathBuf>) -> Result<prep::Layout> {
    prep::Layout::discover(prep::resolve_root(root)?)
}

/// Like [`discover_layout`] but for the `deploy` path: does NOT require a
/// pinned generation (`deploy/current`), so a fresh checkout can run its first
/// `weles deploy`.
pub fn discover_layout_for_deploy(root: Option<PathBuf>) -> Result<prep::Layout> {
    prep::Layout::discover_for_deploy(prep::resolve_root(root)?)
}

/// The whole `weles up` lifecycle. Returns when the fleet has been torn down
/// (operator stop) or a boot failure was unwound. `root` is the optional
/// `--root <path>` override threaded from `cli`.
pub fn run_up(root: Option<PathBuf>) -> Result<()> {
    let layout = discover_layout(root)?;

    // The deployed fleet was parsed + validated ONCE at discover
    // (PIN-AT-DISCOVER); the `up` path always has it — `discover` fails rather
    // than hand back a fleet-less layout — so this `expect` names an invariant,
    // not a runtime branch. `deployed` (and everything borrowed from it: `defs`,
    // `passthrough`, `prepare`) borrows `layout` immutably for the whole run,
    // alongside every other `&layout` use.
    let deployed = layout
        .fleet()
        .expect("Layout::discover pins a validated fleet on the up path");
    // weles has NO split/monolith concept — it boots whatever fleet was
    // deployed. Label it by process count for `weles status`/`down`.
    let fleet_label = format!("{}-process", deployed.services.len());

    // Lock FIRST (Step-4 review finding): nothing rollout-bearing may run
    // before this process is inside run/rollout.lock's one permitted rollout —
    // by owning it (the operator path, unchanged) or by consuming a one-shot
    // lease borrowed from the parent that spawned us (a verifyctl stage, which
    // holds that same lock for its whole manifest and would otherwise deadlock
    // against us). `lock::acquire_or_borrow` refuses rather than fall back.
    //
    // `_lock` MUST stay an RAII local declared here and dropped LAST — after
    // teardown, after `control`, after the agent island — because releasing the
    // rollout lock while services still drain lets a devctl/verifyctl rollout
    // start against the shared Postgres mid-teardown.
    //
    // NOTHING ENFORCES THAT BUT THIS COMMENT AND REVIEW. `lock::Lease` is
    // `!Send`, which bars moving it to another thread; it does NOT constrain
    // drop order on this one. Moving `drop(_lock)` above `teardown` would
    // compile and would be a silent regression — do not.
    let run_id = format!("{:016x}", rand::random::<u64>());
    let _lock = lock::acquire_or_borrow(&layout.root, &run_id)?;

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
    // lock + discover, BEFORE the fleet's prepare hooks (edgeca can run for
    // ~30s; an admin-seed hook does a Postgres round-trip). The pin is available
    // from `layout` here, and this early `Starting` checkpoint carries the live
    // pid + `pinned_generation`, so a concurrent `weles deploy` sees the live
    // pin and won't prune this booting up's generation. Without this, the pin
    // was invisible across the whole hook window (state.json absent or
    // stale/terminal), and a deploy could delete the booting up's gen-N out from
    // under it (a loud spawn failure, not silent loss, but still this fix's own
    // new seam). The control endpoint is still bound later, before boot (Part A)
    // — only this state write moves ahead of the hooks.
    let supervisor = ProcessIdentity {
        pid: std::process::id(),
        started_unix: unix_now(),
    };
    let pinned_generation = layout.pinned_generation();
    let reporter = Reporter {
        state_path: layout.run_dir.join("state.json"),
        run_id,
        topology: fleet_label.clone(),
        supervisor,
        pinned_generation: pinned_generation.clone(),
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        // Placeholder overwritten by the early `checkpoint` below.
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: fleet_label,
            status: FleetStatus::Starting,
            control_endpoint: None,
            pinned_generation,
            services: Vec::new(),
        })),
    };
    // Empty fleet: the services aren't built yet, but the supervisor identity +
    // pin ARE — that is all `live_pinned_generation` needs to protect this gen.
    // FAIL-CLOSED: if this initial pin can't be persisted, refuse to start rather
    // than run without retention protection (a silent fail here + a concurrent
    // deploy = a deleted live generation, the exact loss the pin prevents). The
    // `_lock` is released by Drop on this return.
    reporter
        .checkpoint_critical(&[])
        .context("could not persist initial state / pin protection — refusing to start")?;

    // Every binary this run needs must already be staged in
    // <root>/deploy — weles never builds. The set is derived from the deployed
    // fleet (`[[service]]` pkgs ∪ `[[prepare]]` runs), so a hook's binary
    // (edgeca/adminctl) is staged because a hook references it. Dies here
    // (per-line missing list) before any further work if the deploy dir is
    // incomplete.
    let packages = prep::deploy_packages(deployed);
    prep::validate_binaries(&layout, &packages)
        .context("validate deployed fleet binaries")?;

    // Run the fleet's declared `[[prepare]]` hooks (CA mint, admin seed) — in
    // declared order, BEFORE any service is spawned. A nonzero exit or timeout
    // aborts the whole `up` HERE: nothing is spawned past a failed hook (the
    // fleet below isn't even built yet). This occupies the slot the old
    // `mint_ca`/`seed_admin` block held; weles runs each hook domain-blind,
    // knowing only the command name it was told.
    prep::run_prepare(&deployed.prepare, &layout).context("run fleet prepare hooks")?;

    // `defs` IS the booting fleet, borrowed straight from the pinned+validated
    // `deployed` — never re-derived. Every peer address handed to a service is
    // derived from this exact slice (in `compose_env_with_fleet` and
    // `PeerAddrs::from_fleet`), so a service told an address by env and a
    // service that asks for it over the agent cannot be told different things.
    // `passthrough` (env KEYS forwarded from weles's own env) comes from the
    // same fleet.
    let defs: &[ServiceDef] = &deployed.services;
    let passthrough: &[String] = &deployed.passthrough;
    let mut fleet: Vec<Supervised> = defs.iter().cloned().map(Supervised::new).collect();
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

    // Bind the agent endpoint here too — after the Reporter exists, BEFORE
    // `boot`. `boot` is sequential and readyz-gated (one service at a time,
    // each gated on its own /readyz), so a service that needs the agent to
    // reach readyz must find it ALREADY listening. Binding it later would not
    // deadlock — HEALTH_DEADLINE gates the wait and `bail!`s — but it would
    // turn into a bounded startup FAILURE of the first service that asks. Same
    // conclusion as the control endpoint, different reason.
    //
    // Stale-listener check first, exactly as `boot` does before a service's
    // first spawn: anything already on AGENT_PORT is a stale process from a
    // previous hung run, and binding would fail (or, worse, a service would
    // resolve against the OLD agent).
    //
    // Both failures — stale listener and bind — take ONE exit: chained here so
    // neither can grow its own terminal status. A bare `?` on the stale check
    // would return with the state still `Starting`, so `weles status` would
    // report "stale state" for what is really a clean, known startup failure —
    // while the identical failure 20 lines up (control bind) publishes `Failed`.
    // The `resolve` map is derived from `defs` — the SAME slice `spawn_ctx`
    // threads into `compose_env_with_fleet` below. One authority: a service
    // told an address by env and a service that asks for it over the wire
    // cannot be told different things, and a fleet whose services declare no
    // `provider` (e.g. a single-process fleet) yields an empty map so every
    // resolve 404s (no topology branch: see `manifest::PeerAddrs`).
    let agent = match health::ensure_no_stale_listener("weles-agent", manifest::AGENT_PORT)
        .and_then(|()| {
            agentapi::AgentServer::bind(manifest::AGENT_PORT, manifest::PeerAddrs::from_fleet(defs))
        })
    {
        Ok(agent) => agent,
        Err(error) => {
            // Nothing has spawned yet (same position as the control bind
            // failure above): record the terminal status over an unspawned
            // fleet, then fail loudly. `control` is a local — it drops on this
            // return, i.e. strictly AFTER this teardown, preserving P6.
            let _ = teardown(&mut fleet, &reporter, FleetStatus::Failed);
            return Err(error).with_context(|| {
                format!("start the agent endpoint on 127.0.0.1:{}", manifest::AGENT_PORT)
            });
        }
    };

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

    let spawn_ctx = SpawnCtx { layout: &layout, passthrough, defs };
    let run_result = boot(&spawn_ctx, &mut fleet, &reporter, &fleet_stop);
    if run_result.is_ok() && !stop_requested(&fleet_stop) {
        reporter.set_status(FleetStatus::Running);
        reporter.checkpoint(&fleet);
        println!("weles: fleet healthy — press Ctrl-C or run `weles down` to stop");
        monitor(
            &spawn_ctx,
            &mut fleet,
            &reporter,
            &control,
            &agent,
            &readiness,
            &fleet_stop,
        );
    }
    // Stop and join the POLLER before teardown (it probes the services being torn
    // down — stop it first). The CONTROL server is kept ALIVE THROUGH teardown
    // (dropped only after): the teardown checkpoints still carry
    // `control_endpoint: Some(...)`, so a concurrent `weles status`/`down` reaches
    // a LIVE endpoint and sees `Stopping` — rather than classifying `Connect`
    // (classify ignores the endpoint) and dialling a dead pipe, or reading a
    // misleading "very early startup" from a `None` endpoint. `wait_for_terminal`
    // (DOWN_TIMEOUT 130s > worst-case teardown ~110s) then observes the terminal
    // from the end of teardown.
    drop(poller);
    // Runs for every exit path: operator stop, boot failure (unwinding exactly
    // what started, in reverse), or STOP during boot. A failure lands the fleet
    // in Failed; any clean stop lands Stopped.
    let terminal = if run_result.is_ok() {
        FleetStatus::Stopped
    } else {
        FleetStatus::Failed
    };
    let clean = teardown(&mut fleet, &reporter, terminal);
    // Now that the terminal status is published, stop and join the control
    // server. `_lock` drops at the end of the function — strictly after teardown.
    // MUST stay after teardown — control serves status/down through the teardown
    // window; moving this before teardown blinds concurrent status/down (P6).
    drop(control);
    // The agent is dropped here for the SAME reason (P6, service-facing half): a
    // service still draining may call the agent, so the endpoint outlives every
    // service. Its drop is bounded — the accept loop stops on a oneshot (a flag
    // could never reach an accept parked on `.await`) and the runtime is dropped
    // on its OWN thread under SHUTDOWN_GRACE — so `_lock`, which drops at the end
    // of this function, is never stalled behind a blocking `Runtime::drop`.
    drop(agent);
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
/// (keyed by `run_id`), a filesystem-path UDS under `run/weles` on unix
/// (Linux + darwin). Mirrors `tools/devctl/src/supervisor.rs::control_endpoint`.
fn control_endpoint_path(layout: &prep::Layout, run_id: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = layout;
        PathBuf::from(format!(r"\\.\pipe\gamebackend-weles-{run_id}"))
    }
    #[cfg(unix)]
    {
        layout.run_dir.join(format!("control-{run_id}.sock"))
    }
}

/// Spawns each service in manifest order and gates on its readyz before
/// moving to the next. `Ok(())` with STOP set means "operator interrupted the
/// The three things every service spawn needs, travelling as one: where the
/// staged artifacts and logs live, the per-fleet passthrough KEYS, and the
/// BOOTING fleet that peer addresses are derived from. Grouped because peer
/// addresses are only meaningful against the exact fleet `run_up` pinned —
/// passing them separately invites a caller that composes env against a
/// re-derived fleet.
struct SpawnCtx<'a> {
    layout: &'a prep::Layout,
    /// Env KEYS forwarded from weles's own environment into every service
    /// (per-fleet passthrough; from the deployed `fleet.toml`).
    passthrough: &'a [String],
    defs: &'a [ServiceDef],
}

/// boot" — the caller goes straight to teardown of what already started.
fn boot(
    ctx: &SpawnCtx<'_>,
    fleet: &mut [Supervised],
    reporter: &Reporter,
    fleet_stop: &AtomicBool,
) -> Result<()> {
    let layout = ctx.layout;
    for index in 0..fleet.len() {
        if stop_requested(fleet_stop) {
            return Ok(());
        }
        let name = fleet[index].def.name.clone();
        let http_port = fleet[index].def.http_port;

        // First spawn only: a listener on the port is a stale process from a
        // previous hung run. Never re-checked on a crash respawn — the
        // just-killed incarnation's TIME_WAIT would false-positive.
        health::ensure_no_stale_listener(&name, http_port)?;

        let proc = spawn_service(ctx, &fleet[index].def, false)
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
    ctx: &SpawnCtx<'_>,
    fleet: &mut [Supervised],
    reporter: &Reporter,
    control: &ControlServer,
    agent: &agentapi::AgentServer,
    readiness: &Arc<Mutex<Vec<Readiness>>>,
    fleet_stop: &AtomicBool,
) {
    let mut control_death_reported = false;
    let mut agent_death_reported = false;
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
        // Likewise a dead agent endpoint: reported once, never a fleet stop.
        // The agent serves services, not the operator, so the degradation is
        // different — but the stop-authority rule is identical.
        if !agent_death_reported && agent.dead() {
            agent_death_reported = true;
            eprintln!(
                "weles: the agent endpoint is dead — services can no longer reach it; \
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
            let changed = apply(ctx, &mut fleet[index], phase, directive, now);
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
    ctx: &SpawnCtx<'_>,
    svc: &mut Supervised,
    phase: Phase,
    directive: Directive,
    now: Instant,
) -> bool {
    let layout = ctx.layout;
    let name = svc.def.name.clone();
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
            match spawn_service(ctx, &svc.def, true) {
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
            let new_phase = match svc.proc.take() {
                Some(mut proc) => {
                    let result = proc.shutdown(HUNG_GRACE, HUNG_FORCE);
                    if let Err(error) = &result {
                        eprintln!(
                            "weles: stopping hung {name} failed: {error:#} — the process may be \
                             ORPHANED (force could not confirm it exited); giving up on {name} \
                             (NOT respawning over a possible orphan) — check for a stray {name}"
                        );
                    }
                    // Route through the ONE stop authority: an unconfirmed kill
                    // gives up (`Failed`) instead of `Backoff`, so the monitor
                    // never Respawns a second instance over a possible orphan.
                    phase_after_kill(&result, new_phase)
                }
                // observe() found it Alive to reach Kill, so this is unreachable;
                // stay defensive and adopt the intended phase.
                None => new_phase,
            };
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
        let name = fleet[index].def.name.clone();
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
    ctx: &SpawnCtx<'_>,
    def: &ServiceDef,
    append_logs: bool,
) -> Result<OwnedProc> {
    let layout = ctx.layout;
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
        program: layout.binary(&def.pkg),
        args: Vec::new(),
        // Peers resolve against `defs` — the exact fleet run_up pinned from the
        // deployed fleet.toml — never a re-derived one.
        env: manifest::compose_env_with_fleet(def, ctx.passthrough, ctx.defs),
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

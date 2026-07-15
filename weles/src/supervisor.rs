//! Owned foreground fleet supervisor — the heart of weles and its
//! differentiator vs devctl: a crashed service is RESTARTED in place with
//! exponential backoff (devctl tears the whole fleet down on any crash), and
//! a service that keeps failing is given up on ALONE while the rest of the
//! fleet keeps running.
//!
//! `run_up` sequence (the rollout lock comes FIRST — before any build/prep —
//! per the Step-4 review finding): (1) discover layout + acquire
//! `run/rollout.lock`; (2) validate the manifest against disk and the
//! Postgres session budget; (3) prep (build, mint CA, seed admin);
//! (4) install the Ctrl-C/SIGTERM handler; (5) boot each service in manifest
//! order behind a readyz gate; (6) a single non-blocking monitor loop;
//! (7) teardown in reverse spawn order, lock dropped last.
//!
//! Every restart DECISION is a pure function over an injected `now`
//! ([`next_restart`], [`step`]) — the loop merely executes directives — so
//! the crash → backoff → respawn / give-up policy is unit-testable without
//! real processes or a real clock (timing-tests doctrine).

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::cli::Topology;
use crate::health::{self, ProbeResult};
use crate::lock;
use crate::manifest::{self, RuntimeInputs, ServiceDef};
use crate::platform::{self, Outcome, OwnedProc, SpawnSpec};
use crate::prep;
use crate::state::{self, FleetState, ProcessIdentity, ServiceState, Status};

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
/// Teardown per-service shutdown budget (graceful, then force).
const STOP_GRACE: Duration = Duration::from_secs(5);
const STOP_FORCE: Duration = Duration::from_secs(5);
/// A not-yet-healthy service that blew its deadline gets no graceful
/// patience — it already proved unresponsive.
const HUNG_GRACE: Duration = Duration::from_secs(0);
const HUNG_FORCE: Duration = Duration::from_secs(5);

/// Flipped (only) by the Ctrl-C/SIGTERM handler; observed at the top of every
/// boot step and monitor tick, and inside every respawn decision.
static STOP: AtomicBool = AtomicBool::new(false);

fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
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
        }
    }
}

/// Owns everything a checkpoint needs besides the fleet itself. Checkpoint
/// failures are reported but never take the fleet down.
struct Reporter {
    state_path: PathBuf,
    run_id: String,
    topology: &'static str,
    supervisor: ProcessIdentity,
}

impl Reporter {
    fn checkpoint(&self, fleet: &[Supervised]) {
        let snapshot = FleetState {
            run_id: self.run_id.clone(),
            supervisor: self.supervisor,
            topology: self.topology.to_string(),
            control_endpoint: None, // lands in M0 Step 6
            services: fleet
                .iter()
                .map(|svc| ServiceState {
                    name: svc.def.name.to_string(),
                    status: svc.status,
                    pid: svc.proc.as_ref().map(OwnedProc::pid),
                    restarts: svc.restarts,
                })
                .collect(),
        };
        if let Err(error) = state::checkpoint(&self.state_path, &snapshot) {
            eprintln!(
                "weles: state checkpoint to {} failed: {error:#}",
                self.state_path.display()
            );
        }
    }
}

/// The whole `weles up` lifecycle. Returns when the fleet has been torn down
/// (operator stop) or a boot failure was unwound.
pub fn run_up(topology: Topology, skip_build: bool) -> Result<()> {
    // weles's Cargo.toml sits directly at the repo root (unlike tools/*), so
    // the workspace root is exactly one parent up.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("weles crate has no parent directory")?
        .to_path_buf();
    let layout = prep::Layout::discover(root)?;

    // Lock FIRST (Step-4 review finding): nothing rollout-bearing — not even
    // `cargo build` — may run before this process owns run/rollout.lock.
    let run_id = format!("{:016x}", rand::random::<u64>());
    let _lock = lock::acquire(&layout.root, &run_id)?;

    manifest::validate_disk(&layout.root.join("cmd"))
        .context("validate fleet manifest against cmd/*-svc on disk")?;
    manifest::validate_pg_budget().context("validate fleet Postgres session budget")?;

    if !skip_build {
        let mut packages: Vec<&str> = match topology {
            Topology::Split => manifest::split_fleet().iter().map(|svc| svc.pkg).collect(),
            Topology::Monolith => vec![manifest::monolith().pkg],
        };
        packages.extend(["adminctl", "edgeca"]);
        packages.sort_unstable();
        packages.dedup();
        prep::build(&layout, &packages)?;
    }

    let ca = prep::mint_ca(&layout)?;
    let database_url = prep::database_url();
    prep::seed_admin(&layout, &database_url)?;

    install_ctrl_handler()?;

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
    let reporter = Reporter {
        state_path: layout.run_dir.join("state.json"),
        run_id,
        topology: match topology {
            Topology::Split => "split",
            Topology::Monolith => "monolith",
        },
        supervisor: ProcessIdentity {
            pid: std::process::id(),
            started_unix: unix_now(),
        },
    };
    reporter.checkpoint(&fleet);

    let boot_result = boot(&layout, &inputs, &mut fleet, &reporter);
    if boot_result.is_ok() && !stop_requested() {
        println!("weles: fleet healthy — press Ctrl-C to stop");
        monitor(&layout, &inputs, &mut fleet, &reporter);
    }
    // Runs for every exit path: operator stop, boot failure (unwinding
    // exactly what started, in reverse), or STOP during boot.
    teardown(&mut fleet, &reporter);
    // `_lock` drops here — strictly after teardown.
    boot_result
}

/// Spawns each service in manifest order and gates on its readyz before
/// moving to the next. `Ok(())` with STOP set means "operator interrupted the
/// boot" — the caller goes straight to teardown of what already started.
fn boot(
    layout: &prep::Layout,
    inputs: &RuntimeInputs,
    fleet: &mut [Supervised],
    reporter: &Reporter,
) -> Result<()> {
    for index in 0..fleet.len() {
        if stop_requested() {
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
            if stop_requested() {
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
) {
    loop {
        if stop_requested() {
            return;
        }
        for index in 0..fleet.len() {
            let Some(phase) = fleet[index].phase else {
                continue; // never started (impossible after a full boot)
            };
            let now = Instant::now();
            let observed = observe(&mut fleet[index], phase);
            let directive = step(phase, observed, stop_requested(), now, &mut fleet[index].history);
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
/// checkpointing every transition. Also serves as the boot-failure unwind:
/// services boot never reached simply have no process to stop.
fn teardown(fleet: &mut [Supervised], reporter: &Reporter) {
    for index in (0..fleet.len()).rev() {
        let name = fleet[index].def.name;
        match fleet[index].proc.take() {
            Some(mut proc) => {
                fleet[index].status = Status::Stopping;
                reporter.checkpoint(fleet);
                match proc.shutdown(STOP_GRACE, STOP_FORCE) {
                    Ok(Outcome::Graceful(_)) => println!("weles: {name} stopped"),
                    Ok(Outcome::Forced(_)) => println!("weles: {name} force-stopped"),
                    Err(error) => eprintln!("weles: stopping {name} failed: {error:#}"),
                }
                fleet[index].status = Status::Stopped;
                reporter.checkpoint(fleet);
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
    reporter.checkpoint(fleet);
    println!("weles: fleet stopped");
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

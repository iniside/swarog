//! Pure restart-policy tests: every decision flows through an injected `now`
//! (`base + offset` Instants, never a slept-on real clock), so these cover
//! the crash → backoff → respawn / give-up branches without any process or
//! timing dependence (timing-tests doctrine).

use super::*;
use std::path::Path;
use std::sync::{MutexGuard, OnceLock};

fn base() -> Instant {
    Instant::now()
}

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

// ---------------------------------------------------------------------------
// next_restart / RestartHistory — the full case table
// ---------------------------------------------------------------------------

#[test]
fn backoff_case_table_doubles_from_one_second_then_gives_up_on_the_fifth() {
    let t0 = base();
    let mut history = RestartHistory::default();
    // Crashes 100s apart with NO healthy period in between stay consecutive:
    // only 60s of CONTINUOUS Healthy resets the counter, never mere time.
    let expected_delays = [1u64, 2, 4, 8];
    for (index, delay) in expected_delays.iter().enumerate() {
        let failure_number = index as u32 + 1;
        let now = t0 + secs(100 * u64::from(failure_number));
        match next_restart(&history, now) {
            Decision::RespawnAt(at) => assert_eq!(
                at,
                now + secs(*delay),
                "failure #{failure_number} must back off {delay}s"
            ),
            Decision::GiveUp => panic!("failure #{failure_number} must still respawn"),
        }
        history.record_crash(now);
        assert_eq!(history.consecutive_failures, failure_number);
        assert_eq!(history.healthy_since, None, "a crash clears healthy_since");
    }
    // The 5th consecutive failure gives up.
    let now = t0 + secs(500);
    assert_eq!(next_restart(&history, now), Decision::GiveUp);
    history.record_crash(now);
    assert_eq!(history.consecutive_failures, MAX_CONSECUTIVE_FAILURES);
}

#[test]
fn backoff_delay_caps_at_thirty_seconds_without_overflow() {
    assert_eq!(backoff_delay(1), secs(1));
    assert_eq!(backoff_delay(2), secs(2));
    assert_eq!(backoff_delay(5), secs(16));
    assert_eq!(backoff_delay(6), BACKOFF_CAP, "2^5=32s exceeds the cap");
    assert_eq!(backoff_delay(7), BACKOFF_CAP);
    assert_eq!(backoff_delay(63), BACKOFF_CAP, "large counts must not overflow the shift");
    assert_eq!(backoff_delay(u32::MAX), BACKOFF_CAP);
    // Degenerate input (0 failures) still yields the base delay, not a panic.
    assert_eq!(backoff_delay(0), secs(1));
}

#[test]
fn sixty_seconds_of_continuous_health_resets_the_failure_counter() {
    let t0 = base();
    let mut history = RestartHistory {
        consecutive_failures: 4,
        healthy_since: None,
    };
    history.record_healthy(t0);
    // Crash after exactly 60s of health: the counter resets, so this is
    // failure #1 again — 1s backoff instead of GiveUp.
    let now = t0 + HEALTHY_RESET_AFTER;
    assert_eq!(next_restart(&history, now), Decision::RespawnAt(now + secs(1)));
    history.record_crash(now);
    assert_eq!(history.consecutive_failures, 1);
}

#[test]
fn brief_health_does_not_reset_the_failure_counter() {
    let t0 = base();
    let mut history = RestartHistory {
        consecutive_failures: 4,
        healthy_since: None,
    };
    history.record_healthy(t0);
    // Only 59s healthy: still the 5th consecutive failure → GiveUp.
    let now = t0 + secs(59);
    assert_eq!(next_restart(&history, now), Decision::GiveUp);
}

// ---------------------------------------------------------------------------
// step — per-phase transitions
// ---------------------------------------------------------------------------

#[test]
fn healthy_crash_enters_backoff_and_drops_healthy_since() {
    let t0 = base();
    let mut history = RestartHistory::default();
    history.record_healthy(t0);
    let now = t0 + secs(10); // healthy for only 10s: no reset, failure #1
    let directive = step(
        Phase::Healthy { healthy_since: t0 },
        Observed::Exited,
        false,
        now,
        &mut history,
    );
    assert_eq!(directive, Directive::Stay(Phase::Backoff { respawn_at: now + secs(1) }));
    assert_eq!(history.consecutive_failures, 1);
    assert_eq!(history.healthy_since, None);
}

#[test]
fn healthy_alive_is_a_no_op() {
    let t0 = base();
    let mut history = RestartHistory::default();
    let phase = Phase::Healthy { healthy_since: t0 };
    assert_eq!(
        step(phase, Observed::Alive, false, t0 + secs(5), &mut history),
        Directive::Stay(phase)
    );
    assert_eq!(history.consecutive_failures, 0);
}

#[test]
fn fifth_consecutive_crash_from_healthy_fails_the_service() {
    let t0 = base();
    let mut history = RestartHistory {
        consecutive_failures: 4,
        healthy_since: None,
    };
    let directive = step(
        Phase::Healthy { healthy_since: t0 },
        Observed::Exited,
        false,
        t0 + secs(1),
        &mut history,
    );
    assert_eq!(directive, Directive::Stay(Phase::Failed));
}

#[test]
fn waiting_ready_becomes_healthy_and_records_healthy_since() {
    let t0 = base();
    let mut history = RestartHistory::default();
    let now = t0 + secs(3);
    let directive = step(
        Phase::WaitingHealthy { deadline: t0 + HEALTH_DEADLINE },
        Observed::Ready,
        false,
        now,
        &mut history,
    );
    assert_eq!(directive, Directive::Stay(Phase::Healthy { healthy_since: now }));
    assert_eq!(history.healthy_since, Some(now));
}

#[test]
fn waiting_exit_is_a_crash_without_a_kill() {
    let t0 = base();
    let mut history = RestartHistory::default();
    let now = t0 + secs(2);
    let directive = step(
        Phase::WaitingHealthy { deadline: t0 + HEALTH_DEADLINE },
        Observed::Exited,
        false,
        now,
        &mut history,
    );
    // Already dead: Stay(Backoff), never Kill.
    assert_eq!(directive, Directive::Stay(Phase::Backoff { respawn_at: now + secs(1) }));
}

#[test]
fn waiting_not_ready_before_the_deadline_keeps_waiting() {
    let t0 = base();
    let mut history = RestartHistory::default();
    let phase = Phase::WaitingHealthy { deadline: t0 + HEALTH_DEADLINE };
    assert_eq!(
        step(phase, Observed::NotReady, false, t0 + secs(1), &mut history),
        Directive::Stay(phase)
    );
    assert_eq!(history.consecutive_failures, 0);
}

#[test]
fn waiting_deadline_blown_kills_then_backs_off() {
    let t0 = base();
    let mut history = RestartHistory::default();
    let deadline = t0 + HEALTH_DEADLINE;
    let now = deadline + secs(1);
    let directive = step(
        Phase::WaitingHealthy { deadline },
        Observed::NotReady,
        false,
        now,
        &mut history,
    );
    // Alive-but-hung counts as a failure AND the process must be killed.
    assert_eq!(directive, Directive::Kill(Phase::Backoff { respawn_at: now + secs(1) }));
    assert_eq!(history.consecutive_failures, 1);
}

#[test]
fn waiting_deadline_blown_on_the_fifth_failure_kills_then_fails() {
    let t0 = base();
    let mut history = RestartHistory {
        consecutive_failures: 4,
        healthy_since: None,
    };
    let deadline = t0 + HEALTH_DEADLINE;
    let directive = step(
        Phase::WaitingHealthy { deadline },
        Observed::Alive,
        false,
        deadline,
        &mut history,
    );
    assert_eq!(directive, Directive::Kill(Phase::Failed));
}

#[test]
fn failed_stays_failed_no_matter_what() {
    let t0 = base();
    let mut history = RestartHistory::default();
    for observed in [Observed::Alive, Observed::Exited, Observed::Ready, Observed::NotReady] {
        assert_eq!(
            step(Phase::Failed, observed, false, t0, &mut history),
            Directive::Stay(Phase::Failed)
        );
    }
}

// ---------------------------------------------------------------------------
// The reviewed failing branch: STOP mid-backoff → ZERO respawns
// ---------------------------------------------------------------------------

#[test]
fn crash_backoff_respawn_scenario_with_stop_denying_the_respawn() {
    let t0 = base();
    let mut history = RestartHistory::default();

    // A healthy service crashes at t0+10s → Backoff ending at +1s.
    let crash_at = t0 + secs(10);
    let directive = step(
        Phase::Healthy { healthy_since: t0 },
        Observed::Exited,
        false,
        crash_at,
        &mut history,
    );
    let backoff = Phase::Backoff { respawn_at: crash_at + secs(1) };
    assert_eq!(directive, Directive::Stay(backoff));

    // Tick before the backoff elapses: nothing happens.
    assert_eq!(
        step(backoff, Observed::Exited, false, crash_at + Duration::from_millis(500), &mut history),
        Directive::Stay(backoff)
    );

    // Backoff elapsed but STOP is set: the respawn MUST be denied — teardown
    // is about to run and a fresh child would race it (the pinned branch).
    assert_eq!(
        step(backoff, Observed::Exited, true, crash_at + secs(2), &mut history),
        Directive::Stay(backoff),
        "stop mid-backoff must yield zero respawns"
    );
    assert_eq!(history.consecutive_failures, 1, "the denied respawn is not a new failure");

    // Same instant without STOP: the respawn happens.
    assert_eq!(
        step(backoff, Observed::Exited, false, crash_at + secs(2), &mut history),
        Directive::Respawn
    );
}

// ---------------------------------------------------------------------------
// The reversed control-endpoint contract (#2): the fleet stop is a threaded
// `Arc`, and a `down` DURING boot exits boot before it spawns anything. These
// pin the mid-boot-down branch by construction — no real binaries, no clock.
// The `down`-flips-the-Arc and bind-fail-before-boot halves live beside the
// transport in `control_tests.rs`
// (`status_and_down_roundtrip_over_the_real_transport`,
// `bind_failure_errors_without_setting_the_fleet_stop`); composed with the
// boot exit here they prove "`down` during boot exits boot".
// ---------------------------------------------------------------------------

/// A `ServiceDef` whose only job is to give `boot` a non-empty fleet to iterate
/// — its ports/name are never read because the stop check precedes any spawn.
fn dummy_def() -> ServiceDef {
    ServiceDef {
        name: "dummy-svc".to_string(),
        pkg: "dummy-svc".to_string(),
        provider: Some("dummy".to_string()),
        http_port: 65000,
        edge_port: None,
        player_port: None,
        addrs: manifest::Addrs::Told(Vec::new()),
        env: std::collections::BTreeMap::new(),
    }
}

/// A `Reporter` that boot never actually checkpoints through (it returns before
/// the first checkpoint when stop is set on entry), so its state path is a
/// never-written scratch path.
fn dummy_reporter() -> Reporter {
    let supervisor = ProcessIdentity {
        pid: std::process::id(),
        started_unix: 1,
    };
    Reporter {
        state_path: std::env::temp_dir().join("weles-a3-unused-state.json"),
        run_id: "a3-test".to_string(),
        topology: "split".to_string(),
        supervisor,
        pinned_generation: None,
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: "split".to_string(),
            status: FleetStatus::Starting,
            control_endpoint: None,
            pinned_generation: None,
            services: Vec::new(),
        })),
    }
}

#[test]
fn the_early_checkpoint_records_the_pin_with_an_empty_fleet() {
    // The pin must be persisted at the EARLIEST safe point (empty fleet, status
    // Starting, live pid) so a concurrent deploy sees it DURING the slow prep
    // helpers — not only after mint_ca/seed_admin. This drives that early write
    // directly (no helpers) and asserts state.json carries exactly what
    // `live_pinned_generation` needs: a non-terminal status, the live pid, and
    // the pinned generation name.
    let dir = std::env::temp_dir().join(format!(
        "weles-early-pin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let state_path = dir.join("state.json");
    let supervisor = ProcessIdentity {
        pid: std::process::id(),
        started_unix: unix_now(),
    };
    let reporter = Reporter {
        state_path: state_path.clone(),
        run_id: "early-pin".to_string(),
        topology: "split".to_string(),
        supervisor,
        pinned_generation: Some("gen-1".to_string()),
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: "split".to_string(),
            status: FleetStatus::Starting,
            control_endpoint: None,
            pinned_generation: None,
            services: Vec::new(),
        })),
    };

    // The early write: an EMPTY fleet, exactly as run_up does before the helpers.
    reporter.checkpoint(&[]);

    let loaded = crate::state::load(&state_path)
        .expect("load state")
        .expect("state file exists after the early checkpoint");
    assert_eq!(
        loaded.pinned_generation.as_deref(),
        Some("gen-1"),
        "the early checkpoint must carry the pinned generation"
    );
    assert!(
        !loaded.status.is_terminal(),
        "the early checkpoint is non-terminal (Starting) so a deploy protects the pin"
    );
    assert_eq!(loaded.supervisor.pid, std::process::id(), "carries the live pid");
    assert!(loaded.services.is_empty(), "the fleet is not built yet at the early write");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Serializes every test in this binary that reads or writes the process-global
/// `STOP` static. `cargo test` runs tests concurrently and there is no
/// `--test-threads=1` in this repo, so a test that flips `STOP` would otherwise
/// race any `STOP`-sensitive reader. Same `OnceLock<Mutex<()>>` shape as
/// `prep_tests.rs::env_guard` — copied with provenance, not imported (zero-sharing).
/// Poison-tolerant: a panicking guarded test must not wedge the rest. It ALSO
/// resets `STOP` to `false` on drop (before releasing the lock), so a guarded
/// test that panics mid-body — after setting `STOP=true` but before its own
/// manual reset — cannot leave a stale `STOP=true` to wedge the next holder.
fn stop_guard() -> StopGuard {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let held = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    StopGuard { _held: held }
}

/// RAII guard returned by `stop_guard`. Drop order is the fix: a struct's own
/// `Drop::drop` runs BEFORE its fields are dropped, so `STOP` is reset to
/// `false` while the mutex is still held — the reset happens-before the next
/// holder acquires the lock, so it can never observe a stale `STOP`.
struct StopGuard {
    _held: MutexGuard<'static, ()>,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        // Reset first (while `_held` still owns the lock); `_held` drops after,
        // releasing the lock only once `STOP` is already clear.
        STOP.store(false, Ordering::SeqCst);
    }
}

#[test]
fn stop_requested_honors_the_threaded_fleet_stop() {
    // Reads `STOP` through `stop_requested` (the `!stop_requested(&clear)` arm
    // asserts STOP is clear), so it must hold `stop_guard` against any concurrent
    // test that sets STOP=true — otherwise that assertion flakes.
    let _guard = stop_guard();
    // The threaded fleet stop is sufficient on its own: a `weles down` request
    // (which flips only this Arc, never the signal-handler `STOP` static) still
    // requests a stop. And with neither flag set, no stop is requested.
    let set = AtomicBool::new(true);
    assert!(
        stop_requested(&set),
        "a set fleet_stop must request a stop even with the signal STOP clear"
    );
    let clear = AtomicBool::new(false);
    assert!(
        !stop_requested(&clear),
        "no stop is requested when neither the signal STOP nor fleet_stop is set"
    );
}

#[test]
fn boot_with_the_fleet_stop_set_on_entry_spawns_nothing() {
    // Robust to STOP because fleet_stop=true already forces the gate; no guard
    // needed unless this is made STOP-sensitive.
    // The mid-boot-down branch (supervisor.rs boot loop: `if stop_requested`
    // FIRST, before ensure_no_stale_listener / spawn). A `fleet_stop` already
    // true on entry — as a `down` received before boot reaches the service would
    // leave it — must return Ok(()) with the process still unspawned. A NON-empty
    // fleet is used deliberately so the loop actually reaches (and returns from)
    // the stop check, rather than trivially skipping an empty range.
    let layout = prep::Layout::for_test(
        std::env::temp_dir(),
        std::env::temp_dir(),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let reporter = dummy_reporter();
    let mut fleet = vec![Supervised::new(dummy_def())];
    let fleet_stop = Arc::new(AtomicBool::new(true));

    let defs = [dummy_def()];
    let ctx = SpawnCtx { layout: &layout, passthrough: &[], defs: &defs };
    let result = boot(&ctx, &mut fleet, &reporter, &fleet_stop);

    assert!(result.is_ok(), "a stop on boot entry is a clean interrupt, not an error");
    assert!(
        fleet[0].proc.is_none(),
        "boot must spawn nothing once the fleet stop is set on entry"
    );
    assert!(
        fleet[0].phase.is_none(),
        "the service never advanced past Starting"
    );
}

#[test]
fn boot_with_the_signal_stop_set_on_entry_spawns_nothing() {
    // proves STOP is observed by boot's entry gate (the STOP arm of
    // stop_requested's ||). The sibling above pins the fleet_stop arm; this pins
    // the OTHER arm by flipping the process-global signal `STOP` static directly
    // (fleet_stop stays false), so it is `STOP` — not fleet_stop — that halts
    // boot. Guarded because it WRITES the process-global STOP (A3); the guard's
    // Drop resets STOP=false under the held lock (even on panic), so no later
    // test observes a stale STOP — no manual reset needed.
    let _guard = stop_guard();
    STOP.store(true, Ordering::SeqCst);

    let layout = prep::Layout::for_test(
        std::env::temp_dir(),
        std::env::temp_dir(),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let reporter = dummy_reporter();
    let mut fleet = vec![Supervised::new(dummy_def())];
    // fleet_stop is CLEAR: only the signal STOP may halt boot here.
    let fleet_stop = Arc::new(AtomicBool::new(false));

    let defs = [dummy_def()];
    let ctx = SpawnCtx { layout: &layout, passthrough: &[], defs: &defs };
    let result = boot(&ctx, &mut fleet, &reporter, &fleet_stop);

    assert!(result.is_ok(), "a signal STOP on boot entry is a clean interrupt, not an error");
    assert!(
        fleet[0].proc.is_none(),
        "boot must spawn nothing once the signal STOP is set on entry"
    );
    assert!(
        fleet[0].phase.is_none(),
        "the service never advanced past Starting"
    );
    // No manual STOP reset: `_guard`'s Drop clears STOP (under the held lock)
    // whether this test returns normally or unwinds on a failed assertion.
}

// ---------------------------------------------------------------------------
// checkpoint_critical (#P3): the early pin write is fail-closed.
// ---------------------------------------------------------------------------

#[test]
fn checkpoint_critical_fails_closed_where_checkpoint_swallows() {
    // The previously-swallowed branch: `checkpoint` eprintln!s and returns `()`
    // on a write failure, so a fleet whose initial pin cannot be persisted would
    // start blind (no retention protection). `checkpoint_critical` must instead
    // return Err so run_up refuses to start. A state_path inside a nonexistent
    // directory cannot be written — exercising exactly that branch, no process,
    // no clock. The contrast (`checkpoint` swallowing the SAME path) is asserted
    // too, pinning that only the critical variant is fatal.
    let missing = std::env::temp_dir()
        .join(format!("weles-p3-missing-{}", std::process::id()))
        .join("does-not-exist")
        .join("state.json");
    let supervisor = ProcessIdentity {
        pid: std::process::id(),
        started_unix: 1,
    };
    let reporter = Reporter {
        state_path: missing,
        run_id: "p3".to_string(),
        topology: "split".to_string(),
        supervisor,
        pinned_generation: Some("gen-1".to_string()),
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: "split".to_string(),
            status: FleetStatus::Starting,
            control_endpoint: None,
            pinned_generation: None,
            services: Vec::new(),
        })),
    };

    let error = reporter
        .checkpoint_critical(&[])
        .expect_err("checkpoint_critical must fail when the state dir is unwritable");
    assert!(
        error.to_string().contains("persist initial state"),
        "the error must name the failed persist, got: {error:#}"
    );

    // The best-effort variant swallows the SAME failure (returns `()`, no panic):
    // it is the reason the fatal variant had to exist.
    reporter.checkpoint(&[]);
}

// ---------------------------------------------------------------------------
// stop_outcome (#P2): the one authority for teardown accuracy.
// ---------------------------------------------------------------------------

#[test]
fn stop_outcome_maps_every_shutdown_result_to_status_and_cleanliness() {
    // A pure case table: a fixture cannot reproduce a shutdown-`Err` (force is a
    // non-blockable SIGKILL / TerminateJobObject from userspace), so the
    // previously-wrong branch is pinned purely, not by an integration process.
    use crate::platform::ExitInfo;

    // Both Ok variants mean the process is CONFIRMED gone → Stopped, clean.
    assert_eq!(
        stop_outcome(&Ok(Outcome::Graceful(ExitInfo::from_code(Some(0))))),
        (Status::Stopped, true),
        "a graceful exit is a clean Stopped"
    );
    assert_eq!(
        stop_outcome(&Ok(Outcome::Forced(ExitInfo::from_code(None)))),
        (Status::Stopped, true),
        "Forced is a confirmed exit — a console-less weles degrades EVERY stop to \
         Forced, so it must count as clean"
    );

    // The previously-WRONG branch: an Err (force could not confirm the exit)
    // was reported as an unconditional Stopped. It is now Failed AND unclean —
    // a possible orphan the fleet's exit code must surface.
    assert_eq!(
        stop_outcome(&Err(anyhow::anyhow!("force timed out"))),
        (Status::Failed, false),
        "an unconfirmed stop is Failed and unclean, never a false Stopped"
    );
}

// ---------------------------------------------------------------------------
// phase_after_kill (#A1): the Kill-loop policy layered on stop_outcome's
// confirmed-gone authority — an unconfirmed kill gives up (`Failed`) rather than
// respawning a second instance over a possible orphan.
// ---------------------------------------------------------------------------

#[test]
fn phase_after_kill_gives_up_on_an_unconfirmed_kill_instead_of_respawning() {
    use crate::platform::ExitInfo;

    let respawn_at = base() + secs(1);
    let backoff = Phase::Backoff { respawn_at };

    // The previously-WRONG branch: an Err (force could not confirm the exit) was
    // adopted UNCONDITIONALLY as the intended `Backoff`, which the monitor would
    // later Respawn — a second process over a possible orphan. It is now `Failed`.
    assert_eq!(
        phase_after_kill(&Err(anyhow::anyhow!("force timed out")), backoff),
        Phase::Failed,
        "an unconfirmed kill gives up (Failed), never Backoff — no respawn over an orphan"
    );

    // A CONFIRMED stop adopts the intended phase unchanged: Forced is a real,
    // confirmed exit (console-less weles degrades every stop to Forced).
    assert_eq!(
        phase_after_kill(&Ok(Outcome::Forced(ExitInfo::from_code(Some(1)))), backoff),
        backoff,
        "a confirmed (Forced) kill adopts the intended Backoff"
    );
    assert_eq!(
        phase_after_kill(&Ok(Outcome::Graceful(ExitInfo::from_code(Some(0)))), Phase::Failed),
        Phase::Failed,
        "a confirmed (Graceful) kill adopts the intended phase, here Failed"
    );

    // The status table must show Failed, not Backoff, for the unconfirmed kill.
    assert_eq!(
        status_of(phase_after_kill(&Err(anyhow::anyhow!("x")), backoff)),
        Status::Failed,
        "the checkpoint status of an unconfirmed kill is Failed, not Backoff"
    );
}

// ---------------------------------------------------------------------------
// Readiness (#3): a post-healthy `/readyz` dimension that NEVER restarts.
// The authority is structural — a POLLER probe becomes a `Readiness` (never an
// `Observed`/`Directive`), and `fold_readiness` writes ONLY `readiness`. These
// prove the at-risk wiring where it lives, not with a tautological `step()`.
// The other two mechanisms (`step`'s Healthy catch-all, `observe` never forging
// an `Exited`) are pinned at the bottom of this file.
// ---------------------------------------------------------------------------

/// A `Supervised` pinned into a chosen phase/status for the readiness tests.
fn supervised_in(phase: Phase, status: Status) -> Supervised {
    let mut svc = Supervised::new(dummy_def());
    svc.phase = Some(phase);
    svc.status = status;
    svc
}

#[test]
fn probe_result_maps_to_readiness_on_every_variant() {
    // The ONE place a probe becomes a verdict — and it yields nothing but a
    // `Readiness`, so no probe outcome can synthesize a restart input.
    assert_eq!(readiness_for(ProbeResult::Ready), Readiness::Ready);
    assert_eq!(readiness_for(ProbeResult::NotReady), Readiness::Degraded);
    assert_eq!(
        readiness_for(ProbeResult::ConnectFailed),
        Readiness::Unreachable
    );
}

#[test]
fn healthy_service_probed_not_ready_records_degraded_without_touching_the_restart_state() {
    // The pinned failing branch: a Healthy service whose `/readyz` answers a 503
    // (ProbeResult::NotReady) must record Degraded and NOTHING else — no phase
    // change, no failure count, no respawn. Folding is the WHOLE effect a probe
    // has on supervised state.
    let t0 = base();
    let mut svc = Supervised::new(dummy_def());
    svc.phase = Some(Phase::Healthy { healthy_since: t0 });
    svc.status = Status::Healthy;
    svc.history.record_healthy(t0);

    let latest = vec![readiness_for(ProbeResult::NotReady)];
    let mut fleet = vec![svc];
    let changed = fold_readiness(&mut fleet, &latest);

    assert!(changed, "a readiness change must request a checkpoint");
    assert_eq!(fleet[0].readiness, Readiness::Degraded, "503 → Degraded");
    // The restart lifecycle is UNTOUCHED — the invariant "503 never restarts".
    assert_eq!(
        fleet[0].phase,
        Some(Phase::Healthy { healthy_since: t0 }),
        "readiness must not advance the phase"
    );
    assert_eq!(fleet[0].status, Status::Healthy);
    assert_eq!(fleet[0].restarts, 0);
    assert_eq!(fleet[0].history.consecutive_failures, 0);
    assert_eq!(fleet[0].history.healthy_since, Some(t0));

    // And the ONLY thing that CAN restart this service — the liveness `step` —
    // is driven by `Observed`, never the probe: an Alive tick is a no-op even
    // though readiness is now Degraded (no Respawn/Kill directive).
    let directive = step(
        Phase::Healthy { healthy_since: t0 },
        Observed::Alive,
        false,
        t0 + secs(1),
        &mut fleet[0].history,
    );
    assert_eq!(
        directive,
        Directive::Stay(Phase::Healthy { healthy_since: t0 }),
        "a Degraded readiness must not turn a live Healthy service into a Respawn"
    );
}

#[test]
fn folding_readiness_never_perturbs_phase_status_or_the_failure_count() {
    // Poller ⊥ monitor: updating the readiness vector across a fleet spanning
    // every phase mutates ONLY `readiness`; the restart-lifecycle fields are
    // byte-for-byte unchanged, and `fold_readiness` returns a bool — never a
    // `Directive` (the type system already forbids it from restarting anything).
    let t0 = base();
    let mut fleet = vec![
        supervised_in(Phase::Healthy { healthy_since: t0 }, Status::Healthy),
        supervised_in(
            Phase::Backoff {
                respawn_at: t0 + secs(1),
            },
            Status::Backoff,
        ),
        supervised_in(
            Phase::WaitingHealthy {
                deadline: t0 + HEALTH_DEADLINE,
            },
            Status::WaitingHealthy,
        ),
        supervised_in(Phase::Failed, Status::Failed),
    ];
    let before: Vec<_> = fleet
        .iter()
        .map(|svc| {
            (
                svc.phase,
                svc.status,
                svc.restarts,
                svc.history.consecutive_failures,
            )
        })
        .collect();

    let latest = vec![
        Readiness::Degraded,
        Readiness::Unreachable,
        Readiness::Ready,
        Readiness::Unknown,
    ];
    let changed = fold_readiness(&mut fleet, &latest);
    assert!(changed);

    for (index, svc) in fleet.iter().enumerate() {
        assert_eq!(
            (
                svc.phase,
                svc.status,
                svc.restarts,
                svc.history.consecutive_failures
            ),
            before[index],
            "fold_readiness perturbed the restart lifecycle of service {index}"
        );
        assert_eq!(svc.readiness, latest[index], "readiness must be applied");
    }
}

#[test]
fn next_probe_index_round_robins_over_healthy_and_skips_the_rest() {
    // Services 0 and 2 Healthy, 1 not: the cursor visits only Healthy indices.
    let healthy = [true, false, true];
    assert_eq!(next_probe_index(&healthy, 2), Some(0), "cursor 2 → wrap to 0");
    assert_eq!(next_probe_index(&healthy, 0), Some(2), "skip non-Healthy 1");
    assert_eq!(next_probe_index(&healthy, 2), Some(0), "wrap back to 0");

    // Over N cycles each Healthy service is probed the same number of times and
    // the non-Healthy one is never probed.
    let mut cursor = healthy.len() - 1;
    let mut hits = [0usize; 3];
    for _ in 0..6 {
        let index = next_probe_index(&healthy, cursor).expect("a Healthy service exists");
        hits[index] += 1;
        cursor = index;
    }
    assert_eq!(hits, [3, 0, 3], "each Healthy probed equally, non-Healthy never");
}

#[test]
fn next_probe_index_handles_no_healthy_and_empty_without_panicking() {
    assert_eq!(next_probe_index(&[false, false], 0), None, "no Healthy → None");
    assert_eq!(
        next_probe_index(&[], 0),
        None,
        "empty fleet → None, never a div-by-zero"
    );
    // A cursor past the end must wrap, not panic.
    assert_eq!(next_probe_index(&[true], 999), Some(0));
}

// ---------------------------------------------------------------------------
// The other two mechanisms behind "readiness never restarts" (the readiness
// POLLER thread is not one of them — it exists for latency): `step`'s
// `Phase::Healthy` catch-all, and `observe` never forging an `Observed::Exited`
// out of a probe. Both are pure-authority guards a future refactor (M1: probe
// I/O onto a runtime) could silently break, so they are pinned here.
// ---------------------------------------------------------------------------

#[test]
fn a_healthy_service_restarts_on_a_real_exit_alone_and_ignores_every_probe_derived_observation() {
    // Mechanism (b): the `Phase::Healthy` arm of `step` is a catch-all around
    // `Observed::Exited`. Feed it every observation a probe could possibly
    // produce — a 200 (`Ready`) and a 503/unreachable (`NotReady`) — and the
    // service must simply stay Healthy: no Respawn, no Kill, no failure counted,
    // no `healthy_since` cleared. (Mirrors `failed_stays_failed_no_matter_what`
    // for the phase where a `/readyz` blip actually happens.)
    let t0 = base();
    let phase = Phase::Healthy { healthy_since: t0 };
    for observed in [Observed::Alive, Observed::Ready, Observed::NotReady] {
        for stop in [false, true] {
            let mut history = RestartHistory::default();
            history.record_healthy(t0);
            let directive = step(phase, observed, stop, t0 + secs(1), &mut history);
            assert_eq!(
                directive,
                Directive::Stay(phase),
                "{observed:?} (stop={stop}) must leave a Healthy service alone"
            );
            assert_eq!(
                history.consecutive_failures, 0,
                "{observed:?} must not count a failure"
            );
            assert_eq!(
                history.healthy_since,
                Some(t0),
                "{observed:?} must not clear healthy_since"
            );
        }
    }

    // The contrast that proves the assertions above are not vacuous: a REAL
    // process exit — the one observation a probe cannot forge (see `observe`) —
    // does restart the same service from the same state.
    let mut history = RestartHistory::default();
    history.record_healthy(t0);
    let crash_at = t0 + secs(1);
    assert_eq!(
        step(phase, Observed::Exited, false, crash_at, &mut history),
        Directive::Stay(Phase::Backoff {
            respawn_at: crash_at + secs(1)
        }),
        "only a real process exit restarts a Healthy service"
    );
    assert_eq!(history.consecutive_failures, 1);
    assert_eq!(history.healthy_since, None);
}

/// The blocking child for the `observe` test: a REAL process that stays alive
/// and listens on NOTHING. `#[ignore]`d, so an ordinary `cargo test` never runs
/// it — `spawn_live_child` re-execs this very test binary with
/// `--ignored --exact <this test>` to obtain a live `OwnedProc`. (Re-exec-self
/// rather than `tests/platform.rs`'s `__test-child`: `CARGO_BIN_EXE_weles` is
/// defined for integration-test targets only, and `observe` is private to this
/// module, so it can only be called from an in-crate unit test.)
#[test]
#[ignore = "spawned as a child process by the observe test; not a test itself"]
fn blocking_child_fixture_for_the_observe_test() {
    println!("observe-fixture: ready");
    std::io::Write::flush(&mut std::io::stdout()).ok();
    // Hang guard: never outlive the test run (mirrors `fixture.rs`'s 60s).
    std::thread::sleep(secs(60));
}

const OBSERVE_FIXTURE_TEST: &str = concat!(
    "supervisor::supervisor_tests::",
    "blocking_child_fixture_for_the_observe_test"
);

/// Removes a scratch dir on drop (declared BEFORE the `OwnedProc` holder, so
/// the child — and its stdout handle — is gone before the removal runs).
struct ScratchDir {
    path: PathBuf,
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn scratch_dir(name: &str) -> ScratchDir {
    let path = std::env::temp_dir().join(format!("weles-observe-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    ScratchDir { path }
}

/// Spawns [`blocking_child_fixture_for_the_observe_test`] as a real child and
/// returns once it has printed its ready marker — so "the process is alive" is
/// guaranteed by construction, not by a sleep.
fn spawn_live_child(dir: &Path) -> OwnedProc {
    let stdout_path = dir.join("child.log");
    let stdout = File::create(&stdout_path).expect("create child log");
    let stderr = File::create(dir.join("child.err.log")).expect("create child err log");
    // Minimal deliberate environment (SystemRoot is required by Win32 for a
    // working child), same shape as `tests/platform.rs::fixture_env`.
    let mut env = std::collections::BTreeMap::new();
    for key in ["SystemRoot", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(std::ffi::OsString::from(key), value);
        }
    }
    let proc = platform::spawn(SpawnSpec {
        program: std::env::current_exe().expect("resolve this test binary"),
        args: vec![
            "--exact".into(),
            OBSERVE_FIXTURE_TEST.into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        env,
        cwd: Some(dir.to_path_buf()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .expect("spawn the blocking child fixture");

    // Bounded poll for the marker: the deadline only bounds a condition the
    // fixture reaches unconditionally (it prints before blocking), so this is
    // never a race against a real clock. A filter typo makes the child exit
    // without the marker → a loud, self-explaining failure here.
    let deadline = Instant::now() + secs(20);
    loop {
        let contents = std::fs::read_to_string(&stdout_path).unwrap_or_default();
        if contents.contains("observe-fixture: ready") {
            return proc;
        }
        assert!(
            Instant::now() < deadline,
            "the child fixture never signalled ready (is {OBSERVE_FIXTURE_TEST} still named that?); \
             child stdout: {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An ephemeral port HELD against every other binder, to be released only at the
/// point of use. This is the `ProbeResult::ConnectFailed` input for `observe`.
///
/// Bind-then-immediately-release (the `health_tests::closed_port` pattern) would
/// leave the port up for grabs across this test's whole setup — a `spawn` plus a
/// marker poll, hundreds of ms. That window has a concrete in-suite adversary:
/// `health_tests::serve_once` binds `("127.0.0.1", 0)` concurrently in THIS test
/// binary, and `probe_reports_ready_on_200` has one of those answer a literal
/// `HTTP/1.1 200 OK`. Were it handed this port, `observe` would see `Ready` and
/// the test would go red for a reason unrelated to the invariant.
///
/// Holding the listener bound closes that window structurally rather than
/// probabilistically: a bound port cannot be handed to another `bind(port 0)`,
/// so for the entire setup the collision is impossible, not merely unlikely.
/// Nothing probes the port before `observe`, so an unaccepting listener costs
/// nothing. [`PortClaim::release_and_assert_refused`] then drops it and asserts
/// the refusal AT the point of use, leaving only CPU between the check and
/// `observe` — and a residual squatter hits that assert with a self-explaining
/// message instead of corrupting the verdict.
struct PortClaim {
    listener: std::net::TcpListener,
    port: u16,
}

impl PortClaim {
    fn claim() -> Self {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        PortClaim { listener, port }
    }

    fn port(&self) -> u16 {
        self.port
    }

    /// Releases the claim and asserts the port now refuses. Call this
    /// IMMEDIATELY before the `observe` under test: it consumes the claim, so
    /// the hold cannot be accidentally extended past the assert, and the assert
    /// cannot be accidentally hoisted back above the setup.
    fn release_and_assert_refused(self) {
        let port = self.port;
        drop(self.listener);
        assert_eq!(
            health::probe(port),
            ProbeResult::ConnectFailed,
            "precondition: :{port} must refuse the moment the claim is released"
        );
    }
}

#[test]
fn a_live_service_whose_readyz_refuses_the_connection_is_observed_not_ready_never_exited() {
    // Mechanism (c): `Observed::Exited` — the ONE observation that restarts a
    // service — is unforgeable from a probe. This drives `observe` with the most
    // extreme probe failure there is (nothing listening AT ALL, worse than the
    // 503 a Postgres blip yields) against a process that is very much alive, on
    // a REAL `OwnedProc` and a REAL refused TCP connect. It must read NotReady;
    // reading Exited would restart a service whose only sin is a cold /readyz.
    let dir = scratch_dir("refused");
    // Held (not merely sampled) across the spawn + marker poll below, so no
    // concurrent binder in this test binary can be handed the same port.
    let claim = PortClaim::claim();

    let mut svc = Supervised::new(ServiceDef {
        http_port: claim.port(),
        ..dummy_def()
    });
    svc.proc = Some(spawn_live_child(&dir.path));
    assert!(
        svc.proc
            .as_mut()
            .expect("fixture proc")
            .try_wait()
            .expect("try_wait")
            .is_none(),
        "precondition: the child fixture must be alive"
    );

    let t0 = base();
    // The last statement before the call under test: from here to `observe`
    // there is no I/O and no scheduling point, only this frame's own CPU.
    claim.release_and_assert_refused();
    // The boot gate DOES probe (by design — a service that never comes up must
    // be killed), and a refused connect there is NotReady, never Exited.
    assert_eq!(
        observe(&mut svc, Phase::WaitingHealthy { deadline: t0 + HEALTH_DEADLINE }),
        Observed::NotReady,
        "a refused /readyz on a LIVE process is NotReady, never Exited"
    );
    // Past the gate, the probe is not even consulted: bare liveness. This is
    // what makes a `Healthy` service's readiness a checkpoint-only dimension.
    assert_eq!(
        observe(&mut svc, Phase::Healthy { healthy_since: t0 }),
        Observed::Alive,
        "a Healthy service reports bare liveness, not a probe verdict"
    );

    // Contrast (so the NotReady/Alive above are not constants): real death — and
    // ONLY real death — is Exited. Forcing the container guarantees the exit, so
    // the deadline below bounds a condition, it does not race one.
    let proc = svc.proc.as_mut().expect("fixture proc");
    proc.force().expect("force the child fixture");
    let deadline = Instant::now() + secs(10);
    while proc.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "the forced child never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        observe(&mut svc, Phase::Healthy { healthy_since: t0 }),
        Observed::Exited,
        "a dead process is Exited"
    );
    assert_eq!(
        observe(&mut svc, Phase::WaitingHealthy { deadline: t0 + HEALTH_DEADLINE }),
        Observed::Exited,
        "liveness wins over the probe: a dead process is Exited in the boot gate too"
    );
}

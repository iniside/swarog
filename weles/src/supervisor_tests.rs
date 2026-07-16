//! Pure restart-policy tests: every decision flows through an injected `now`
//! (`base + offset` Instants, never a slept-on real clock), so these cover
//! the crash → backoff → respawn / give-up branches without any process or
//! timing dependence (timing-tests doctrine).

use super::*;

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
        name: "dummy-svc",
        pkg: "dummy-svc",
        http_port: 65000,
        edge_port: None,
        player_port: None,
        has_db: false,
        pool_max: 0,
        env_extra: &[],
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
        topology: "split",
        supervisor,
        status: Cell::new(FleetStatus::Starting),
        control_endpoint: RefCell::new(None),
        shared: Arc::new(Mutex::new(FleetState {
            run_id: String::new(),
            supervisor,
            topology: "split".to_string(),
            status: FleetStatus::Starting,
            control_endpoint: None,
            services: Vec::new(),
        })),
    }
}

#[test]
fn stop_requested_honors_the_threaded_fleet_stop() {
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
    // The mid-boot-down branch (supervisor.rs boot loop: `if stop_requested`
    // FIRST, before ensure_no_stale_listener / spawn). A `fleet_stop` already
    // true on entry — as a `down` received before boot reaches the service would
    // leave it — must return Ok(()) with the process still unspawned. A NON-empty
    // fleet is used deliberately so the loop actually reaches (and returns from)
    // the stop check, rather than trivially skipping an empty range.
    let layout = prep::Layout {
        root: std::env::temp_dir(),
        run_dir: std::env::temp_dir(),
        bin_dir: std::env::temp_dir(),
    };
    let inputs = RuntimeInputs {
        database_url: String::new(),
        ca_cert: PathBuf::new(),
        ca_key: PathBuf::new(),
    };
    let reporter = dummy_reporter();
    let mut fleet = vec![Supervised::new(dummy_def())];
    let fleet_stop = Arc::new(AtomicBool::new(true));

    let result = boot(&layout, &inputs, &mut fleet, &reporter, &fleet_stop);

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

//! Control-endpoint tests. The transport tests run a same-process client and
//! server over a REAL named pipe / UDS with a temp run dir (cfg'd to the
//! supported targets — Windows + unix (Linux, darwin)). Every wait is a poll-with-deadline
//! loop — never a sleep-as-correctness (timing-tests doctrine). The frame
//! codec, identity classification, and liveness probe are platform-neutral
//! unit tests that pin the previously-wrong branches by construction.

use super::*;

use std::sync::atomic::AtomicU32;

use crate::state::{Readiness, ServiceState};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn unique() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

fn sample_state(status: FleetStatus, supervisor_pid: u32) -> FleetState {
    FleetState {
        run_id: "test-run".to_string(),
        supervisor: ProcessIdentity {
            pid: supervisor_pid,
            started_unix: 1,
        },
        topology: "split".to_string(),
        status,
        control_endpoint: None,
        pinned_generation: None,
        services: vec![
            ServiceState {
                name: "accounts-svc".to_string(),
                status: Status::Healthy,
                pid: Some(1001),
                restarts: 0,
                readiness: Readiness::Ready,
            },
            ServiceState {
                name: "gateway-svc".to_string(),
                status: Status::Backoff,
                pid: None,
                restarts: 2,
                readiness: Readiness::Unknown,
            },
        ],
    }
}

/// A control endpoint bound to a temp location; cleans up its temp dir on drop.
struct TestEndpoint {
    path: PathBuf,
    dir: Option<PathBuf>,
}

impl Drop for TestEndpoint {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn test_endpoint(tag: &str) -> TestEndpoint {
    #[cfg(windows)]
    {
        TestEndpoint {
            path: PathBuf::from(format!(
                r"\\.\pipe\gamebackend-weles-test-{tag}-{}",
                unique()
            )),
            dir: None,
        }
    }
    #[cfg(unix)]
    {
        let dir = std::env::temp_dir().join(format!("weles-control-{tag}-{}", unique()));
        std::fs::create_dir_all(&dir).expect("create control test temp dir");
        TestEndpoint {
            path: dir.join("control.sock"),
            dir: Some(dir),
        }
    }
}

fn poll_until(deadline: Duration, mut done: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + deadline;
    while !done() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Real-transport roundtrip (status + down) — the pipe/UDS path
// ---------------------------------------------------------------------------

#[cfg(any(windows, unix))]
#[test]
fn status_and_down_roundtrip_over_the_real_transport() {
    let endpoint = test_endpoint("roundtrip");
    // The server runs IN THIS process, so the peer/pid validation resolves the
    // recorded supervisor to our own pid.
    let me = std::process::id();
    let state = Arc::new(Mutex::new(sample_state(FleetStatus::Running, me)));
    let stop = Arc::new(AtomicBool::new(false));
    let server = ControlServer::bind(endpoint.path.clone(), Arc::clone(&state), Arc::clone(&stop))
        .expect("bind control server");
    let expected = ProcessIdentity {
        pid: me,
        started_unix: 1,
    };

    // status: rendered per-service table, and the stop atomic stays clear.
    let message = request(&endpoint.path, "status", &expected).expect("status request");
    assert!(message.contains("running"), "status header: {message}");
    assert!(message.contains("accounts-svc"), "status table: {message}");
    assert!(
        !stop.load(Ordering::SeqCst),
        "a status request must not request shutdown"
    );

    // down: acknowledged AND sets the supervisor stop atomic.
    let message = request(&endpoint.path, "down", &expected).expect("down request");
    assert!(
        message.to_lowercase().contains("shutdown"),
        "down ack: {message}"
    );
    poll_until(
        Duration::from_secs(5),
        || stop.load(Ordering::SeqCst),
        "down did not set the supervisor stop atomic",
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// Server teardown: prompt join, and NEVER a store into the fleet stop
// ---------------------------------------------------------------------------

#[cfg(any(windows, unix))]
#[test]
fn server_drop_joins_promptly_and_never_sets_the_fleet_stop() {
    let endpoint = test_endpoint("shutdown");
    let state = Arc::new(Mutex::new(sample_state(
        FleetStatus::Running,
        std::process::id(),
    )));
    let fleet_stop = Arc::new(AtomicBool::new(false));
    let server = ControlServer::bind(endpoint.path.clone(), state, Arc::clone(&fleet_stop))
        .expect("bind");

    // Dropping the server alone must stop and join the serve thread (bounded),
    // via its PRIVATE shutdown flag — the fleet stop is not its to touch.
    let joined = Arc::new(AtomicBool::new(false));
    let joined_flag = Arc::clone(&joined);
    let joiner = std::thread::spawn(move || {
        drop(server);
        joined_flag.store(true, Ordering::SeqCst);
    });
    poll_until(
        Duration::from_secs(5),
        || joined.load(Ordering::SeqCst),
        "control server thread did not join within 5s of drop",
    );
    joiner.join().expect("join the drop helper");
    assert!(
        !fleet_stop.load(Ordering::SeqCst),
        "server teardown must never store into the fleet stop (single stop authority)"
    );
}

// ---------------------------------------------------------------------------
// Bind failure: loud error, and the fleet stop stays FALSE (the MAJOR pin —
// a control-plane failure must never masquerade as an operator `down`)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn bind_failure_errors_without_setting_the_fleet_stop() {
    // A UDS path inside a nonexistent directory cannot bind.
    let endpoint = std::env::temp_dir()
        .join(format!("weles-control-nonexistent-{}", unique()))
        .join("control.sock");
    let state = Arc::new(Mutex::new(sample_state(
        FleetStatus::Running,
        std::process::id(),
    )));
    let fleet_stop = Arc::new(AtomicBool::new(false));
    let result = ControlServer::bind(endpoint, state, Arc::clone(&fleet_stop));
    assert!(result.is_err(), "bind into a nonexistent dir must fail");
    assert!(
        !fleet_stop.load(Ordering::SeqCst),
        "a bind failure must never store into the fleet stop"
    );
}

#[cfg(windows)]
#[test]
fn bind_failure_errors_without_setting_the_fleet_stop() {
    // FIRST_PIPE_INSTANCE: a second server on an already-owned pipe name fails.
    let endpoint = test_endpoint("bind-conflict");
    let state = Arc::new(Mutex::new(sample_state(
        FleetStatus::Running,
        std::process::id(),
    )));
    let first_stop = Arc::new(AtomicBool::new(false));
    let _first = ControlServer::bind(
        endpoint.path.clone(),
        Arc::clone(&state),
        Arc::clone(&first_stop),
    )
    .expect("first bind");

    let second_stop = Arc::new(AtomicBool::new(false));
    let result = ControlServer::bind(endpoint.path.clone(), state, Arc::clone(&second_stop));
    assert!(result.is_err(), "a second bind on an owned pipe name must fail");
    assert!(
        !second_stop.load(Ordering::SeqCst),
        "a bind failure must never store into the fleet stop"
    );
    assert!(
        !first_stop.load(Ordering::SeqCst),
        "the healthy server's fleet stop must be untouched by the failed bind"
    );
}

// ---------------------------------------------------------------------------
// wait_for_terminal: terminal outcomes + the write-then-exit race guard
// ---------------------------------------------------------------------------

/// A pid that is not a live process on Windows or Linux (huge and 4-aligned).
const DEAD_PID: u32 = 0x3FFF_FFFC;

fn write_state_file(tag: &str, status: FleetStatus, pid: u32) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("weles-terminal-{tag}-{}", unique()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("state.json");
    crate::state::checkpoint(&path, &sample_state(status, pid)).expect("write state");
    (dir, path)
}

#[test]
fn wait_for_terminal_returns_ok_on_a_stopped_state() {
    let (dir, path) = write_state_file("stopped", FleetStatus::Stopped, DEAD_PID);
    let supervisor = ProcessIdentity {
        pid: DEAD_PID,
        started_unix: 1,
    };
    // Dead supervisor + terminal Stopped state: the terminal check (and the
    // dead-supervisor re-read guard behind it) resolves Ok, never an error.
    wait_for_terminal(&path, &supervisor, Duration::from_secs(5)).expect("stopped is success");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_for_terminal_returns_err_on_a_failed_state() {
    let (dir, path) = write_state_file("failed", FleetStatus::Failed, DEAD_PID);
    let supervisor = ProcessIdentity {
        pid: DEAD_PID,
        started_unix: 1,
    };
    let error = wait_for_terminal(&path, &supervisor, Duration::from_secs(5))
        .expect_err("a Failed terminal state is an error");
    assert!(error.to_string().contains("failed"), "{error:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_for_terminal_reports_a_supervisor_that_exited_without_a_terminal_state() {
    // The race-guard's failing branch: non-terminal state + dead supervisor.
    // The guard re-reads once (still Running) and must report the premature
    // exit — not spin to the timeout, not claim success.
    let (dir, path) = write_state_file("premature", FleetStatus::Running, DEAD_PID);
    let supervisor = ProcessIdentity {
        pid: DEAD_PID,
        started_unix: 1,
    };
    let error = wait_for_terminal(&path, &supervisor, Duration::from_secs(5))
        .expect_err("dead supervisor + non-terminal state is an error");
    assert!(
        error.to_string().contains("before publishing"),
        "must name the premature exit, got: {error:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Frame codec bounds
// ---------------------------------------------------------------------------

#[test]
fn frame_roundtrips_through_the_codec() {
    let stop = AtomicBool::new(false);
    let mut buffer = Vec::new();
    write_frame(&mut buffer, b"a control frame", &stop).expect("write frame");
    let mut cursor = buffer.as_slice();
    let back = read_frame(&mut cursor, &stop).expect("read frame");
    assert_eq!(back, b"a control frame");
}

#[test]
fn oversized_frame_length_is_rejected() {
    // A length header above the bound is refused before any body is read (so
    // no unbounded allocation and no 2s stall on a missing body).
    let stop = AtomicBool::new(false);
    let mut framed = Vec::new();
    framed.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
    let mut cursor = framed.as_slice();
    assert!(
        read_frame(&mut cursor, &stop).is_err(),
        "a frame length above the bound must be refused"
    );
}

#[test]
fn write_frame_refuses_an_oversized_payload() {
    let stop = AtomicBool::new(false);
    let big = vec![0u8; MAX_FRAME + 1];
    let mut buffer = Vec::new();
    assert!(write_frame(&mut buffer, &big, &stop).is_err());
}

// ---------------------------------------------------------------------------
// Client-side identity classification (the wrong-pid / stale-state branch)
// ---------------------------------------------------------------------------

#[test]
fn classify_connects_for_a_live_running_fleet() {
    let state = sample_state(FleetStatus::Running, std::process::id());
    assert_eq!(classify(&state, now_unix(), true), Disposition::Connect);
}

#[test]
fn classify_connects_while_the_fleet_is_stopping_with_a_live_supervisor() {
    // P6: control is kept alive THROUGH teardown, so teardown checkpoints carry a
    // non-terminal `Stopping` status, a live supervisor, AND a live
    // `control_endpoint`. A concurrent `status`/`down` must classify Connect (dial
    // the LIVE endpoint and see Stopping) — never Inactive (Stopping is not
    // terminal) and never Stale (the supervisor is alive). classify ignores the
    // endpoint field, so Some(...) here only models the real teardown snapshot.
    let mut state = sample_state(FleetStatus::Stopping, std::process::id());
    state.control_endpoint = Some(r"\\.\pipe\gamebackend-weles-p6".to_string());
    assert_eq!(classify(&state, now_unix(), true), Disposition::Connect);
}

#[test]
fn classify_reports_inactive_for_a_terminal_fleet() {
    // A finished fleet (even with a dead supervisor) is inactive, not stale.
    let state = sample_state(FleetStatus::Stopped, std::process::id());
    assert!(matches!(
        classify(&state, now_unix(), false),
        Disposition::Inactive(_)
    ));
}

#[test]
fn classify_reports_stale_when_the_supervisor_is_dead() {
    // The reviewed failing branch: a state file claiming a RUNNING fleet whose
    // supervisor is not alive must be rejected as stale — never "up".
    let state = sample_state(FleetStatus::Running, 999_999_999);
    match classify(&state, now_unix(), false) {
        Disposition::Stale(message) => assert!(message.contains("stale"), "{message}"),
        other => panic!("expected Stale, got {other:?}"),
    }
}

#[test]
fn classify_rejects_a_future_started_timestamp_as_stale() {
    // Even a live pid with an implausible (future) start time is stale — that
    // is a rewritten/corrupt state file, not a real supervisor.
    let mut state = sample_state(FleetStatus::Running, std::process::id());
    state.supervisor.started_unix = now_unix() + 3600;
    assert!(matches!(
        classify(&state, now_unix(), true),
        Disposition::Stale(_)
    ));
}

#[test]
fn a_wrong_pid_running_state_is_classified_stale_end_to_end() {
    // The full client-side identity check: a fake state file naming a
    // supervisor pid that is not a live process is rejected as stale. The pid
    // is huge and 4-aligned so it is not a live process on Windows or Linux.
    let bogus_pid = 0x3FFF_FFFC;
    let state = sample_state(FleetStatus::Running, bogus_pid);
    let alive = supervisor_alive(&state.supervisor);
    assert!(!alive, "the bogus pid must not be a live process");
    assert!(matches!(
        classify(&state, now_unix(), alive),
        Disposition::Stale(_)
    ));
}

#[test]
fn supervisor_alive_is_true_for_this_process() {
    // `started_unix` = now: this process's real creation time is <= now, so the
    // Windows asymmetric reuse check sees `actual <= recorded` and never rejects
    // it. (A `started_unix` of 1 — before this process was created — would look
    // like pid reuse on Windows; that fixture was unrealistic vs run_up, which
    // records the start AFTER the OS creates the process.)
    let me = ProcessIdentity {
        pid: std::process::id(),
        started_unix: now_unix(),
    };
    assert!(supervisor_alive(&me));
}

// ---------------------------------------------------------------------------
// Windows asymmetric pid-reuse guard (pure fns — no real process / FFI)
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn filetime_to_unix_maps_known_instants() {
    // The 1601 FILETIME epoch is Unix time 0 (and saturates, never underflows).
    assert_eq!(filetime_to_unix(0), 0);
    // A hand-computed instant: Unix 1_000_000_000 s == (1e9 + 11_644_473_600) s
    // of 100ns ticks == 126_444_736_000_000_000 ticks.
    assert_eq!(filetime_to_unix(126_444_736_000_000_000), 1_000_000_000);
    // Sub-epoch tick counts saturate to 0 rather than wrapping.
    assert_eq!(filetime_to_unix(10_000_000), 0);
}

#[cfg(windows)]
#[test]
fn is_reused_pid_rejects_only_a_strictly_later_creation() {
    // Reused: the process behind the pid was created AFTER the recorded start
    // (beyond skew) — a different, later process holding the same pid.
    assert!(is_reused_pid(1_000, 1_010, 3), "created 10s later ⇒ reused");
    // Live, started before the recorded stamp (the common case: creation time
    // precedes the SystemTime::now() captured just after spawn).
    assert!(
        !is_reused_pid(1_000, 900, 3),
        "created before the recorded start ⇒ live, never reused"
    );
    // Live slow-start within skew (the H1/H2 regression guard): a creation
    // second one past the recorded start, absorbed by skew, must NOT reject —
    // a symmetric |Δ|<=TOL check would, and would false-kill a live supervisor.
    assert!(
        !is_reused_pid(1_000, 1_001, 3),
        "one second past, within skew ⇒ still live"
    );
    // Exactly at the skew boundary is still live (strict `>`).
    assert!(
        !is_reused_pid(1_000, 1_003, 3),
        "creation at recorded+skew is the boundary, not reuse"
    );
}

#[cfg(windows)]
#[test]
fn supervisor_alive_is_false_for_a_reused_pid_through_real_getprocesstimes() {
    // Real-FFI coverage of the PRIMARY reuse→dead branch. Every OTHER reuse test
    // drives the pure `is_reused_pid`, so a creation-time UNDER-READ inside the
    // FFI path — reading the zeroed `exited`/`kernel`/`user` out-param instead of
    // `created`, or a Hi/Lo swap in the reuse direction — would compute
    // `filetime_to_unix(0) = 0`, make `0 > recorded+5` false (not reused), and
    // still PASS every pure-fn test. This drives `supervisor_alive` to FALSE
    // through the REAL `GetProcessTimes` on a live pid: the current process is
    // alive, but `started_unix: 1` (1970) is impossibly early — a real
    // supervisor stamps `started_unix` AFTER its own OS creation — so real
    // `GetProcessTimes` returns a creation ~now, `is_reused_pid(1, ~now, 5)` is
    // true, and the reuse→dead branch fires through real FFI. If the FFI read
    // the wrong FILETIME field (creation under-read → 0), this assertion FAILS.
    let identity = ProcessIdentity {
        pid: std::process::id(),
        started_unix: 1,
    };
    assert!(
        !supervisor_alive(&identity),
        "own live pid + impossibly-early recorded start reads as a reused pid → dead"
    );
}

#[cfg(any(windows, unix))]
#[test]
fn classify_connects_for_a_live_slow_start_supervisor() {
    // H2 regression: model a live supervisor whose real OS creation precedes its
    // recorded `started_unix` — the common case, since `run_up` stamps the start
    // (here `now_unix()`) seconds AFTER the OS created the process. The
    // asymmetric Windows check must NOT report it reused (`actual_creation <
    // recorded`), so `supervisor_alive` stays true and classify yields Connect —
    // status/down never break on a live fleet.
    let mut state = sample_state(FleetStatus::Running, std::process::id());
    state.supervisor.started_unix = now_unix();
    let alive = supervisor_alive(&state.supervisor);
    assert!(alive, "a live supervisor must probe alive under the reuse check");
    assert_eq!(classify(&state, now_unix(), alive), Disposition::Connect);
}

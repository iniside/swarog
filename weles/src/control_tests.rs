//! Control-endpoint tests. The transport tests run a same-process client and
//! server over a REAL named pipe / UDS with a temp run dir (cfg'd to the two
//! supported targets; they compile on both). Every wait is a poll-with-deadline
//! loop — never a sleep-as-correctness (timing-tests doctrine). The frame
//! codec, identity classification, and liveness probe are platform-neutral
//! unit tests that pin the previously-wrong branches by construction.

use super::*;

use std::sync::atomic::AtomicU32;

use crate::state::ServiceState;

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
        services: vec![
            ServiceState {
                name: "accounts-svc".to_string(),
                status: Status::Healthy,
                pid: Some(1001),
                restarts: 0,
            },
            ServiceState {
                name: "gateway-svc".to_string(),
                status: Status::Backoff,
                pid: None,
                restarts: 2,
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
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::temp_dir().join(format!("weles-control-{tag}-{}", unique()));
        std::fs::create_dir_all(&dir).expect("create control test temp dir");
        TestEndpoint {
            path: dir.join("control.sock"),
            dir: Some(dir),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = tag;
        TestEndpoint {
            path: PathBuf::from("unsupported"),
            dir: None,
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

#[cfg(any(windows, target_os = "linux"))]
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
// Server thread shuts down cleanly (and promptly) once STOP is set
// ---------------------------------------------------------------------------

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn server_thread_joins_promptly_after_stop() {
    let endpoint = test_endpoint("shutdown");
    let state = Arc::new(Mutex::new(sample_state(
        FleetStatus::Running,
        std::process::id(),
    )));
    let stop = Arc::new(AtomicBool::new(false));
    let server =
        ControlServer::bind(endpoint.path.clone(), state, Arc::clone(&stop)).expect("bind");

    // Signal shutdown; dropping the server must join the serve thread bounded.
    stop.store(true, Ordering::SeqCst);
    let joined = Arc::new(AtomicBool::new(false));
    let joined_flag = Arc::clone(&joined);
    let joiner = std::thread::spawn(move || {
        drop(server);
        joined_flag.store(true, Ordering::SeqCst);
    });
    poll_until(
        Duration::from_secs(5),
        || joined.load(Ordering::SeqCst),
        "control server thread did not join within 5s of STOP",
    );
    joiner.join().expect("join the drop helper");
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
    let me = ProcessIdentity {
        pid: std::process::id(),
        started_unix: 1,
    };
    assert!(supervisor_alive(&me));
}

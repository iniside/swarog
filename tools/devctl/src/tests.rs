use super::cli::{parse, Command, Topology};
use super::control::{self, ControlServer};
use super::supervisor::{client_command, service_specs};
#[cfg(windows)]
use super::supervisor::wait_for_terminal;
use processctl::{
    observe_process_identity, EnvironmentSnapshot, FleetState, FleetStatus, ManagedProcess,
    ProcessIdentity, StartMarker, StateStore,
};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// The four supervised-child tests (transient cancellation/deadline, requested-stop
// reap, failed-shutdown cleanup state, missing-executable spawn+reap) and their
// managed-child entrypoint live in `tests/supervised.rs`: they drive processctl's
// production spawn path, which on unix re-execs `current_exe
// --__processctl-guardian-v1`, and only a `harness = false` target can dispatch
// that guardian before the tests run. See that file's module comment.

#[test]
fn up_defaults_to_monolith_and_switches_topology() {
    assert_eq!(
        parse(["up".into()]).unwrap(),
        Command::Up {
            topology: Topology::Monolith,
            skip_build: false
        }
    );
    assert_eq!(
        parse(["up".into(), "split".into(), "--skip-build".into()]).unwrap(),
        Command::Up {
            topology: Topology::Split,
            skip_build: true
        }
    );
}

#[test]
fn secret_capable_cli_environment_overrides_are_rejected() {
    let error = parse([
        "up".into(),
        "--env".into(),
        "DATABASE_URL=postgres://secret".into(),
    ])
    .unwrap_err()
    .to_string();
    assert!(!error.contains("secret"));
}

#[test]
fn microservices_alias_selects_split() {
    assert!(matches!(
        parse(["up".into(), "microservices".into()]).unwrap(),
        Command::Up {
            topology: Topology::Split,
            ..
        }
    ));
}

#[test]
fn client_commands_succeed_without_supervisor_state() {
    let directory = test_directory("inactive-missing");
    let store = StateStore::new(directory.join("state.json"));

    assert!(client_command(&store, "status").is_ok());
    assert!(client_command(&store, "down").is_ok());
    assert!(client_command(&store, "bogus")
        .unwrap_err()
        .to_string()
        .contains("unknown control command"));
}

#[test]
fn client_commands_do_not_contact_stopped_or_cleanly_failed_supervisors() {
    let directory = test_directory("inactive-clean-terminal");
    let store = StateStore::new(directory.join("state.json"));
    let unusable_supervisor = ProcessIdentity {
        pid: u32::MAX,
        executable: PathBuf::from("unusable-devctl-supervisor"),
        started: StartMarker(0),
    };

    let mut stopped = FleetState::new("stopped-test", "split").unwrap();
    stopped.set_status(FleetStatus::Stopped);
    stopped.set_supervisor(unusable_supervisor.clone());
    stopped.set_control_endpoint(Some(PathBuf::from("unusable-control-endpoint")));
    store.write_atomic(&stopped).unwrap();
    assert!(client_command(&store, "status").is_ok());
    assert!(client_command(&store, "down").is_ok());

    let mut failed = FleetState::new("failed-test", "split").unwrap();
    failed.set_status(FleetStatus::Failed);
    failed.record_failure("build", None::<String>).unwrap();
    failed.set_supervisor(unusable_supervisor);
    failed.set_control_endpoint(Some(PathBuf::from("unusable-control-endpoint")));
    store.write_atomic(&failed).unwrap();
    assert!(client_command(&store, "status").is_ok());
    assert!(client_command(&store, "down").is_ok());
}

#[test]
fn client_commands_surface_terminal_cleanup_uncertainty_without_contact() {
    let directory = test_directory("inactive-uncertain-terminal");
    let store = StateStore::new(directory.join("state.json"));
    let unusable_supervisor = ProcessIdentity {
        pid: u32::MAX,
        executable: PathBuf::from("unusable-devctl-supervisor"),
        started: StartMarker(0),
    };
    let failed_process = |label: &str| {
        let mut process = ManagedProcess::new(
            label,
            unusable_supervisor.clone(),
            directory.join(format!("{label}.out")),
            directory.join(format!("{label}.err")),
        )
        .unwrap();
        process.set_status(processctl::ManagedStatus::Failed);
        process
    };

    let mut cleanup_failed = FleetState::new("cleanup-failed-test", "split").unwrap();
    cleanup_failed.set_status(FleetStatus::Failed);
    cleanup_failed
        .record_failure("cleanup", Some("orphan-svc"))
        .unwrap();
    cleanup_failed.push_process(failed_process("orphan-svc"));
    cleanup_failed.set_supervisor(unusable_supervisor.clone());
    cleanup_failed.set_control_endpoint(Some(PathBuf::from("unusable-control-endpoint")));
    store.write_atomic(&cleanup_failed).unwrap();
    for command in ["status", "down"] {
        let error = client_command(&store, command).unwrap_err().to_string();
        assert!(error.contains("cleanup"));
        assert!(error.contains("orphan-svc"));
    }

    let mut checkpoint_failed = FleetState::new("checkpoint-failed-test", "split").unwrap();
    checkpoint_failed.set_status(FleetStatus::Failed);
    checkpoint_failed
        .record_failure("checkpoint-final", None::<String>)
        .unwrap();
    checkpoint_failed.set_supervisor(unusable_supervisor.clone());
    checkpoint_failed.set_control_endpoint(Some(PathBuf::from("unusable-control-endpoint")));
    store.write_atomic(&checkpoint_failed).unwrap();
    for command in ["status", "down"] {
        assert!(client_command(&store, command)
            .unwrap_err()
            .to_string()
            .contains("checkpoint-final"));
    }
}

#[test]
fn client_commands_require_control_data_for_nonterminal_state() {
    let directory = test_directory("active-missing-control");
    let store = StateStore::new(directory.join("state.json"));

    for status in [
        FleetStatus::Starting,
        FleetStatus::Running,
        FleetStatus::Stopping,
    ] {
        let mut state = FleetState::new("active-test", "monolith").unwrap();
        state.set_status(status);
        store.write_atomic(&state).unwrap();

        for command in ["status", "down"] {
            let error = client_command(&store, command).unwrap_err().to_string();
            assert!(error.contains("control endpoint"));
        }

        state.set_control_endpoint(Some(PathBuf::from("unusable-control-endpoint")));
        store.write_atomic(&state).unwrap();
        for command in ["status", "down"] {
            let error = client_command(&store, command).unwrap_err().to_string();
            assert!(error.contains("supervisor identity"));
        }
    }
}

#[test]
fn topology_specs_are_isolated_and_unknown_overrides_fail_closed() {
    let cert = PathBuf::from("run/test-ca.crt");
    let key = PathBuf::from("run/test-ca.key");
    let environment = EnvironmentSnapshot::from_values([
        ("HTTP_PROXY".into(), "http://proxy".into()),
        ("CARGO_HOME".into(), "cargo-home".into()),
        ("ACCOUNTS_DEV_AUTH".into(), "0".into()),
        ("PORT".into(), ":9999".into()),
    ]);
    let monolith = service_specs(
        Topology::Monolith,
        "postgres://typed",
        &cert,
        &key,
        &environment,
    );
    assert_eq!(monolith.len(), 1);
    assert_eq!(monolith[0].name, "monolith");
    assert!(!monolith[0].env.contains_key("HTTP_PROXY"));
    assert!(!monolith[0].env.contains_key("CARGO_HOME"));
    assert_eq!(
        monolith[0].env.get("PORT").map(String::as_str),
        Some(":8080")
    );
    assert_eq!(
        monolith[0].env.get("ACCOUNTS_DEV_AUTH").map(String::as_str),
        Some("0")
    );

    let split = service_specs(
        Topology::Split,
        "postgres://typed",
        &cert,
        &key,
        &environment,
    );
    assert_eq!(split.len(), 12);
    assert!(split
        .iter()
        .all(|service| !service.env.contains_key("HTTP_PROXY")));

    assert_eq!(
        split
            .iter()
            .find(|s| s.name == "accounts-svc")
            .unwrap()
            .env
            .get("ACCOUNTS_DEV_AUTH")
            .map(String::as_str),
        Some("0")
    );
}

#[cfg(unix)]
#[test]
fn unix_control_round_trips_and_rejects_reused_pid() {
    // A DIRECT filesystem UDS test (Postgres-free, no fleet): bind + connect +
    // peer-cred check. The socket path must stay under darwin's 104-byte
    // `sun_path`, so it lives directly in temp_dir under a short name rather than
    // in a nested per-test directory.
    let endpoint = std::env::temp_dir().join(format!("dc{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&endpoint);
    let identity = observe_process_identity(std::process::id()).unwrap();
    let mut fleet = FleetState::new("control-unix", "monolith").unwrap();
    fleet.set_supervisor(identity.clone());
    fleet.set_control_endpoint(Some(endpoint.clone()));
    let state = Arc::new(Mutex::new(fleet));
    let stop = Arc::new(AtomicBool::new(false));
    let server = ControlServer::bind(endpoint.clone(), state, Arc::clone(&stop)).unwrap();

    // Accept path: our own pid+uid round-trips a status.
    let status = retry_control_unix(&endpoint, "status", &identity).unwrap();
    assert!(status.starts_with("monolith"));

    // Reject path: the anti-reused-pid guard. A DIFFERENT but live process passes
    // the observe_process_identity() precheck (its identity is genuine), yet the
    // connecting peer is us, so the peer pid (LOCAL_PEERPID on darwin) will not
    // match `expected.pid`. A Linux->darwin port that dropped the second
    // getsockopt — reading only the pid-less `xucred` — would silently ACCEPT
    // this reused-pid impostor; this assertion pins that it is refused.
    let mut impostor = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn impostor process");
    let impostor_identity = observe_process_identity(impostor.id()).unwrap();
    assert_ne!(impostor_identity.pid, std::process::id());
    let error = control::request(&endpoint, "status", &impostor_identity)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not the recorded supervisor"),
        "expected peer-cred rejection, got: {error}"
    );

    assert_eq!(
        retry_control_unix(&endpoint, "down", &identity).unwrap(),
        "shutdown requested"
    );
    drop(server);
    let _ = impostor.kill();
    let _ = impostor.wait();
    let _ = std::fs::remove_file(&endpoint);
}

#[cfg(unix)]
fn retry_control_unix(
    endpoint: &std::path::Path,
    command: &str,
    identity: &processctl::ProcessIdentity,
) -> anyhow::Result<String> {
    let mut last = None;
    for _ in 0..50 {
        match control::request(endpoint, command, identity) {
            Ok(response) => return Ok(response),
            Err(error) => last = Some(error),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(last.expect("at least one attempt"))
}

#[cfg(windows)]
#[test]
fn owner_only_control_pipe_round_trips_and_rejects_wrong_supervisor() {
    let endpoint = PathBuf::from(format!(
        r"\\.\pipe\gamebackend-devctl-test-{}",
        std::process::id()
    ));
    let identity = observe_process_identity(std::process::id()).unwrap();
    let mut fleet = FleetState::new("control-test", "monolith").unwrap();
    fleet.set_supervisor(identity.clone());
    fleet.set_control_endpoint(Some(endpoint.clone()));
    let state = Arc::new(Mutex::new(fleet));
    let stop = Arc::new(AtomicBool::new(false));
    let server = ControlServer::bind(endpoint.clone(), state, Arc::clone(&stop)).unwrap();

    let status = retry_control(&endpoint, "status", &identity).unwrap();
    assert!(status.starts_with("monolith starting"));

    let mut wrong = identity.clone();
    wrong.pid = wrong.pid.saturating_add(1);
    assert!(control::request(&endpoint, "status", &wrong).is_err());

    assert_eq!(
        retry_control(&endpoint, "down", &identity).unwrap(),
        "shutdown requested"
    );
    drop(server);
}

#[cfg(windows)]
#[test]
fn partial_control_frame_cannot_hang_server_drop() {
    use std::io::Write as _;
    let (endpoint, _identity, state, stop) = control_fixture("partial");
    let server = ControlServer::bind(endpoint.clone(), state, stop).unwrap();
    let mut client = open_pipe(&endpoint);
    client.write_all(&[0, 0]).unwrap();
    let started = std::time::Instant::now();
    drop(server);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(windows)]
#[test]
fn control_bind_is_ready_and_duplicate_bind_fails() {
    let (endpoint, identity, state, stop) = control_fixture("bind");
    let server =
        ControlServer::bind(endpoint.clone(), Arc::clone(&state), Arc::clone(&stop)).unwrap();
    assert!(control::request(&endpoint, "status", &identity).is_ok());
    let duplicate = ControlServer::bind(endpoint.clone(), state, stop);
    assert!(duplicate.is_err());
    drop(server);
}

#[cfg(windows)]
#[test]
fn concurrent_control_clients_retry_pipe_busy() {
    let (endpoint, identity, state, stop) = control_fixture("concurrent");
    let server = ControlServer::bind(endpoint.clone(), state, stop).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let endpoint = endpoint.clone();
            let identity = identity.clone();
            std::thread::spawn(move || control::request(&endpoint, "status", &identity))
        })
        .collect();
    for thread in threads {
        assert!(thread.join().unwrap().is_ok());
    }
    drop(server);
}

#[cfg(windows)]
#[test]
fn down_waits_for_stopped_checkpoint_and_reports_failed_cleanup() {
    let directory = std::env::temp_dir().join(format!(
        "devctl-down-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let store = StateStore::new(directory.join("state.json"));
    let supervisor = observe_process_identity(std::process::id()).unwrap();
    let mut state = FleetState::new("down-test", "monolith").unwrap();
    state.set_supervisor(supervisor.clone());
    state.set_status(FleetStatus::Stopping);
    store.write_atomic(&state).unwrap();

    let writer_store = store.clone();
    let mut stopped = state.clone();
    stopped.set_status(FleetStatus::Stopped);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(75));
        writer_store.write_atomic(&stopped).unwrap();
    });
    // Hang-guard, not a latency bound: the writer thread's 75ms sleep + file
    // write can be starved well past 1s under a full-workspace parallel run.
    assert!(wait_for_terminal(&store, &supervisor, Duration::from_secs(15)).is_ok());
    writer.join().unwrap();

    state.set_status(FleetStatus::Failed);
    store.write_atomic(&state).unwrap();
    // The Failed state is already on disk; the budget only guards against a
    // starved file read misreporting as a timeout instead of "shutdown failed".
    assert!(
        wait_for_terminal(&store, &supervisor, Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("shutdown failed")
    );
}

fn test_directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "devctl-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[cfg(windows)]
fn control_fixture(
    name: &str,
) -> (
    PathBuf,
    processctl::ProcessIdentity,
    Arc<Mutex<FleetState>>,
    Arc<AtomicBool>,
) {
    let endpoint = PathBuf::from(format!(
        r"\\.\pipe\gamebackend-devctl-test-{name}-{}",
        std::process::id()
    ));
    let identity = observe_process_identity(std::process::id()).unwrap();
    let mut fleet = FleetState::new(format!("control-{name}"), "monolith").unwrap();
    fleet.set_supervisor(identity.clone());
    fleet.set_control_endpoint(Some(endpoint.clone()));
    (
        endpoint,
        identity,
        Arc::new(Mutex::new(fleet)),
        Arc::new(AtomicBool::new(false)),
    )
}

#[cfg(windows)]
fn open_pipe(endpoint: &std::path::Path) -> std::fs::File {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    let name: Vec<u16> = endpoint.as_os_str().encode_wide().chain(Some(0)).collect();
    loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return unsafe { std::fs::File::from_raw_handle(handle.cast()) };
        }
        unsafe { WaitNamedPipeW(name.as_ptr(), 20) };
    }
}

#[cfg(windows)]
fn retry_control(
    endpoint: &std::path::Path,
    command: &str,
    identity: &processctl::ProcessIdentity,
) -> anyhow::Result<String> {
    let mut last = None;
    for _ in 0..50 {
        match control::request(endpoint, command, identity) {
            Ok(response) => return Ok(response),
            Err(error) => last = Some(error),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(last.expect("at least one attempt"))
}

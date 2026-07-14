use super::cli::{parse, Command, Topology};
use super::control::{self, ControlServer};
use super::supervisor::{
    client_command, run_transient, service_specs, spawn_managed, teardown, teardown_with,
    wait_for_terminal, wait_healthy, StepOutcome, TransientOutcome,
};
use processctl::{
    observe_process_identity, EnvironmentSnapshot, FleetState, FleetStatus, ManagedProcess,
    OutputDestination, OwnedChild, PoolBudget, ProcessGroupPolicy, ProcessIdentity, ServiceSpec, SpawnSpec,
    StartMarker, StateStore, ShutdownPolicy, WorkspaceLayout,
};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[test]
fn transient_child_entry() {
    if std::env::var_os("DEVCTL_TRANSIENT_CHILD").is_some() {
        if let Some(path) = std::env::var_os("DEVCTL_TRANSIENT_READY") {
            std::fs::write(path, std::process::id().to_string()).unwrap();
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn transient_children_obey_cancellation_and_deadline() {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    let directory = test_directory("transient");
    let spec = |ready: &std::path::Path| SpawnSpec {
        label: "devctl-transient-test".into(),
        executable: std::env::current_exe().unwrap(),
        args: ["--exact", "tests::transient_child_entry", "--nocapture"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        env: BTreeMap::from([
            (
                OsString::from("DEVCTL_TRANSIENT_CHILD"),
                OsString::from("1"),
            ),
            (
                OsString::from("DEVCTL_TRANSIENT_READY"),
                ready.as_os_str().to_owned(),
            ),
        ]),
        cwd: std::env::current_dir().unwrap(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    };

    let cancelled = Arc::new(AtomicBool::new(false));
    let ready = directory.join("cancel.ready");
    let setter = Arc::clone(&cancelled);
    let setter_ready = ready.clone();
    let trigger = std::thread::spawn(move || {
        while !setter_ready.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        setter.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    assert_eq!(
        run_transient(spec(&ready), None, &cancelled, Duration::from_secs(5)).unwrap(),
        TransientOutcome::Cancelled
    );
    trigger.join().unwrap();
    // Hang-guard, not a latency bound: the `Cancelled` outcome already proves
    // correctness; this only catches a genuine wedge (real OS spawn+signal+reap timing
    // is not something the test should pin under load).
    assert!(started.elapsed() < Duration::from_secs(30));
    let pid: u32 = std::fs::read_to_string(&ready).unwrap().parse().unwrap();
    assert!(observe_process_identity(pid).is_err());

    let store = StateStore::new(directory.join("build-state.json"));
    let state = Arc::new(Mutex::new(
        FleetState::new("build-stop", "monolith").unwrap(),
    ));
    teardown(&store, &state, &mut [], false).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().status(),
        FleetStatus::Stopped
    );

    let running = AtomicBool::new(false);
    assert!(run_transient(
        spec(&directory.join("timeout.ready")),
        None,
        &running,
        Duration::from_millis(50),
    )
    .unwrap_err()
    .to_string()
    .contains("timed out"));
}

#[test]
fn requested_stop_during_health_finishes_stopped_and_reaps_child() {
    let directory = test_directory("health-stop");
    let ready = directory.join("child.ready");
    let child = OwnedChild::spawn(fake_child_spec(&ready)).unwrap();
    wait_for_file(&ready);
    let identity = child.identity().clone();
    let service = fake_service("health-svc", "unused", 65_000);
    let mut fleet = FleetState::new("health-stop", "split").unwrap();
    fleet.push_process(
        ManagedProcess::new(
            service.name,
            identity.clone(),
            directory.join("health.out"),
            directory.join("health.err"),
        )
        .unwrap(),
    );
    let state = Arc::new(Mutex::new(fleet));
    let store = StateStore::new(directory.join("state.json"));
    store.write_atomic(&state.lock().unwrap()).unwrap();
    let mut children = vec![child];
    let stop = AtomicBool::new(true);
    assert_eq!(
        wait_healthy(&service, &mut children[0], &stop).unwrap(),
        StepOutcome::RequestedStop
    );
    teardown(&store, &state, &mut children, false).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().status(),
        FleetStatus::Stopped
    );
    assert!(observe_process_identity(identity.pid).is_err());
}

#[test]
fn failed_child_shutdown_persists_failed_cleanup_state() {
    let directory = test_directory("cleanup-failure-transition");
    let ready = directory.join("child.ready");
    let child = OwnedChild::spawn(fake_child_spec(&ready)).unwrap();
    wait_for_file(&ready);
    let identity = child.identity().clone();
    let mut fleet = FleetState::new("cleanup-failure", "monolith").unwrap();
    fleet.push_process(
        ManagedProcess::new(
            "orphan-svc",
            identity.clone(),
            directory.join("orphan.out"),
            directory.join("orphan.err"),
        )
        .unwrap(),
    );
    let state = Arc::new(Mutex::new(fleet));
    let store = StateStore::new(directory.join("state.json"));
    store.write_atomic(&state.lock().unwrap()).unwrap();
    let mut children = vec![child];

    let error = teardown_with(&store, &state, &mut children, false, |_| {
        anyhow::bail!("injected shutdown failure")
    })
    .unwrap_err();
    assert!(error.to_string().contains("orphan-svc"));
    assert!(error.to_string().contains("injected shutdown failure"));

    let terminal = store.load().unwrap().unwrap();
    assert_eq!(terminal.status(), FleetStatus::Failed);
    assert!(matches!(
        terminal.processes()[0].status(),
        processctl::ManagedStatus::Failed
    ));
    assert_eq!(terminal.failure().unwrap().stage(), "cleanup");
    assert_eq!(terminal.failure().unwrap().process(), Some("orphan-svc"));

    children[0]
        .shutdown(ShutdownPolicy {
            graceful_timeout: Duration::from_millis(100),
            force_timeout: Duration::from_secs(1),
        })
        .unwrap();
    assert!(observe_process_identity(identity.pid).is_err());
}

#[test]
fn missing_service_executable_records_spawn_and_reaps_prefix() {
    let directory = test_directory("spawn-failure");
    let ready = directory.join("prefix.ready");
    let prefix = OwnedChild::spawn(fake_child_spec(&ready)).unwrap();
    wait_for_file(&ready);
    let prefix_identity = prefix.identity().clone();
    let prefix_process = ManagedProcess::new(
        "prefix-svc",
        prefix_identity.clone(),
        directory.join("prefix.out"),
        directory.join("prefix.err"),
    )
    .unwrap();
    let mut fleet = FleetState::new("spawn-failure", "split").unwrap();
    fleet.push_process(prefix_process);
    let state = Arc::new(Mutex::new(fleet));
    let store = StateStore::new(directory.join("state.json"));
    store.write_atomic(&state.lock().unwrap()).unwrap();
    let mut children = vec![prefix];
    let missing = fake_service("missing-svc", "definitely-missing", 65_001);

    let layout = WorkspaceLayout::from_root(directory.clone(), &std::collections::BTreeMap::new());
    let primary = spawn_managed(
        &layout,
        &directory,
        &missing,
        &store,
        &state,
        &mut children,
    )
    .unwrap_err();
    assert!(primary.to_string().contains("start missing-svc"));
    let checkpointed = store.load().unwrap().unwrap();
    assert_eq!(checkpointed.failure().unwrap().stage(), "spawn");
    assert_eq!(
        checkpointed.failure().unwrap().process(),
        Some("missing-svc")
    );

    teardown(&store, &state, &mut children, true).unwrap();
    let terminal = store.load().unwrap().unwrap();
    assert_eq!(terminal.status(), FleetStatus::Failed);
    assert_eq!(terminal.failure().unwrap().stage(), "spawn");
    assert_eq!(terminal.failure().unwrap().process(), Some("missing-svc"));
    assert!(observe_process_identity(prefix_identity.pid).is_err());
}

fn fake_child_spec(ready: &std::path::Path) -> SpawnSpec {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    SpawnSpec {
        label: "devctl-fake-child".into(),
        executable: std::env::current_exe().unwrap(),
        args: ["--exact", "tests::transient_child_entry", "--nocapture"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        env: BTreeMap::from([
            (
                OsString::from("DEVCTL_TRANSIENT_CHILD"),
                OsString::from("1"),
            ),
            (
                OsString::from("DEVCTL_TRANSIENT_READY"),
                ready.as_os_str().to_owned(),
            ),
        ]),
        cwd: std::env::current_dir().unwrap(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

fn fake_service(
    name: &'static str,
    executable_package: &'static str,
    http_port: u16,
) -> ServiceSpec {
    ServiceSpec {
        name,
        executable_package,
        http_port,
        edge_port: None,
        player_port: None,
        dependencies: vec![],
        env: Default::default(),
        overrideable_env: &[],
        pool_budget: PoolBudget { pool_max: 0, dedicated: 0 },
    }
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "fake child did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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

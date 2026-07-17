//! Supervised-child tests for devctl's supervisor. These exercise processctl's
//! PRODUCTION spawn path (`OwnedChild::spawn`, `run_transient`, `spawn_managed`,
//! `teardown`), which on unix re-execs `current_exe --__processctl-guardian-v1`.
//! A libtest unit binary has no early guardian hook, so that re-exec lands on the
//! test harness and exits 101 — the exact cross-unix gap these four hit on any
//! unix (they only ever passed under Windows Job Objects, which don't re-exec).
//!
//! This is a `harness = false` target (see `tools/devctl/Cargo.toml`): its `main`
//! owns the entrypoint, so it can call `dispatch_guardian_from_current_exe()`
//! FIRST — exactly as the production `main.rs` and the processctl downstream /
//! lease_marker fixtures do — before running any test logic. The four tests'
//! assertions are byte-identical to their previous libtest form; only their
//! location and this guardian-dispatch bootstrap changed. They are Postgres-free.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use devctl::supervisor::{
    run_transient, spawn_managed, teardown, teardown_with, wait_healthy, StepOutcome,
    TransientOutcome,
};
use processctl::{
    observe_process_identity, FleetState, FleetStatus, ManagedProcess, OutputDestination,
    OwnedChild, PoolBudget, ProcessGroupPolicy, ServiceSpec, ShutdownPolicy, SpawnSpec, StateStore,
    WorkspaceLayout,
};

fn main() -> ExitCode {
    // Production ordering: the guardian re-exec must be caught before anything
    // else runs. Without this, the four tests below could never spawn a managed
    // child on unix.
    if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
        return code;
    }

    // Managed-child role: when a test spawns `current_exe` as a supervised child,
    // the guardian execs into this same binary with `DEVCTL_TRANSIENT_CHILD` set.
    // (Previously a `#[test] transient_child_entry` selected by a libtest
    // `--exact` filter; with no harness here, the env var alone selects the role.)
    if std::env::var_os("DEVCTL_TRANSIENT_CHILD").is_some() {
        if let Some(path) = std::env::var_os("DEVCTL_TRANSIENT_READY") {
            std::fs::write(path, std::process::id().to_string()).unwrap();
        }
        std::thread::sleep(Duration::from_secs(60));
        return ExitCode::SUCCESS;
    }

    let cases: &[(&str, fn())] = &[
        (
            "transient_children_obey_cancellation_and_deadline",
            transient_children_obey_cancellation_and_deadline,
        ),
        (
            "requested_stop_during_health_finishes_stopped_and_reaps_child",
            requested_stop_during_health_finishes_stopped_and_reaps_child,
        ),
        (
            "failed_child_shutdown_persists_failed_cleanup_state",
            failed_child_shutdown_persists_failed_cleanup_state,
        ),
        (
            "missing_service_executable_records_spawn_and_reaps_prefix",
            missing_service_executable_records_spawn_and_reaps_prefix,
        ),
    ];

    let mut passed = 0usize;
    let mut ok = true;
    for (name, case) in cases {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(case)) {
            Ok(()) => {
                println!("test {name} ... ok");
                passed += 1;
            }
            Err(_) => {
                eprintln!("test {name} ... FAILED");
                ok = false;
            }
        }
    }
    println!(
        "supervised: {passed}/{} passed",
        cases.len()
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn transient_children_obey_cancellation_and_deadline() {
    let directory = test_directory("transient");
    let spec = |ready: &std::path::Path| SpawnSpec {
        label: "devctl-transient-test".into(),
        executable: std::env::current_exe().unwrap(),
        args: Vec::new(),
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
    SpawnSpec {
        label: "devctl-fake-child".into(),
        executable: std::env::current_exe().unwrap(),
        args: Vec::new(),
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

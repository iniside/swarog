use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use processctl::{
    game_backend_fleet_with_environment, game_backend_monolith, observe_process_identity,
    rollout_lock_path, EnvironmentSnapshot, FleetFlavor, FleetInputs, FleetState, FleetStatus,
    ManagedProcess, ManagedStatus, OutputDestination, OwnedChild, ProcessGroupPolicy, RolloutLock,
    ServiceSpec, ShutdownPolicy, SpawnSpec, StateStore, WorkspaceLayout,
};
use rand::RngCore as _;

use crate::cli::{Command, Topology, USAGE};
use crate::control::{self, ControlServer};

const DEFAULT_DB: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const DOWN_TIMEOUT: Duration = Duration::from_secs(130);
const BUILD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ADMIN_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN: ShutdownPolicy = ShutdownPolicy {
    graceful_timeout: Duration::from_secs(5),
    force_timeout: Duration::from_secs(5),
};
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepOutcome {
    Completed,
    RequestedStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransientOutcome {
    Completed,
    Cancelled,
}

pub fn execute(command: Command) -> Result<()> {
    let root = workspace_root()?;
    let run_dir = root.join("run/devctl");
    let store = StateStore::new(run_dir.join("state.json"));
    match command {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Status => client_command(&store, "status"),
        Command::Down => client_command(&store, "down"),
        Command::Up {
            topology,
            skip_build,
        } => {
            std::fs::create_dir_all(&run_dir).context("create devctl run directory")?;
            supervise(&root, &run_dir, &store, topology, skip_build)
        }
    }
}

pub(crate) fn client_command(store: &StateStore, command: &str) -> Result<()> {
    if !matches!(command, "status" | "down") {
        bail!("unknown control command {command:?}");
    }
    let Some(state) = store.load()? else {
        println!("devctl: inactive (no supervisor state)");
        return Ok(());
    };
    match state.status() {
        FleetStatus::Stopped => {
            println!("devctl: inactive (last {} stopped)", state.topology());
            return Ok(());
        }
        FleetStatus::Failed => {
            let failure = state
                .failure()
                .context("failed devctl state has no failure record")?;
            let unreaped: Vec<_> = state
                .processes()
                .iter()
                .filter(|process| !matches!(process.status(), ManagedStatus::Exited { .. }))
                .map(|process| process.label())
                .collect();
            let cleanup_uncertain = failure.stage() == "cleanup"
                || failure.stage().starts_with("checkpoint")
                || !unreaped.is_empty();
            if cleanup_uncertain {
                bail!(
                    "devctl: last {} run failed at {}; cleanup is not proven; unreaped entries: {unreaped:?}",
                    state.topology(),
                    failure.stage()
                );
            }
            if let Some(process) = failure.process() {
                println!(
                    "devctl: inactive (last {} failed at {} for {process})",
                    state.topology(),
                    failure.stage()
                );
            } else {
                println!(
                    "devctl: inactive (last {} failed at {})",
                    state.topology(),
                    failure.stage()
                );
            }
            return Ok(());
        }
        FleetStatus::Starting | FleetStatus::Running | FleetStatus::Stopping => {}
    }
    let endpoint = state
        .control_endpoint()
        .context("state has no control endpoint")?;
    let supervisor = state
        .supervisor()
        .context("state has no supervisor identity")?;
    let message = control::request(endpoint, command, supervisor)?;
    if command != "down" {
        println!("{message}");
        return Ok(());
    }
    wait_for_terminal(store, supervisor, DOWN_TIMEOUT)
}

pub(crate) fn wait_for_terminal(
    store: &StateStore,
    supervisor: &processctl::ProcessIdentity,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = store
            .load()?
            .context("devctl state disappeared during shutdown")?;
        match state.status() {
            FleetStatus::Stopped => {
                println!(
                    "{} stopped ({} processes reaped)",
                    state.topology(),
                    state.processes().len()
                );
                return Ok(());
            }
            FleetStatus::Failed => {
                let failed: Vec<_> = state
                    .processes()
                    .iter()
                    .filter(|process| matches!(process.status(), ManagedStatus::Failed))
                    .map(|process| process.label())
                    .collect();
                bail!(
                    "{} shutdown failed; failed cleanup entries: {failed:?}",
                    state.topology()
                );
            }
            _ => {}
        }
        if observe_process_identity(supervisor.pid).ok().as_ref() != Some(supervisor) {
            bail!("supervisor exited before publishing a terminal cleanup state");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting {timeout:?} for shutdown cleanup");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn supervise(
    root: &Path,
    run_dir: &Path,
    store: &StateStore,
    topology: Topology,
    skip_build: bool,
) -> Result<()> {
    let environment = EnvironmentSnapshot::capture();
    // One authority for artifact lookup, built from the SAME frozen build env the
    // build step spawns cargo with (honors CARGO_TARGET_DIR). cwd stays `root`.
    let layout = WorkspaceLayout::from_root(root.to_path_buf(), &environment.build_environment());
    install_signal_handler()?;
    INTERRUPTED.store(false, Ordering::SeqCst);
    let run_id = run_id();
    let _lease = RolloutLock::acquire_exclusive(rollout_lock_path(root), &run_id)
        .context("acquire rollout lock")?;
    let db_url = environment
        .value("DATABASE_URL")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| DEFAULT_DB.to_string());
    let ca_cert = run_dir.join("edge-ca.crt");
    let ca_key = run_dir.join("edge-ca.key");
    let services = service_specs(topology, &db_url, &ca_cert, &ca_key, &environment);
    let endpoint = control_endpoint(run_dir, &run_id);
    let mut initial = FleetState::new(&run_id, topology.name())?;
    initial.set_supervisor(observe_process_identity(std::process::id())?);
    initial.set_control_endpoint(Some(endpoint.clone()));
    let state = Arc::new(Mutex::new(initial));
    let stop = Arc::new(AtomicBool::new(false));
    let _control = ControlServer::bind(endpoint, Arc::clone(&state), Arc::clone(&stop))?;
    checkpoint(store, &state, &mut [], "supervisor", None)?;

    let mut children = Vec::new();
    let result = (|| -> Result<()> {
        if !skip_build {
            match build(root, topology, &services, &environment, &stop) {
                Ok(StepOutcome::Completed) => {}
                Ok(StepOutcome::RequestedStop) => return Ok(()),
                Err(error) => {
                    state
                        .lock()
                        .expect("state mutex poisoned")
                        .record_failure("build", None::<String>)?;
                    return Err(error);
                }
            }
        }
        if stop_requested(&stop) {
            return Ok(());
        }
        if let Err(error) = edgeca::mint_dev_ca(&ca_cert, &ca_key) {
            state
                .lock()
                .expect("state mutex poisoned")
                .record_failure("edge-ca", None::<String>)?;
            return Err(anyhow::anyhow!(error.to_string()));
        }
        match seed_admin(&layout, &db_url, &environment, &stop) {
            Ok(StepOutcome::Completed) => {}
            Ok(StepOutcome::RequestedStop) => return Ok(()),
            Err(error) => {
                state
                    .lock()
                    .expect("state mutex poisoned")
                    .record_failure("admin-seed", None::<String>)?;
                return Err(error);
            }
        }
        for service in &services {
            if stop_requested(&stop) {
                return Ok(());
            }
            println!(
                "devctl: starting {} on :{}",
                service.name, service.http_port
            );
            spawn_managed(&layout, run_dir, service, store, &state, &mut children)?;
            match wait_healthy(service, children.last_mut().expect("just pushed"), &stop) {
                Ok(StepOutcome::Completed) => {}
                Ok(StepOutcome::RequestedStop) => return Ok(()),
                Err(error) => {
                    {
                        let mut state = state.lock().expect("state mutex poisoned");
                        state
                            .processes_mut()
                            .last_mut()
                            .expect("matching process state")
                            .set_status(ManagedStatus::Failed);
                        state.record_failure("health", Some(service.name))?;
                    }
                    if let Err(checkpoint_error) = checkpoint(
                        store,
                        &state,
                        &mut children,
                        "health-failure",
                        Some(service.name),
                    ) {
                        bail!(
                            "{error:#}; failed-state checkpoint also failed: {checkpoint_error:#}"
                        );
                    }
                    return Err(error);
                }
            }
            state
                .lock()
                .expect("state mutex poisoned")
                .processes_mut()
                .last_mut()
                .expect("matching process state")
                .set_status(ManagedStatus::Healthy);
            checkpoint(store, &state, &mut children, "healthy", Some(service.name))?;
            println!("devctl: {} healthy", service.name);
        }
        state
            .lock()
            .expect("state mutex poisoned")
            .set_status(FleetStatus::Running);
        checkpoint(store, &state, &mut children, "running", None)?;
        println!(
            "devctl: {} running; press Ctrl-C or run `devctl down`",
            topology.name()
        );
        if let Some((index, status)) = monitor(&mut children, &stop)? {
            let label = services[index].name;
            {
                let mut state = state.lock().expect("state mutex poisoned");
                state.processes_mut()[index].set_status(ManagedStatus::Exited {
                    code: status.code(),
                });
                state.record_failure("unexpected-exit", Some(label))?;
            }
            if let Err(checkpoint_error) =
                checkpoint(store, &state, &mut children, "unexpected-exit", Some(label))
            {
                bail!("{label} exited unexpectedly with {status}; failed-state checkpoint also failed: {checkpoint_error:#}");
            }
            bail!("{label} exited unexpectedly with {status}");
        }
        Ok(())
    })();

    let primary = result.err();
    let cleanup = teardown(store, &state, &mut children, primary.is_some()).err();
    match (primary, cleanup) {
        (None, None) => Ok(()),
        (Some(primary), None) => Err(primary),
        (None, Some(cleanup)) => Err(cleanup),
        (Some(primary), Some(cleanup)) => {
            bail!("primary failure: {primary:#}; cleanup also failed: {cleanup:#}")
        }
    }
}

pub(crate) fn service_specs(
    topology: Topology,
    db: &str,
    cert: &Path,
    key: &Path,
    environment: &EnvironmentSnapshot,
) -> Vec<ServiceSpec> {
    let inputs = FleetInputs {
        database_url: db.into(),
        edge_ca_cert: cert.into(),
        edge_ca_key: key.into(),
    };
    match topology {
        Topology::Split => {
            game_backend_fleet_with_environment(&inputs, FleetFlavor::Development, environment)
                .services()
                .to_vec()
        }
        Topology::Monolith => vec![game_backend_monolith(
            &inputs,
            FleetFlavor::Development,
            environment,
        )],
    }
}

fn build(
    root: &Path,
    topology: Topology,
    services: &[ServiceSpec],
    environment: &EnvironmentSnapshot,
    stop: &AtomicBool,
) -> Result<StepOutcome> {
    let mut packages: Vec<&str> = services
        .iter()
        .map(|service| service.executable_package)
        .collect();
    packages.extend(["adminctl", "playercli", "csharp-client-gen"]);
    packages.sort_unstable();
    packages.dedup();
    let mut args = vec![OsString::from("build")];
    for package in packages {
        args.extend([OsString::from("-p"), OsString::from(package)]);
    }
    println!("devctl: building {} topology", topology.name());
    let build_env = environment.build_environment();
    let cargo = executable_on_path("cargo", &build_env)?;
    match run_transient(
        SpawnSpec {
            label: "devctl-cargo-build".into(),
            executable: cargo,
            args,
            env: os_env(build_env),
            cwd: root.into(),
            stdout: OutputDestination::Inherit,
            stderr: OutputDestination::Inherit,
            process_group: ProcessGroupPolicy::Owned,
        },
        None,
        stop,
        BUILD_TIMEOUT,
    )
    .context("cargo build")?
    {
        TransientOutcome::Completed => Ok(StepOutcome::Completed),
        TransientOutcome::Cancelled => Ok(StepOutcome::RequestedStop),
    }
}

fn seed_admin(
    layout: &WorkspaceLayout,
    db: &str,
    environment: &EnvironmentSnapshot,
    stop: &AtomicBool,
) -> Result<StepOutcome> {
    let mut env = environment.runtime_environment();
    env.insert("DATABASE_URL".into(), db.into());
    let spec = SpawnSpec {
        label: "devctl-admin-seed".into(),
        executable: layout.binary("debug", "adminctl"),
        args: ["create-user", "admin", "--password-stdin"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        env: os_env(env),
        cwd: layout.root.clone(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    };
    match run_transient(spec, Some(b"admin\n"), stop, ADMIN_TIMEOUT)
        .context("seed development admin user")?
    {
        TransientOutcome::Completed => Ok(StepOutcome::Completed),
        TransientOutcome::Cancelled => Ok(StepOutcome::RequestedStop),
    }
}

pub(crate) fn run_transient(
    spec: SpawnSpec,
    stdin: Option<&[u8]>,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<TransientOutcome> {
    let mut child = match stdin {
        Some(bytes) => OwnedChild::spawn_with_stdin_bytes(spec, bytes)?,
        None => OwnedChild::spawn(spec)?,
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(TransientOutcome::Completed);
            }
            bail!("child exited with {status}");
        }
        let interrupted = stop_requested(stop);
        let timed_out = Instant::now() >= deadline;
        if interrupted || timed_out {
            let cleanup = child
                .shutdown(SHUTDOWN)
                .err()
                .map(|error| error.to_string());
            if let Some(cleanup) = cleanup {
                let reason = if interrupted {
                    "cancelled"
                } else {
                    "timed out"
                };
                bail!("transient child {reason}; cleanup also failed: {cleanup}");
            }
            if interrupted {
                return Ok(TransientOutcome::Cancelled);
            }
            bail!("transient child timed out after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn spawn_managed(
    layout: &WorkspaceLayout,
    run_dir: &Path,
    service: &ServiceSpec,
    store: &StateStore,
    state: &Arc<Mutex<FleetState>>,
    children: &mut Vec<OwnedChild>,
) -> Result<()> {
    let mut child = match OwnedChild::spawn(spawn_spec(layout, run_dir, service)) {
        Ok(child) => child,
        Err(error) => {
            state
                .lock()
                .expect("state mutex poisoned")
                .record_failure("spawn", Some(service.name))?;
            if let Err(checkpoint_error) =
                checkpoint(store, state, children, "spawn-failure", Some(service.name))
            {
                bail!(
                    "start {}: {error}; cleanup checkpoint also failed: {checkpoint_error:#}",
                    service.name
                );
            }
            return Err(error).with_context(|| format!("start {}", service.name));
        }
    };
    let process = match ManagedProcess::new(
        service.name,
        child.identity().clone(),
        run_dir.join(format!("{}.out.log", service.name)),
        run_dir.join(format!("{}.err.log", service.name)),
    ) {
        Ok(process) => process,
        Err(error) => {
            state
                .lock()
                .expect("state mutex poisoned")
                .record_failure("spawn", Some(service.name))?;
            let cleanup = child
                .shutdown(SHUTDOWN)
                .err()
                .map(|error| error.to_string());
            let checkpoint_error =
                checkpoint(store, state, children, "spawn-failure", Some(service.name)).err();
            match (cleanup, checkpoint_error) {
                (None, None) => return Err(error.into()),
                (cleanup, checkpoint) => bail!(
                    "create managed state for {}: {error}; cleanup failures: child={cleanup:?}, checkpoint={checkpoint:?}",
                    service.name
                ),
            }
        }
    };
    children.push(child);
    state
        .lock()
        .expect("state mutex poisoned")
        .push_process(process);
    checkpoint(store, state, children, "spawn", Some(service.name))
}

fn spawn_spec(layout: &WorkspaceLayout, run_dir: &Path, service: &ServiceSpec) -> SpawnSpec {
    SpawnSpec {
        label: service.name.into(),
        executable: layout.binary("debug", service.executable_package),
        args: vec![],
        env: os_env(service.env.clone()),
        cwd: layout.root.clone(),
        stdout: OutputDestination::File(run_dir.join(format!("{}.out.log", service.name))),
        stderr: OutputDestination::File(run_dir.join(format!("{}.err.log", service.name))),
        process_group: ProcessGroupPolicy::Owned,
    }
}

pub(crate) fn wait_healthy(
    service: &ServiceSpec,
    child: &mut OwnedChild,
    stop: &AtomicBool,
) -> Result<StepOutcome> {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    while Instant::now() < deadline {
        if stop_requested(stop) {
            return Ok(StepOutcome::RequestedStop);
        }
        if let Some(status) = child.try_wait()? {
            bail!("{} exited during startup with {status}", service.name);
        }
        if ready(service.http_port) {
            return Ok(StepOutcome::Completed);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "{} did not become healthy on :{}",
        service.name,
        service.http_port
    )
}

fn ready(port: u16) -> bool {
    use std::io::{Read as _, Write as _};
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("valid socket"),
        Duration::from_millis(300),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = [0u8; 64];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    response[..read].starts_with(b"HTTP/1.1 200") || response[..read].starts_with(b"HTTP/1.0 200")
}

fn monitor(
    children: &mut [OwnedChild],
    stop: &AtomicBool,
) -> Result<Option<(usize, std::process::ExitStatus)>> {
    loop {
        if stop_requested(stop) {
            return Ok(None);
        }
        for (index, child) in children.iter_mut().enumerate() {
            if let Some(status) = child.try_wait()? {
                return Ok(Some((index, status)));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn teardown(
    store: &StateStore,
    state: &Arc<Mutex<FleetState>>,
    children: &mut [OwnedChild],
    failed: bool,
) -> Result<()> {
    teardown_with(store, state, children, failed, |child| {
        child.shutdown(SHUTDOWN)?;
        Ok(child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code()))
    })
}

pub(crate) fn teardown_with<F>(
    store: &StateStore,
    state: &Arc<Mutex<FleetState>>,
    children: &mut [OwnedChild],
    failed: bool,
    mut shutdown: F,
) -> Result<()>
where
    F: FnMut(&mut OwnedChild) -> Result<Option<i32>>,
{
    state
        .lock()
        .expect("state mutex poisoned")
        .set_status(FleetStatus::Stopping);
    let mut checkpoint_error = checkpoint(store, state, children, "stopping", None).err();
    let mut cleanup_failures = Vec::new();
    for index in (0..children.len()).rev() {
        state.lock().expect("state mutex poisoned").processes_mut()[index]
            .set_status(ManagedStatus::Stopping);
        if checkpoint_error.is_none() {
            let label = state.lock().expect("state mutex poisoned").processes()[index]
                .label()
                .to_string();
            checkpoint_error =
                checkpoint(store, state, children, "process-stopping", Some(&label)).err();
        }
        let status = match shutdown(&mut children[index]) {
            Ok(code) => ManagedStatus::Exited { code },
            Err(error) => {
                let label = state.lock().expect("state mutex poisoned").processes()[index]
                    .label()
                    .to_string();
                eprintln!("devctl: cleanup {} failed: {error}", label);
                cleanup_failures.push(format!("{label}: {error:#}"));
                state
                    .lock()
                    .expect("state mutex poisoned")
                    .record_failure("cleanup", Some(label))?;
                ManagedStatus::Failed
            }
        };
        state.lock().expect("state mutex poisoned").processes_mut()[index].set_status(status);
        if checkpoint_error.is_none() {
            let label = state.lock().expect("state mutex poisoned").processes()[index]
                .label()
                .to_string();
            checkpoint_error =
                checkpoint(store, state, children, "process-reaped", Some(&label)).err();
        }
    }
    state.lock().expect("state mutex poisoned").set_status(
        if failed || checkpoint_error.is_some() || !cleanup_failures.is_empty() {
            FleetStatus::Failed
        } else {
            FleetStatus::Stopped
        },
    );
    if let Err(error) = store.write_atomic(&state.lock().expect("state mutex poisoned")) {
        state
            .lock()
            .expect("state mutex poisoned")
            .record_failure("checkpoint-final", None::<String>)?;
        if checkpoint_error.is_none() {
            checkpoint_error = Some(error.into());
        }
    }
    match (checkpoint_error, cleanup_failures.is_empty()) {
        (None, true) => Ok(()),
        (None, false) => bail!("cleanup failures: {}", cleanup_failures.join("; ")),
        (Some(error), true) => Err(error),
        (Some(error), false) => bail!(
            "checkpoint failure: {error:#}; cleanup failures: {}",
            cleanup_failures.join("; ")
        ),
    }
}

fn checkpoint(
    store: &StateStore,
    state: &Arc<Mutex<FleetState>>,
    children: &mut [OwnedChild],
    stage: &'static str,
    process: Option<&str>,
) -> Result<()> {
    if let Err(error) = store.checkpoint_or_rollback(
        &state.lock().expect("state mutex poisoned"),
        children,
        SHUTDOWN,
    ) {
        state
            .lock()
            .expect("state mutex poisoned")
            .record_failure(format!("checkpoint-{stage}"), process.map(str::to_owned))?;
        return Err(error.into());
    }
    Ok(())
}

fn stop_requested(stop: &AtomicBool) -> bool {
    stop.load(Ordering::SeqCst) || INTERRUPTED.load(Ordering::SeqCst)
}

fn os_env(env: BTreeMap<String, String>) -> BTreeMap<OsString, OsString> {
    env.into_iter().map(|(k, v)| (k.into(), v.into())).collect()
}

fn executable_on_path(name: &str, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let path = env
        .get("PATH")
        .context("PATH is absent from build environment")?;
    let extensions: Vec<&str> = if cfg!(windows) {
        env.get("PATHEXT")
            .map(|v| v.split(';').collect())
            .unwrap_or_else(|| vec![".EXE", ".CMD", ".BAT"])
    } else {
        vec![""]
    };
    for directory in std::env::split_paths(path) {
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!("{name} not found on sanitized PATH")
}

fn workspace_root() -> Result<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("devctl workspace root")?
        .to_path_buf())
}

fn run_id() -> String {
    let mut bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn control_endpoint(run_dir: &Path, run_id: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = run_dir;
        PathBuf::from(format!(r"\\.\pipe\gamebackend-devctl-{run_id}"))
    }
    #[cfg(target_os = "linux")]
    {
        run_dir.join(format!("control-{run_id}.sock"))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        run_dir.join("unsupported-control")
    }
}

#[cfg(windows)]
fn install_signal_handler() -> Result<()> {
    unsafe extern "system" fn handler(_: u32) -> i32 {
        INTERRUPTED.store(true, Ordering::SeqCst);
        1
    }
    if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1) } == 0
    {
        return Err(std::io::Error::last_os_error()).context("install Ctrl-C handler");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_signal_handler() -> Result<()> {
    extern "C" fn handler(_: i32) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn install_signal_handler() -> Result<()> {
    bail!("devctl supports only Windows and Linux")
}

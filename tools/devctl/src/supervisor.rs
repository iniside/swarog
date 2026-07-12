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
    ServiceSpec, ShutdownPolicy, SpawnSpec, StateStore,
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

fn client_command(store: &StateStore, command: &str) -> Result<()> {
    let state = store.load()?.context("no devctl supervisor state")?;
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
            build(root, topology, &services, &environment, &stop)?;
        }
        edgeca::mint_dev_ca(&ca_cert, &ca_key)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        seed_admin(root, &db_url, &environment, &stop)?;
        for service in &services {
            if stop_requested(&stop) {
                bail!("startup interrupted");
            }
            println!(
                "devctl: starting {} on :{}",
                service.name, service.http_port
            );
            let child = OwnedChild::spawn(spawn_spec(root, run_dir, service))
                .with_context(|| format!("start {}", service.name))?;
            let process = ManagedProcess::new(
                service.name,
                child.identity().clone(),
                run_dir.join(format!("{}.out.log", service.name)),
                run_dir.join(format!("{}.err.log", service.name)),
            )?;
            children.push(child);
            state
                .lock()
                .expect("state mutex poisoned")
                .push_process(process);
            checkpoint(store, &state, &mut children, "spawn", Some(service.name))?;
            if let Err(error) =
                wait_healthy(service, children.last_mut().expect("just pushed"), &stop)
            {
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
                    bail!("{error:#}; failed-state checkpoint also failed: {checkpoint_error:#}");
                }
                return Err(error);
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
) -> Result<()> {
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
    run_transient(
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
    .context("cargo build")
}

fn seed_admin(
    root: &Path,
    db: &str,
    environment: &EnvironmentSnapshot,
    stop: &AtomicBool,
) -> Result<()> {
    let mut env = environment.runtime_environment();
    env.insert("DATABASE_URL".into(), db.into());
    let spec = SpawnSpec {
        label: "devctl-admin-seed".into(),
        executable: binary(root, "adminctl"),
        args: ["create-user", "admin", "--password-stdin"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        env: os_env(env),
        cwd: root.into(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    };
    run_transient(spec, Some(b"admin\n"), stop, ADMIN_TIMEOUT)
        .context("seed development admin user")
}

fn run_transient(
    spec: SpawnSpec,
    stdin: Option<&[u8]>,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<()> {
    let mut child = match stdin {
        Some(bytes) => OwnedChild::spawn_with_stdin_bytes(spec, bytes)?,
        None => OwnedChild::spawn(spec)?,
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("child exited with {status}");
        }
        let interrupted = stop_requested(stop);
        let timed_out = Instant::now() >= deadline;
        if interrupted || timed_out {
            let reason = if interrupted {
                "cancelled"
            } else {
                "timed out"
            };
            let cleanup = child
                .shutdown(SHUTDOWN)
                .err()
                .map(|error| error.to_string());
            if let Some(cleanup) = cleanup {
                bail!("transient child {reason}; cleanup also failed: {cleanup}");
            }
            bail!("transient child {reason} after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_spec(root: &Path, run_dir: &Path, service: &ServiceSpec) -> SpawnSpec {
    SpawnSpec {
        label: service.name.into(),
        executable: binary(root, service.executable_package),
        args: vec![],
        env: os_env(service.env.clone()),
        cwd: root.into(),
        stdout: OutputDestination::File(run_dir.join(format!("{}.out.log", service.name))),
        stderr: OutputDestination::File(run_dir.join(format!("{}.err.log", service.name))),
        process_group: ProcessGroupPolicy::Owned,
    }
}

fn wait_healthy(service: &ServiceSpec, child: &mut OwnedChild, stop: &AtomicBool) -> Result<()> {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    while Instant::now() < deadline {
        if stop_requested(stop) {
            bail!("startup interrupted");
        }
        if let Some(status) = child.try_wait()? {
            bail!("{} exited during startup with {status}", service.name);
        }
        if ready(service.http_port) {
            return Ok(());
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

fn teardown(
    store: &StateStore,
    state: &Arc<Mutex<FleetState>>,
    children: &mut [OwnedChild],
    failed: bool,
) -> Result<()> {
    state
        .lock()
        .expect("state mutex poisoned")
        .set_status(FleetStatus::Stopping);
    let mut checkpoint_error = checkpoint(store, state, children, "stopping", None).err();
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
        let outcome = children[index].shutdown(SHUTDOWN);
        let status = match outcome {
            Ok(_) => ManagedStatus::Exited {
                code: children[index]
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code()),
            },
            Err(error) => {
                let label = state.lock().expect("state mutex poisoned").processes()[index]
                    .label()
                    .to_string();
                eprintln!("devctl: cleanup {} failed: {error}", label);
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
        if failed || checkpoint_error.is_some() {
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
    if let Some(error) = checkpoint_error {
        return Err(error);
    }
    Ok(())
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

fn binary(root: &Path, package: &str) -> PathBuf {
    root.join("target/debug")
        .join(format!("{package}{}", std::env::consts::EXE_SUFFIX))
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

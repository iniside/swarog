use super::cli::{parse, Command, Topology};
use super::control::{self, ControlServer};
use super::supervisor::service_specs;
use processctl::{observe_process_identity, EnvironmentSnapshot, FleetState};
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

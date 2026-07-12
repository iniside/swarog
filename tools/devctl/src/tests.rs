use super::cli::{parse, Command, Topology};
use super::control::{self, ControlServer};
use super::supervisor::service_specs;
use processctl::{observe_process_identity, FleetState};
use std::collections::BTreeMap;
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
            skip_build: false,
            overrides: vec![]
        }
    );
    assert_eq!(
        parse(["up".into(), "split".into(), "--skip-build".into()]).unwrap(),
        Command::Up {
            topology: Topology::Split,
            skip_build: true,
            overrides: vec![]
        }
    );
}

#[test]
fn override_is_structured_without_rendering_its_value() {
    let command = parse([
        "up".into(),
        "--env".into(),
        "DATABASE_URL=postgres://secret".into(),
    ])
    .unwrap();
    match command {
        Command::Up { overrides, .. } => {
            assert_eq!(overrides[0].0, "DATABASE_URL");
            assert_eq!(overrides[0].1, "postgres://secret");
        }
        _ => panic!("expected up"),
    }
    let error = parse(["up".into(), "--env".into(), "broken".into()])
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
    let monolith = service_specs(
        Topology::Monolith,
        "postgres://typed",
        &cert,
        &key,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(monolith.len(), 1);
    assert_eq!(monolith[0].name, "monolith");
    assert!(!monolith[0].env.contains_key("HTTP_PROXY"));
    assert!(!monolith[0].env.contains_key("CARGO_HOME"));

    let split = service_specs(
        Topology::Split,
        "postgres://typed",
        &cert,
        &key,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(split.len(), 12);
    assert!(split
        .iter()
        .all(|service| !service.env.contains_key("HTTP_PROXY")));

    let error = service_specs(
        Topology::Split,
        "postgres://typed",
        &cert,
        &key,
        &BTreeMap::from([("UNDECLARED_SECRET".into(), "do-not-render".into())]),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("UNDECLARED_SECRET"));
    assert!(!error.contains("do-not-render"));
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

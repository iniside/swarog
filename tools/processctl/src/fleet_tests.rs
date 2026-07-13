use std::collections::BTreeMap;

use crate::{
    game_backend_fleet, game_backend_fleet_with_environment, game_backend_monolith,
    EnvironmentSnapshot, FleetError, FleetFlavor, FleetInputs, FleetSpec, ServiceSpec,
};
#[cfg(windows)]
use crate::build_environment;

fn inputs() -> FleetInputs {
    FleetInputs {
        database_url: "postgres://proof".into(),
        edge_ca_cert: "run/edge-ca.crt".into(),
        edge_ca_key: "run/edge-ca.key".into(),
    }
}

#[test]
fn inherited_application_overrides_are_explicit_and_cannot_rewire_topology() {
    let environment = EnvironmentSnapshot::from_values([
        ("ACCOUNTS_DEV_AUTH".into(), "0".into()),
        ("PORT".into(), ":9999".into()),
        ("EDGE_ADDR".into(), ":9998".into()),
        ("EDGE_CA_KEY".into(), "attacker-key".into()),
        ("HTTP_PROXY".into(), "http://proxy".into()),
    ]);
    let fleet = game_backend_fleet_with_environment(
        &inputs(), FleetFlavor::Development, &environment,
    );
    let accounts = fleet.service("accounts-svc").unwrap();
    assert_eq!(accounts.env.get("ACCOUNTS_DEV_AUTH").map(String::as_str), Some("0"));
    assert_eq!(accounts.env.get("PORT").map(String::as_str), Some(":8084"));
    assert_eq!(accounts.env.get("EDGE_ADDR").map(String::as_str), Some(":9003"));
    assert_eq!(accounts.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
    assert!(!accounts.env.contains_key("HTTP_PROXY"));

    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Development, &environment);
    assert_eq!(monolith.env.get("PORT").map(String::as_str), Some(":8080"));
    assert_eq!(monolith.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
}

#[cfg(windows)]
#[test]
fn inherited_windows_baseline_lookup_is_case_insensitive() {
    let environment = EnvironmentSnapshot::from_values([
        ("Path".into(), "typed-path".into()),
        ("SystemRoot".into(), "typed-root".into()),
    ]);
    let runtime = environment.runtime_environment();
    assert_eq!(runtime.get("PATH").map(String::as_str), Some("typed-path"));
    assert_eq!(runtime.get("SYSTEMROOT").map(String::as_str), Some("typed-root"));
}

#[test]
fn proof_fleet_is_the_canonical_twelve_service_snapshot() {
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let snapshot: Vec<_> = fleet
        .services()
        .iter()
        .map(|service| {
            (
                service.name,
                service.executable_package,
                service.http_port,
                service.edge_port,
                service.player_port,
                service.dependencies.clone(),
            )
        })
        .collect();
    assert_eq!(snapshot, vec![
        ("accounts-svc", "accounts-svc", 8084, Some(9003), None, vec![]),
        ("apikeys-svc", "apikeys-svc", 8091, Some(9009), None, vec![]),
        ("audit-svc", "audit-svc", 8086, Some(9004), None, vec![]),
        ("scheduler-svc", "scheduler-svc", 8087, Some(9005), None, vec![]),
        ("rating-svc", "rating-svc", 8089, Some(9007), None, vec![]),
        ("leaderboard-svc", "leaderboard-svc", 8090, Some(9008), None, vec![]),
        ("match-svc", "match-svc", 8088, Some(9006), None, vec!["rating-svc"]),
        ("characters-svc", "characters-svc", 8080, Some(9000), None, vec![]),
        ("config-svc", "config-svc", 8083, Some(9002), None, vec![]),
        ("inventory-svc", "inventory-svc", 8081, Some(9001), None, vec!["characters-svc", "config-svc"]),
        ("gateway-svc", "gateway-svc", 8082, None, Some(9100), vec!["characters-svc", "inventory-svc", "accounts-svc", "match-svc", "leaderboard-svc", "apikeys-svc"]),
        ("admin-svc", "admin-svc", 8085, None, None, vec!["characters-svc", "inventory-svc", "config-svc", "accounts-svc", "audit-svc", "scheduler-svc", "apikeys-svc"]),
    ]);
}

#[test]
fn proof_overlay_is_explicit_and_name_lookup_is_stable() {
    let development = game_backend_fleet(&inputs(), FleetFlavor::Development);
    let proof = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    assert!(!development.service("accounts-svc").unwrap().env.contains_key("EPIC_TOKEN_URL"));
    assert_eq!(
        proof
            .service("accounts-svc")
            .unwrap()
            .env
            .get("EPIC_REDIRECT_URI")
            .map(String::as_str),
        Some("http://127.0.0.1:8082/accounts/epic/callback")
    );
    assert_eq!(proof.service("scheduler-svc").unwrap().env.get("SCHEDULER_ENABLED").map(String::as_str), Some("1"));
    assert!(matches!(proof.service("missing"), Err(FleetError::UnknownService(_))));
}

#[test]
fn disk_drift_compares_names_not_order() {
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let reversed = fleet.services().iter().rev().map(|service| service.name.to_string());
    assert!(fleet.validate_names(reversed).is_ok());
    assert!(matches!(fleet.validate_names(["accounts-svc".to_string()]), Err(FleetError::DiskDrift { .. })));
}

#[test]
fn hard_dependencies_must_appear_earlier_in_startup_order() {
    let service = |name, dependencies| ServiceSpec {
        name,
        executable_package: name,
        http_port: 1,
        edge_port: None,
        player_port: None,
        dependencies,
        env: BTreeMap::new(),
        overrideable_env: &[],
    };
    let error = FleetSpec::new(vec![
        service("consumer-svc", vec!["provider-svc"]),
        service("provider-svc", vec![]),
    ])
    .unwrap_err();
    assert!(matches!(error, FleetError::DependencyNotEarlier { .. }));
}

#[cfg(windows)]
#[test]
fn build_environment_preserves_appdata_case_insensitively() {
    let environment = EnvironmentSnapshot::from_values([
        ("appdata".into(), r"C:\Users\test\AppData\Roaming".into()),
        ("localappdata".into(), r"C:\Users\test\AppData\Local".into()),
    ]);

    let env = environment.build_environment();

    assert_eq!(
        env.get("APPDATA").map(String::as_str),
        Some(r"C:\Users\test\AppData\Roaming")
    );
    assert!(!env.contains_key("LOCALAPPDATA"));
}

#[cfg(windows)]
#[test]
fn sanitized_build_path_contains_the_discovered_msvc_linker() {
    let env = build_environment();
    assert_eq!(
        env.get("ProgramFiles(x86)").map(String::as_str),
        std::env::var("ProgramFiles(x86)").ok().as_deref()
    );
    let path = env.get("PATH").expect("build environment has PATH");
    assert!(
        std::env::split_paths(path).any(|directory| directory.join("link.exe").is_file()),
        "sanitized build PATH must contain an installed MSVC linker"
    );
    assert!(
        env.get("LIB")
            .into_iter()
            .flat_map(std::env::split_paths)
            .any(|directory| directory.join("kernel32.lib").is_file()),
        "sanitized build LIB must contain the Windows SDK libraries"
    );
    assert!(env.contains_key("INCLUDE"));
}

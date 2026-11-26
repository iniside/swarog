//! Pins every launcher copy of the dev `ACCOUNTS_SIGNING_KEK` to
//! `testdb::DEV_ACCOUNTS_SIGNING_KEK`, and every launcher's `ACCOUNTS_OIDC_ISSUER` to its own
//! front door. verifyctl sees testdb, processctl's fleet builders and weles's own fleet loader;
//! none of those launchers can depend on testdb themselves.

use std::path::{Path, PathBuf};

use processctl::{
    game_backend_fleet_with_environment, game_backend_monolith, EnvironmentSnapshot, FleetFlavor,
    FleetInputs,
};
use weles::manifest::{Port, ServiceDef};

/// The issuer accounts takes when `ACCOUNTS_OIDC_ISSUER` is unset: the monolith's own port.
const ACCOUNTS_DEFAULT_ISSUER: &str = "http://localhost:8080/accounts/oidc";

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

/// Both processctl topologies, Development and Proof, hand accounts the testdb KEK.
#[test]
fn processctl_fleets_seal_with_the_testdb_kek() {
    let inputs = FleetInputs {
        database_url: "postgres://pin".into(),
        edge_ca_cert: "run/edge-ca.crt".into(),
        edge_ca_key: "run/edge-ca.key".into(),
    };
    let bare = EnvironmentSnapshot::from_values([]);
    for flavor in [FleetFlavor::Development, FleetFlavor::Proof] {
        let fleet = game_backend_fleet_with_environment(&inputs, flavor, &bare);
        let split = &fleet.service("accounts-svc").expect("split fleet hosts accounts-svc").env;
        let monolith = &game_backend_monolith(&inputs, flavor, &bare).env;
        for (topology, env) in [("split", split), ("monolith", monolith)] {
            assert_eq!(
                env.get("ACCOUNTS_SIGNING_KEK").map(String::as_str),
                Some(testdb::DEV_ACCOUNTS_SIGNING_KEK),
                "{topology} {flavor:?}"
            );
        }
    }
}

fn hosts_accounts(service: &ServiceDef) -> bool {
    service.pkg == "server" || service.provider.as_deref() == Some("accounts")
}

fn is_front_door(service: &ServiceDef) -> bool {
    service.pkg == "server" || service.provider.as_deref() == Some("gateway")
}

/// Every `weles/fleet*.toml`: each service hosting accounts (the monolith `server` or the
/// `accounts` provider) seals with the testdb KEK and names the fleet's front door as issuer.
#[test]
fn weles_fleets_seal_with_the_testdb_kek_and_name_their_front_door() {
    let mut fleets: Vec<PathBuf> = std::fs::read_dir(repo("weles"))
        .expect("read weles/")
        .map(|entry| entry.expect("weles/ entry").path())
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
            name.starts_with("fleet") && name.ends_with(".toml")
        })
        .collect();
    fleets.sort();
    assert!(!fleets.is_empty(), "no weles/fleet*.toml found");
    for fleet in &fleets {
        let def = weles::fleet_toml::load(fleet).unwrap_or_else(|e| panic!("{fleet:?}: {e}"));
        let fronts: Vec<&ServiceDef> = def.services.iter().filter(|s| is_front_door(s)).collect();
        let [front] = fronts.as_slice() else {
            panic!("{fleet:?} has {} front doors, want one", fronts.len());
        };
        let Port::Literal(port) = front.http_port else {
            panic!("{fleet:?}: front door {} has no fixed port", front.name);
        };
        let issuer = format!("http://localhost:{port}/accounts/oidc");
        let accounts: Vec<&ServiceDef> =
            def.services.iter().filter(|s| hosts_accounts(s)).collect();
        assert!(!accounts.is_empty(), "{fleet:?} hosts no accounts");
        for service in accounts {
            let env = &service.env;
            assert_eq!(
                env.get("ACCOUNTS_SIGNING_KEK").map(String::as_str),
                Some(testdb::DEV_ACCOUNTS_SIGNING_KEK),
                "{fleet:?} {}",
                service.name
            );
            assert_eq!(
                env.get("ACCOUNTS_OIDC_ISSUER").map_or(ACCOUNTS_DEFAULT_ISSUER, String::as_str),
                issuer,
                "{fleet:?} {}",
                service.name
            );
        }
    }
}

/// The trimmed items of the `environment:` list under the `server:` service of a compose
/// file: a line scan, since verifyctl has no YAML parser and both files keep that layout.
fn server_environment(file: &str) -> Vec<String> {
    let text = std::fs::read_to_string(repo(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    let mut items = Vec::new();
    let mut in_environment = false;
    for line in text.lines().skip_while(|line| *line != "  server:").skip(1) {
        if !line.starts_with("    ") && !line.trim().is_empty() {
            break;
        }
        if line.starts_with("    ") && !line.starts_with("     ") {
            in_environment = line.trim() == "environment:";
        } else if in_environment {
            if let Some(item) = line.trim().strip_prefix("- ") {
                items.push(item.to_string());
            }
        }
    }
    assert!(!items.is_empty(), "{file} has no server environment");
    items
}

/// compose.yaml passes both keys through to `server` bare, from the operator's shell.
#[test]
fn compose_passes_both_accounts_keys_through() {
    let items = server_environment("compose.yaml");
    for key in ["ACCOUNTS_SIGNING_KEK", "ACCOUNTS_OIDC_ISSUER"] {
        assert!(items.iter().any(|item| item == key), "compose.yaml server does not pass {key}");
    }
}

/// The host-port expression compose.yaml publishes the server's 8080 on.
fn compose_http_port() -> String {
    let text = std::fs::read_to_string(repo("compose.yaml")).expect("read compose.yaml");
    let mapping = text
        .lines()
        .map(|line| line.trim().trim_start_matches("- ").trim_matches('"'))
        .find(|line| line.ends_with(":8080") && line.contains("${"))
        .expect("compose.yaml publishes 8080");
    let start = mapping.rfind("${").expect("a port expression");
    let end = start + mapping[start..].find('}').expect("a closed expression") + 1;
    mapping[start..end].to_string()
}

#[test]
fn compose_dev_seals_with_the_testdb_kek_and_names_the_published_port() {
    let items = server_environment("compose.dev.yaml");
    let values = |key: &str| -> Vec<String> {
        let prefix = format!("{key}=");
        items.iter().filter_map(|item| item.strip_prefix(&prefix)).map(String::from).collect()
    };
    assert_eq!(values("ACCOUNTS_SIGNING_KEK"), [testdb::DEV_ACCOUNTS_SIGNING_KEK]);
    let issuer = format!("http://localhost:{}/accounts/oidc", compose_http_port());
    assert_eq!(values("ACCOUNTS_OIDC_ISSUER"), [issuer]);
}

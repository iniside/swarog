//! Unit tests for archcheck's path classification and the single-front-door allow-list.
//! The dependency-edge scan itself runs against real `cargo metadata` (exercised by the
//! `fortress` verify stage); here we pin the pure helpers that decide WHAT a package is
//! and WHICH cmd binaries may host the `gateway` crate.

use super::{
    classify, cmd_is_a_main, forbidden_api_deps, has_non_dev_dep, Kind, FORBIDDEN_API_DEPS,
    FRONT_DOOR_HOSTS, GATEWAY_CRATE,
};

#[test]
fn classifies_module_manifest() {
    assert!(matches!(
        classify("G:/Projects/GameBackend/modules/characters/Cargo.toml"),
        Kind::Module(n) if n == "characters"
    ));
}

#[test]
fn classifies_rpc_glue_manifest() {
    assert!(matches!(
        classify("/repo/api/characters/rpc/Cargo.toml"),
        Kind::Rpc(n) if n == "characters"
    ));
}

#[test]
fn classifies_cmd_manifest_by_dir_name() {
    assert!(matches!(
        classify("/repo/cmd/characters-svc/Cargo.toml"),
        Kind::Cmd(n) if n == "characters-svc"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\cmd\gateway-svc\Cargo.toml"),
        Kind::Cmd(n) if n == "gateway-svc"
    ));
}

#[test]
fn non_module_non_cmd_is_other() {
    assert!(matches!(
        classify("/repo/core/app/Cargo.toml"),
        Kind::Other
    ));
}

#[test]
fn classifies_api_contract_manifest() {
    assert!(matches!(
        classify("/repo/api/config/api/Cargo.toml"),
        Kind::Api(n) if n == "config"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\api\characters\api\Cargo.toml"),
        Kind::Api(n) if n == "characters"
    ));
}

#[test]
fn only_front_processes_may_host_the_gateway() {
    // The two sanctioned front doors.
    assert!(FRONT_DOOR_HOSTS.contains(&"gateway-svc"));
    assert!(FRONT_DOOR_HOSTS.contains(&"server"));
    // Domain svcs must NOT be on the allow-list — a `gateway` dep there is a violation.
    for svc in [
        "characters-svc",
        "inventory-svc",
        "config-svc",
        "accounts-svc",
        "match-svc",
        "leaderboard-svc",
    ] {
        assert!(
            !FRONT_DOOR_HOSTS.contains(&svc),
            "{svc} must not be permitted to host the front door"
        );
    }
    assert_eq!(GATEWAY_CRATE, "gateway");
}

#[test]
fn forbidden_api_deps_include_edge_and_remote() {
    // Fact 6: the realistic regression vector is a <name>api picking up edge/remote,
    // not a raw transport crate directly — both must be in the forbid-list.
    assert!(FORBIDDEN_API_DEPS.contains(&"edge"));
    assert!(FORBIDDEN_API_DEPS.contains(&"remote"));
    assert!(FORBIDDEN_API_DEPS.contains(&"tokio"));
}

#[test]
fn api_crate_with_edge_dep_is_a_violation() {
    // A synthetic <name>api package that (regressively) depends on `edge`.
    let deps = serde_json::json!([
        { "name": "serde", "kind": null },
        { "name": "edge", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert_eq!(forbidden_api_deps(&deps), vec!["edge".to_string()]);
}

#[test]
fn api_crate_with_edge_dev_dep_only_is_clean() {
    // A dev-dependency on a transport crate (e.g. an integration test) is not a
    // violation — only the runtime import graph is constrained.
    let deps = serde_json::json!([
        { "name": "edge", "kind": "dev" },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(forbidden_api_deps(&deps).is_empty());
}

#[test]
fn every_svc_and_server_is_a_main_requiring_metrics() {
    assert!(cmd_is_a_main("characters-svc"));
    assert!(cmd_is_a_main("server"));
    // A hypothetical non-process cmd/ helper crate is not constrained.
    assert!(!cmd_is_a_main("playercli"));
}

#[test]
fn cmd_without_metrics_dep_is_a_violation() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(!has_non_dev_dep(&deps, "metrics"));
}

#[test]
fn cmd_with_metrics_dep_is_clean() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
        { "name": "metrics", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(has_non_dev_dep(&deps, "metrics"));
}

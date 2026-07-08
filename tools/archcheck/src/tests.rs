//! Unit tests for archcheck's path classification and the single-front-door allow-list.
//! The dependency-edge scan itself runs against real `cargo metadata` (exercised by the
//! `fortress` verify stage); here we pin the pure helpers that decide WHAT a package is
//! and WHICH cmd binaries may host the `gateway` crate.

use super::{classify, Kind, FRONT_DOOR_HOSTS, GATEWAY_CRATE};

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
    // The `api/<name>/api` contract crate is not `<name>/rpc`, so it is Other.
    assert!(matches!(
        classify("/repo/api/characters/api/Cargo.toml"),
        Kind::Other
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

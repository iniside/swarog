use super::*;
use std::net::{TcpListener, UdpSocket};

#[test]
fn health_client_builds_without_an_ambient_runtime() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    health_client(&runtime).unwrap();
}

#[test]
fn c1_to_c6_predicates_are_exact() {
    assert!(predicate(0, "anything", Expected::Success));
    assert!(!predicate(1, "anything", Expected::Success));
    assert!(predicate(
        1,
        "status Unauthorized",
        Expected::Error("Unauthorized")
    ));
    assert!(!predicate(
        0,
        "Unauthorized",
        Expected::Error("Unauthorized")
    ));
    assert!(!predicate(1, "Forbidden", Expected::Error("NotFound")));
    assert!(predicate(
        1,
        "status Forbidden",
        Expected::Error("Forbidden")
    ));
    assert!(predicate(0, "flow complete", Expected::Success));
}

#[test]
fn occupied_port_is_detected() {
    let listener = TcpListener::bind(("127.0.0.1", HTTP_PORT)).unwrap();
    assert!(ports_occupied());
    drop(listener);
    let socket = UdpSocket::bind(("127.0.0.1", PLAYER_PORT)).unwrap();
    assert!(ports_occupied());
    drop(socket);
}

#[test]
fn cleanup_never_names_unowned_processes() {
    let source = include_str!("csharp.rs");
    for forbidden in [
        ["task", "kill"].concat(),
        ["p", "kill"].concat(),
        ["Stop", "-Process"].concat(),
    ] {
        assert!(!source.contains(&forbidden));
    }
}

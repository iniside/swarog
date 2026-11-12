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

#[test]
fn tls_cases_differ_from_the_pinned_pass_only_in_the_name_or_the_trust() {
    let cases = tls_cases("root.pem");
    let labels: Vec<&str> = cases.iter().map(|(label, _, _)| *label).collect();
    assert_eq!(labels, ["t1", "t2", "t3"]);
    let pinned = &cases[0].1;
    assert!(matches!(cases[0].2, Expected::Success));
    assert!(pinned.windows(2).any(|w| w == ["--ca", "root.pem"]), "{pinned:?}");
    assert!(pinned.windows(2).any(|w| w == ["--server-name", "localhost"]), "{pinned:?}");

    let renamed: Vec<String> = pinned
        .iter()
        .map(|a| if a == "localhost" { "not-localhost.verifyctl.test".to_string() } else { a.clone() })
        .collect();
    assert_eq!(cases[1].1, renamed);
    assert!(matches!(cases[1].2, Expected::Error(PIN_REJECTED)));

    let system: Vec<String> = pinned
        .iter()
        .filter(|a| *a != "root.pem")
        .map(|a| if a == "--ca" { "--system-roots".to_string() } else { a.clone() })
        .collect();
    assert_eq!(cases[2].1, system);
    assert!(matches!(cases[2].2, Expected::Error(CHAIN_REJECTED)));
}

#[test]
fn tls_rejection_markers_tell_the_pin_from_the_os_chain() {
    let pin = concat!(
        "gbclient: IOException: dial 127.0.0.1:9100: 127.0.0.1:9100: AuthenticationException: ",
        "The remote certificate was rejected by the provided RemoteCertificateValidationCallback."
    );
    let chain = concat!(
        "gbclient: IOException: dial 127.0.0.1:9100: 127.0.0.1:9100: AuthenticationException: ",
        "The remote certificate is invalid because of errors in the certificate chain: RemoteCertificateChainErrors"
    );
    assert!(predicate(1, pin, Expected::Error(PIN_REJECTED)));
    assert!(!predicate(1, chain, Expected::Error(PIN_REJECTED)));
    assert!(predicate(1, chain, Expected::Error(CHAIN_REJECTED)));
    assert!(!predicate(1, pin, Expected::Error(CHAIN_REJECTED)));
    assert!(!predicate(0, pin, Expected::Error(PIN_REJECTED)));
}

#[tokio::test]
async fn minted_chain_reaches_the_root_only_through_the_intermediate() {
    let dir = std::env::temp_dir().join(format!("verifyctl-csharp-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let chain = mint_player_chain(&dir).unwrap();
    let chain_pem = std::fs::read_to_string(&chain.chain).unwrap();
    let blocks: Vec<&str> = chain_pem.split_inclusive("-----END CERTIFICATE-----").filter(|b| b.contains("BEGIN")).collect();
    assert_eq!(blocks.len(), 2, "the chain file is the leaf then the intermediate");
    let leaf_only = dir.join("leaf-only.pem");
    std::fs::write(&leaf_only, blocks[0]).unwrap();
    let root = edge::DevCA::load_cert_only(chain.root.to_str().unwrap()).unwrap();
    let guard = Duration::from_secs(30);

    let full = edge::PlayerServer::new()
        .listen(
            "127.0.0.1:0".parse().unwrap(),
            edge::PlayerTlsConfig::new(edge::pem_cert_resolver(&chain.chain, &chain.key).unwrap()).unwrap(),
        )
        .unwrap();
    tokio::time::timeout(guard, edge::PlayerClient::dial(full.local_addr(), "localhost", &root))
        .await
        .expect("dial hang-guard")
        .expect("the root accepts the leaf for localhost through the served intermediate");
    full.close();

    let bare = edge::PlayerServer::new()
        .listen(
            "127.0.0.1:0".parse().unwrap(),
            edge::PlayerTlsConfig::new(edge::pem_cert_resolver(&leaf_only, &chain.key).unwrap()).unwrap(),
        )
        .unwrap();
    let refused = tokio::time::timeout(guard, edge::PlayerClient::dial(bare.local_addr(), "localhost", &root))
        .await
        .expect("dial hang-guard");
    match refused {
        Err(edge::Error::Connection(text)) => assert!(text.contains("invalid peer certificate"), "{text}"),
        Err(other) => panic!("expected a certificate rejection, got {other:?}"),
        Ok(_) => panic!("the root must not accept the leaf without the intermediate"),
    }
    bare.close();
}

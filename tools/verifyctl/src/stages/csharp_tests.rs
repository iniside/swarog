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
        Expected::Error(&["Unauthorized"])
    ));
    assert!(!predicate(
        0,
        "Unauthorized",
        Expected::Error(&["Unauthorized"])
    ));
    assert!(!predicate(1, "Forbidden", Expected::Error(&["NotFound"])));
    assert!(predicate(
        1,
        "status Forbidden",
        Expected::Error(&["Forbidden"])
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
fn tls_cases_differ_from_the_pinned_pass_in_one_dimension_each() {
    let cases = tls_cases("root.pem", "stranger.pem");
    let labels: Vec<&str> = cases.iter().map(|(label, _, _)| *label).collect();
    assert_eq!(labels, ["t1", "t2", "t3", "t4"]);
    let pinned = &cases[0].1;
    assert!(matches!(cases[0].2, Expected::Success));
    assert!(pinned.windows(2).any(|w| w == ["--ca", "root.pem"]), "{pinned:?}");
    assert!(pinned.windows(2).any(|w| w == ["--addr", "127.0.0.1:9100"]), "{pinned:?}");
    assert!(!pinned.iter().any(|a| a == "--server-name"), "t1 must take the IP-literal default name: {pinned:?}");

    let named = &cases[1].1;
    let at = named.iter().position(|a| a == "--server-name").expect("t2 names the server");
    assert_eq!(named[at + 1], "not-localhost.verifyctl.test");
    let unnamed: Vec<String> = named[..at].iter().chain(&named[at + 2..]).cloned().collect();
    assert_eq!(&unnamed, pinned);
    assert!(matches!(cases[1].2, Expected::Error(&[CALLBACK_REJECTED, NAME_MISMATCH])));

    let system: Vec<String> = pinned
        .iter()
        .filter(|a| *a != "root.pem")
        .map(|a| if a == "--ca" { "--system-roots".to_string() } else { a.clone() })
        .collect();
    assert_eq!(cases[2].1, system);
    assert!(matches!(cases[2].2, Expected::Error(&[CALLBACK_REJECTED, SYSTEM_CHAIN_REJECTED])));

    let stranger: Vec<String> = pinned
        .iter()
        .map(|a| if a == "root.pem" { "stranger.pem".to_string() } else { a.clone() })
        .collect();
    assert_eq!(cases[3].1, stranger);
    assert!(matches!(cases[3].2, Expected::Error(&[CALLBACK_REJECTED, ROOT_NOT_PINNED, PINNED_CA])));
}

/// Each refusing case's markers match the output of its own cause and of no other.
/// The outputs are gbclient's stderr lines as `Program.cs`, `PinToCa` and
/// `ReportSystemRejection` format them; `bare callback` is a refusal with no reason
/// line, so no case may pass on the generic callback refusal alone.
#[test]
fn tls_case_markers_match_only_their_own_cause() {
    let callback = concat!(
        "gbclient: IOException: dial 127.0.0.1:9100: 127.0.0.1:9100: AuthenticationException: ",
        "The remote certificate was rejected by the provided RemoteCertificateValidationCallback."
    );
    let outputs = [
        ("t2", format!("gbclient: certificate rejected: policy errors RemoteCertificateNameMismatch\n{callback}")),
        ("t3", format!("gbclient: certificate rejected: policy errors RemoteCertificateChainErrors: UntrustedRoot\n{callback}")),
        (
            "t3",
            format!("gbclient: certificate rejected: policy errors RemoteCertificateChainErrors: PartialChain, UntrustedRoot\n{callback}"),
        ),
        ("t4", format!("gbclient: certificate rejected: chain does not build to the pinned CA: PartialChain\n{callback}")),
        (
            "t4",
            format!("gbclient: certificate rejected: chain does not end at the pinned CA: root CN=verifyctl csharp root\n{callback}"),
        ),
        ("bare callback", callback.to_string()),
    ];
    let cases = tls_cases("root.pem", "stranger.pem");
    for (label, _, expected) in &cases[1..] {
        for (cause, output) in &outputs {
            assert_eq!(predicate(1, output, *expected), label == cause, "{label} against {cause}: {output}");
            assert!(!predicate(0, output, *expected), "{label}: a zero exit must never pass");
        }
    }
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
    assert_ne!(std::fs::read(&chain.stranger_root).unwrap(), std::fs::read(&chain.root).unwrap());
    let stranger = edge::DevCA::load_cert_only(chain.stranger_root.to_str().unwrap()).unwrap();
    let refused = tokio::time::timeout(guard, edge::PlayerClient::dial(full.local_addr(), "localhost", &stranger))
        .await
        .expect("dial hang-guard");
    match refused {
        // BadSignature, not UnknownIssuer: the stranger matches the intermediate's
        // issuer name, so only its key tells it from the root.
        Err(edge::Error::Connection(text)) => assert!(text.contains("BadSignature"), "{text}"),
        Err(other) => panic!("expected a certificate rejection, got {other:?}"),
        Ok(_) => panic!("the stranger root must not accept the leaf"),
    }
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

//! Unit tests for `TlsFronts::from_values` — pure values in/out through a map-backed
//! getter, no process-global env mutation.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::{PemFiles, TlsFront, TlsFronts};

fn parse(vars: &[(&str, &str)]) -> anyhow::Result<Option<TlsFront>> {
    let map: HashMap<String, String> =
        vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    TlsFronts::from_values(|k| map.get(k).cloned()).map(|fronts| fronts.https)
}

#[test]
fn unset_and_blank_and_off_mean_plain_http() {
    assert_eq!(parse(&[]).unwrap(), None, "unset");
    for mode in ["", "  ", "off", "OFF", " off "] {
        let front = parse(&[("TLS_MODE", mode)]).unwrap();
        assert_eq!(front, None, "mode {mode:?}");
    }
}

#[test]
fn unknown_mode_bails_loudly() {
    let err = parse(&[("TLS_MODE", "acme-staging")]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("acme-staging"), "{msg}");
    assert!(msg.contains("TLS_MODE"), "{msg}");
}

#[test]
fn files_mode_requires_both_paths() {
    let front = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "/etc/tls/cert.pem"),
        ("TLS_KEY_PATH", "/etc/tls/key.pem"),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Files {
            cert: PathBuf::from("/etc/tls/cert.pem"),
            key: PathBuf::from("/etc/tls/key.pem"),
        })
    );

    let err = parse(&[("TLS_MODE", "files"), ("TLS_KEY_PATH", "/k.pem")]).unwrap_err();
    assert!(err.to_string().contains("TLS_CERT_PATH"), "{err}");
    let err = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "/c.pem"),
        ("TLS_KEY_PATH", "   "),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("TLS_KEY_PATH"), "{err}");
}

#[test]
fn files_mode_is_case_insensitive() {
    let front = parse(&[
        ("TLS_MODE", "FILES"),
        ("TLS_CERT_PATH", "c.pem"),
        ("TLS_KEY_PATH", "k.pem"),
    ])
    .unwrap();
    assert!(matches!(front, Some(TlsFront::Files { .. })));
}

#[test]
fn acme_mode_parses_domains_contact_and_cache_dir() {
    let front = parse(&[
        ("TLS_MODE", "acme"),
        ("ACME_DOMAINS", " example.com, admin.example.com ,, "),
        ("ACME_CONTACT", "ops@example.com"),
        ("ACME_CACHE_DIR", "/var/lib/acme"),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string(), "admin.example.com".to_string()],
            cache_dir: PathBuf::from("/var/lib/acme"),
            contact: Some("ops@example.com".to_string()),
        })
    );
}

#[test]
fn acme_mode_defaults_cache_dir_and_optional_contact() {
    let front = parse(&[("TLS_MODE", "acme"), ("ACME_DOMAINS", "example.com")]).unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
    let front = parse(&[
        ("TLS_MODE", "acme"),
        ("ACME_DOMAINS", "example.com"),
        ("ACME_CONTACT", "  "),
        ("ACME_CACHE_DIR", " "),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
}

#[test]
fn acme_mode_requires_at_least_one_domain() {
    let unset = parse(&[("TLS_MODE", "acme")]).unwrap_err();
    assert!(unset.to_string().contains("ACME_DOMAINS"), "unset: {unset}");
    for domains in ["", "  ,  , "] {
        let err = parse(&[("TLS_MODE", "acme"), ("ACME_DOMAINS", domains)]).unwrap_err();
        assert!(err.to_string().contains("ACME_DOMAINS"), "domains {domains:?}: {err}");
    }
}

#[test]
fn tls_paths_ignore_acme_vars_and_vice_versa() {
    let front = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "c.pem"),
        ("TLS_KEY_PATH", "k.pem"),
        ("ACME_DOMAINS", "example.com"),
    ])
    .unwrap();
    assert!(matches!(front, Some(TlsFront::Files { .. })));
    let front = parse(&[("TLS_CERT_PATH", "c.pem"), ("ACME_DOMAINS", "example.com")]).unwrap();
    assert_eq!(front, None);
}

fn parse_fronts(vars: &[(&str, &str)]) -> anyhow::Result<TlsFronts> {
    let map: HashMap<String, String> =
        vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    TlsFronts::from_values(|k| map.get(k).cloned())
}

fn pem_files(cert: &str, key: &str) -> PemFiles {
    PemFiles { cert: PathBuf::from(cert), key: PathBuf::from(key) }
}

#[test]
fn player_files_are_both_or_neither() {
    assert_eq!(parse_fronts(&[]).unwrap(), TlsFronts::default());
    let both = parse_fronts(&[("PLAYER_TLS_CERT_PATH", "/p/c.pem"), ("PLAYER_TLS_KEY_PATH", "/p/k.pem")])
        .unwrap();
    assert_eq!(both.player_files, Some(pem_files("/p/c.pem", "/p/k.pem")));
    assert_eq!(both.https, None);

    let err = parse_fronts(&[("PLAYER_TLS_CERT_PATH", "/p/c.pem")]).unwrap_err();
    assert!(err.to_string().contains("PLAYER_TLS_KEY_PATH"), "{err}");
    let err = parse_fronts(&[("PLAYER_TLS_KEY_PATH", "/p/k.pem")]).unwrap_err();
    assert!(err.to_string().contains("PLAYER_TLS_CERT_PATH"), "{err}");
    let err = parse_fronts(&[("PLAYER_TLS_CERT_PATH", "/p/c.pem"), ("PLAYER_TLS_KEY_PATH", "  ")])
        .unwrap_err();
    assert!(err.to_string().contains("PLAYER_TLS_KEY_PATH"), "blank is unset: {err}");
    let blank = parse_fronts(&[("PLAYER_TLS_CERT_PATH", ""), ("PLAYER_TLS_KEY_PATH", " ")]).unwrap();
    assert_eq!(blank, TlsFronts::default());
}

#[test]
fn one_half_of_the_player_files_fails_under_every_tls_mode() {
    let modes: [&[(&str, &str)]; 3] = [
        &[],
        &[("TLS_MODE", "files"), ("TLS_CERT_PATH", "c.pem"), ("TLS_KEY_PATH", "k.pem")],
        &[("TLS_MODE", "acme"), ("ACME_DOMAINS", "example.com")],
    ];
    for mode in modes {
        let mut vars = mode.to_vec();
        vars.push(("PLAYER_TLS_CERT_PATH", "/p/c.pem"));
        let err = parse_fronts(&vars).unwrap_err();
        assert!(err.to_string().contains("PLAYER_TLS_KEY_PATH"), "{mode:?}: {err}");
    }
}

#[test]
fn player_files_ride_alongside_each_https_mode() {
    let player = [("PLAYER_TLS_CERT_PATH", "/p/c.pem"), ("PLAYER_TLS_KEY_PATH", "/p/k.pem")];
    let expected = Some(pem_files("/p/c.pem", "/p/k.pem"));

    let off = parse_fronts(&[player[0], player[1], ("TLS_MODE", "off")]).unwrap();
    assert_eq!(off, TlsFronts { https: None, player_files: expected.clone() });

    let files = parse_fronts(&[
        player[0],
        player[1],
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "c.pem"),
        ("TLS_KEY_PATH", "k.pem"),
    ])
    .unwrap();
    assert_eq!(
        files,
        TlsFronts {
            https: Some(TlsFront::Files { cert: PathBuf::from("c.pem"), key: PathBuf::from("k.pem") }),
            player_files: expected.clone(),
        }
    );

    let acme = parse_fronts(&[player[0], player[1], ("TLS_MODE", "acme"), ("ACME_DOMAINS", "example.com")])
        .unwrap();
    assert_eq!(
        acme,
        TlsFronts {
            https: Some(TlsFront::Acme {
                domains: vec!["example.com".to_string()],
                cache_dir: PathBuf::from("run/acme-cache"),
                contact: None,
            }),
            player_files: expected,
        }
    );
}

struct Minted {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn mint(dir: &std::path::Path, name: &str) -> Minted {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
    std::fs::create_dir_all(dir).unwrap();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec![name.to_string()])
        .unwrap()
        .signed_by(&leaf_key, &ca, &ca_key)
        .unwrap();
    let minted = Minted { ca: dir.join("ca.pem"), cert: dir.join("cert.pem"), key: dir.join("key.pem") };
    std::fs::write(&minted.ca, ca.pem()).unwrap();
    std::fs::write(&minted.cert, leaf.pem()).unwrap();
    std::fs::write(&minted.key, leaf_key.serialize_pem()).unwrap();
    minted
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("app-player-tls-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn anchor(ca: &std::path::Path) -> edge::TrustAnchor {
    edge::DevCA::load_cert_only(ca.to_str().unwrap()).unwrap()
}

const DIAL_GUARD: std::time::Duration = std::time::Duration::from_secs(30);

/// Binds a player QUIC server on `tls` and dials it as a client trusting `trust` and
/// verifying `name`.
async fn dial(tls: edge::PlayerTlsConfig, name: &str, trust: &edge::TrustAnchor) -> Result<(), edge::Error> {
    let running = edge::PlayerServer::new().listen("127.0.0.1:0".parse().unwrap(), tls).unwrap();
    let dial = tokio::time::timeout(DIAL_GUARD, edge::PlayerClient::dial(running.local_addr(), name, trust))
        .await
        .expect("dial hang-guard");
    running.close();
    dial.map(|_client| ())
}

async fn handshakes(tls: edge::PlayerTlsConfig, name: &str, trust: &edge::TrustAnchor) -> bool {
    dial(tls, name, trust).await.is_ok()
}

/// The client refused the server's certificate: a handshake failure
/// (`Error::Connection`) naming the invalid peer certificate, not a dial timeout.
fn assert_certificate_rejected(res: Result<(), edge::Error>, what: &str) {
    match res {
        Err(edge::Error::Connection(text)) => assert!(
            text.contains("invalid peer certificate"),
            "{what}: handshake failed but not on the certificate: {text}"
        ),
        other => panic!("{what}: expected a certificate rejection, got {other:?}"),
    }
}

fn files_front(m: &Minted) -> TlsFront {
    TlsFront::Files { cert: m.cert.clone(), key: m.key.clone() }
}

#[tokio::test]
async fn player_files_beat_the_https_certificate() {
    let https = mint(&fresh_dir("beat-https"), "https.test");
    let player = mint(&fresh_dir("beat-player"), "player.test");
    let (_, https_certs) = crate::resolve_https_tls(Some(&files_front(&https))).unwrap();
    let files = PemFiles { cert: player.cert.clone(), key: player.key.clone() };

    let tls = crate::resolve_player_tls(Some(&files), https_certs.clone()).unwrap();
    assert!(handshakes(tls, "player.test", &anchor(&player.ca)).await, "own files must be served");
    let tls = crate::resolve_player_tls(Some(&files), https_certs).unwrap();
    assert_certificate_rejected(
        dial(tls, "https.test", &anchor(&https.ca)).await,
        "the HTTPS certificate served although the player has its own files",
    );
}

#[tokio::test]
async fn files_mode_serves_the_https_certificate_on_the_player_front() {
    let https = mint(&fresh_dir("shared"), "https.test");
    let (_, https_certs) = crate::resolve_https_tls(Some(&files_front(&https))).unwrap();
    assert!(https_certs.is_some());

    let tls = crate::resolve_player_tls(None, https_certs).unwrap();
    assert!(handshakes(tls, "https.test", &anchor(&https.ca)).await);
}

#[tokio::test]
async fn off_mode_serves_the_dev_ca_leaf_and_not_a_minted_certificate() {
    let (https, https_certs) = crate::resolve_https_tls(None).unwrap();
    assert!(https.is_none() && https_certs.is_none());
    let dev = edge::shared_dev_ca().unwrap();

    let tls = crate::resolve_player_tls(None, None).unwrap();
    assert!(handshakes(tls, "localhost", &dev.trust_anchor()).await, "off keeps the dev CA leaf");
}

#[tokio::test]
async fn bad_player_files_fail_instead_of_falling_back() {
    let dir = fresh_dir("bad");
    let https = mint(&dir, "https.test");
    let (_, https_certs) = crate::resolve_https_tls(Some(&files_front(&https))).unwrap();
    let missing = PemFiles { cert: dir.join("missing-cert.pem"), key: https.key.clone() };

    for fallback in [https_certs, None] {
        let err = crate::resolve_player_tls(Some(&missing), fallback).err().expect("must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("player front certificate"), "{msg}");
        assert!(msg.contains("missing-cert.pem"), "{msg}");
    }
}

#[tokio::test]
async fn bad_https_files_fail_boot_resolution() {
    let dir = fresh_dir("bad-https");
    let err = crate::resolve_https_tls(Some(&TlsFront::Files {
        cert: dir.join("nope-cert.pem"),
        key: dir.join("nope-key.pem"),
    }))
    .err()
    .expect("must fail");
    assert!(format!("{err:#}").contains("nope-cert.pem"), "{err:#}");
}

#[tokio::test]
async fn acme_mode_player_front_refuses_handshakes_until_a_certificate_exists() {
    let front = TlsFront::Acme {
        domains: vec!["example.com".to_string()],
        cache_dir: fresh_dir("acme-cache"),
        contact: None,
    };
    let (https, certs) = crate::resolve_https_tls(Some(&front)).unwrap();
    assert!(matches!(https, Some(crate::HttpsTls::Acme { .. })));
    let certs = certs.expect("acme exposes its resolver to the player front");
    let tls = crate::resolve_player_tls(None, Some(certs)).unwrap();

    let dev = edge::shared_dev_ca().unwrap();
    let res = dial(tls, "localhost", &dev.trust_anchor()).await;
    match res {
        Err(edge::Error::Connection(text)) => {
            assert!(!text.contains("invalid peer certificate"), "the server presented a certificate: {text}")
        }
        other => panic!("an empty ACME resolver must fail the handshake, got {other:?}"),
    }
    drop(https);
}

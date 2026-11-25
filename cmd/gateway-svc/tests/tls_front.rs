//! The real gateway-svc binary (the split topology) under `TLS_MODE=files` and
//! `PLAYER_TLS_*`. Previously-wrong branch: the player QUIC front always served the dev
//! CA leaf (SAN `localhost`), so the handshakes below with the minted CA and the minted
//! name never complete and the dev-CA `localhost` handshake against a minted
//! certificate succeeds.
//!
//! Readiness is not asserted: with no peers up `/readyz` is red; only the QUIC
//! handshake is. The QUIC front binds after `Gateway::start`'s bounded peer refresh
//! (core/app/src/lib.rs, step 7b), seconds with every peer down, hence the long
//! bounded retry.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BOOT_HANG_GUARD: Duration = Duration::from_secs(60);
const EXIT_HANG_GUARD: Duration = Duration::from_secs(30);
const DIAL_ATTEMPT_GUARD: Duration = Duration::from_secs(5);
const DIAL_REJECT_GUARD: Duration = Duration::from_secs(30);
const PLAYER_NAME: &str = "player.test";
const HTTPS_NAME: &str = "https.test";

struct GatewayProcess {
    child: Child,
    log: PathBuf,
}

impl GatewayProcess {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_else(|e| format!("<unreadable log: {e}>"))
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("poll gateway child")
    }
}

impl Drop for GatewayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gateway-svc-tls-front-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mk temp dir");
    dir
}

struct Certs {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

/// A CA-signed leaf whose only SAN is `name` — never `localhost`, which is all a dev
/// CA leaf names.
fn mint_certs(dir: &Path, prefix: &str, name: &str) -> Certs {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
    let ca_key = KeyPair::generate().expect("ca key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name.push(DnType::CommonName, format!("gateway-svc tls_front {prefix} CA"));
    let ca = ca_params.self_signed(&ca_key).expect("ca cert");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let mut leaf_params = CertificateParams::new(vec![name.to_string()]).expect("leaf params");
    leaf_params.distinguished_name.push(DnType::CommonName, name);
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).expect("leaf cert");
    let certs = Certs {
        ca: dir.join(format!("{prefix}-ca.pem")),
        cert: dir.join(format!("{prefix}-cert.pem")),
        key: dir.join(format!("{prefix}-key.pem")),
    };
    std::fs::write(&certs.ca, ca.pem()).expect("write ca");
    std::fs::write(&certs.cert, leaf.pem()).expect("write cert");
    std::fs::write(&certs.key, leaf_key.serialize_pem()).expect("write key");
    certs
}

/// The process's dev CA, minted here so the test knows what the default leaf chains to.
fn write_dev_ca(dir: &Path) -> (PathBuf, PathBuf, edge::TrustAnchor) {
    let ca = edge::DevCA::generate().expect("dev ca");
    let (cert, key) = (dir.join("edge-ca.pem"), dir.join("edge-ca.key"));
    ca.write_pem(cert.to_str().unwrap(), key.to_str().unwrap()).expect("write dev ca");
    (cert, key, ca.trust_anchor())
}

fn anchor_of(certs: &Certs) -> edge::TrustAnchor {
    edge::DevCA::load_cert_only(certs.ca.to_str().unwrap()).expect("load minted ca")
}

async fn dial_once(
    addr: SocketAddr,
    name: &str,
    trust: &edge::TrustAnchor,
    guard: Duration,
) -> Result<(), edge::Error> {
    match tokio::time::timeout(guard, edge::PlayerClient::dial(addr, name, trust)).await {
        Ok(Ok(_client)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(edge::Error::Connect(format!("no answer within {guard:?}"))),
    }
}

/// The client refused the server's certificate: a handshake failure
/// (`Error::Connection`) naming the invalid peer certificate. A dial timeout
/// (`Error::Connect`) is not a rejection.
fn assert_certificate_rejected(res: Result<(), edge::Error>, what: &str, log: &str) {
    match res {
        Err(edge::Error::Connection(text)) => assert!(
            text.contains("invalid peer certificate"),
            "{what}: handshake failed but not on the certificate: {text}; log:
{log}"
        ),
        other => panic!("{what}: expected a certificate rejection, got {other:?}; log:
{log}"),
    }
}

async fn wait_for_handshake(
    gateway: &mut GatewayProcess,
    addr: SocketAddr,
    name: &str,
    trust: &edge::TrustAnchor,
) {
    let deadline = Instant::now() + BOOT_HANG_GUARD;
    let mut backoff = Duration::from_millis(100);
    loop {
        if let Some(exit) = gateway.exited() {
            panic!("gateway-svc exited before serving QUIC ({exit}); log:\n{}", gateway.log_text());
        }
        match dial_once(addr, name, trust, DIAL_ATTEMPT_GUARD).await {
            Ok(()) => return,
            Err(e) if Instant::now() >= deadline => panic!(
                "no QUIC handshake with {name:?} at {addr} within {BOOT_HANG_GUARD:?} (last error: {e}); log:\n{}",
                gateway.log_text()
            ),
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
            }
        }
    }
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("probe tcp port")
        .local_addr()
        .expect("tcp local addr")
        .port()
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("probe udp port")
        .local_addr()
        .expect("udp local addr")
        .port()
}

fn spawn_gateway(
    dir: &Path,
    mode: &str,
    tls_env: &[(&str, &Path)],
    quic_port: u16,
) -> GatewayProcess {
    let log = dir.join("gateway.log");
    let out = std::fs::File::create(&log).expect("create gateway log");
    let err = out.try_clone().expect("clone gateway log handle");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gateway-svc"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("TLS_")
            || key.starts_with("PLAYER_")
            || key.starts_with("RATE_LIMIT_")
            || key.starts_with("EDGE_CA_")
            || key.starts_with("ACME_")
            || key == "ORCHESTRATOR_URL"
        {
            cmd.env_remove(&key);
        }
    }
    cmd.env("TLS_MODE", mode)
        .env("PORT", format!("127.0.0.1:{}", free_tcp_port()))
        .env("EDGE_ADDR", format!("127.0.0.1:{}", free_udp_port()))
        .env("PLAYER_EDGE_ADDR", format!("127.0.0.1:{quic_port}"))
        .env("GATEWAY_DEV_VERIFIER", "1")
        .env("APIKEYS_DEV_ALLOW", "1");
    for (key, path) in tls_env {
        cmd.env(key, path);
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .expect("spawn gateway-svc binary");
    GatewayProcess { child, log }
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_svc_player_front_serves_the_tls_mode_files_certificate() {
    let dir = fresh_dir("files");
    let certs = mint_certs(&dir, "player", PLAYER_NAME);
    let (ca_cert, ca_key, dev_anchor) = write_dev_ca(&dir);
    let quic_port = free_udp_port();
    let mut gateway = spawn_gateway(
        &dir,
        "files",
        &[
            ("TLS_CERT_PATH", certs.cert.as_path()),
            ("TLS_KEY_PATH", certs.key.as_path()),
            ("EDGE_CA_CERT", ca_cert.as_path()),
            ("EDGE_CA_KEY", ca_key.as_path()),
        ],
        quic_port,
    );
    let addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    wait_for_handshake(&mut gateway, addr, PLAYER_NAME, &anchor_of(&certs)).await;

    let rejected = dial_once(addr, "localhost", &dev_anchor, DIAL_REJECT_GUARD).await;
    assert_certificate_rejected(rejected, "a dev CA anchor on the QUIC front under TLS_MODE=files", &gateway.log_text());
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_svc_player_tls_files_beat_the_https_certificate() {
    let dir = fresh_dir("precedence");
    let https = mint_certs(&dir, "https", HTTPS_NAME);
    let player = mint_certs(&dir, "player", PLAYER_NAME);
    let (ca_cert, ca_key, _) = write_dev_ca(&dir);
    let quic_port = free_udp_port();
    let mut gateway = spawn_gateway(
        &dir,
        "files",
        &[
            ("TLS_CERT_PATH", https.cert.as_path()),
            ("TLS_KEY_PATH", https.key.as_path()),
            ("PLAYER_TLS_CERT_PATH", player.cert.as_path()),
            ("PLAYER_TLS_KEY_PATH", player.key.as_path()),
            ("EDGE_CA_CERT", ca_cert.as_path()),
            ("EDGE_CA_KEY", ca_key.as_path()),
        ],
        quic_port,
    );
    let addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    wait_for_handshake(&mut gateway, addr, PLAYER_NAME, &anchor_of(&player)).await;

    let rejected = dial_once(addr, HTTPS_NAME, &anchor_of(&https), DIAL_REJECT_GUARD).await;
    assert_certificate_rejected(rejected, "the HTTPS certificate on the QUIC front although PLAYER_TLS_* is set", &gateway.log_text());
}

#[test]
fn gateway_svc_with_only_one_player_tls_path_fails_startup() {
    for (set, missing) in [
        ("PLAYER_TLS_CERT_PATH", "PLAYER_TLS_KEY_PATH"),
        ("PLAYER_TLS_KEY_PATH", "PLAYER_TLS_CERT_PATH"),
    ] {
        let dir = fresh_dir(&format!("half-{set}"));
        let certs = mint_certs(&dir, "player", PLAYER_NAME);
        let path = if set == "PLAYER_TLS_CERT_PATH" { &certs.cert } else { &certs.key };
        let mut gateway = spawn_gateway(&dir, "off", &[(set, path.as_path())], free_udp_port());

        let deadline = Instant::now() + EXIT_HANG_GUARD;
        let mut backoff = Duration::from_millis(50);
        let exit = loop {
            if let Some(exit) = gateway.exited() {
                break exit;
            }
            assert!(
                Instant::now() < deadline,
                "gateway-svc still running {EXIT_HANG_GUARD:?} after start with only {set}; log:\n{}",
                gateway.log_text()
            );
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(1));
        };
        let log = gateway.log_text();
        assert!(!exit.success(), "gateway-svc exited cleanly with only {set}; log:\n{log}");
        let expected = format!("{set} is set but {missing} is not");
        assert!(log.contains(&expected), "gateway-svc did not report {expected:?}; log:\n{log}");
    }
}

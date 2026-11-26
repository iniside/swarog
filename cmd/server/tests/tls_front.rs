//! The real monolith binary under `TLS_MODE=files`. Previously-wrong branch: before
//! `cmd/server` passed `TlsFronts::from_values` into `Config::with_tls`, the monolith
//! ignored `TLS_MODE` and served plain HTTP — with `.with_tls(tls)` removed, the HTTPS
//! poll below never succeeds and the test fails there (no HTTPS answer).
//!
//! The player QUIC front shares the process, so the same binary is also dialed on
//! `PLAYER_EDGE_ADDR`. Previously-wrong branch: before the player front chose its
//! certificate from `TLS_MODE` / `PLAYER_TLS_*` it always served the dev CA leaf
//! (SAN `localhost`), so the minted-CA handshakes below never complete and the dev-CA
//! `localhost` handshakes against a minted certificate succeed.
//!
//! `PORT` is a loopback `host:port` (`127.0.0.1:{port}`), so this also proves a
//! host:port `PORT` binds under TLS.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BOOT_HANG_GUARD: Duration = Duration::from_secs(60);
const EXIT_HANG_GUARD: Duration = Duration::from_secs(30);
const DIAL_ATTEMPT_GUARD: Duration = Duration::from_secs(5);
const DIAL_REJECT_GUARD: Duration = Duration::from_secs(30);
const PLAYER_NAME: &str = "player.test";
const DEV_CERT_WARNING: &str = "serves a dev certificate";

struct ServerProcess {
    child: Child,
    log: PathBuf,
}

impl ServerProcess {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_else(|e| format!("<unreadable log: {e}>"))
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("poll server child")
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("server-tls-front-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mk temp dir");
    dir
}

fn mint_localhost_cert(dir: &Path) -> (PathBuf, PathBuf, String) {
    let ck = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])
    .expect("mint cert");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let cert_pem = ck.cert.pem();
    std::fs::write(&cert_path, &cert_pem).expect("write cert");
    std::fs::write(&key_path, ck.key_pair.serialize_pem()).expect("write key");
    (cert_path, key_path, cert_pem)
}

struct PlayerCerts {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

/// A CA-signed leaf whose only SAN is `PLAYER_NAME` — not `localhost`, which is all a
/// dev CA leaf names.
fn mint_player_certs(dir: &Path) -> PlayerCerts {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
    let ca_key = KeyPair::generate().expect("ca key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name.push(DnType::CommonName, "server tls_front test CA");
    let ca = ca_params.self_signed(&ca_key).expect("ca cert");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let mut leaf_params = CertificateParams::new(vec![PLAYER_NAME.to_string()]).expect("leaf params");
    leaf_params.distinguished_name.push(DnType::CommonName, PLAYER_NAME);
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).expect("leaf cert");
    let certs = PlayerCerts {
        ca: dir.join("player-ca.pem"),
        cert: dir.join("player-cert.pem"),
        key: dir.join("player-key.pem"),
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

fn minted_anchor(certs: &PlayerCerts) -> edge::TrustAnchor {
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

/// Retries until the QUIC front completes a handshake: it binds only after boot, and
/// a dial before that gets no answer.
async fn wait_for_handshake(
    server: &mut ServerProcess,
    addr: SocketAddr,
    name: &str,
    trust: &edge::TrustAnchor,
) {
    let deadline = Instant::now() + BOOT_HANG_GUARD;
    let mut backoff = Duration::from_millis(100);
    loop {
        if let Some(exit) = server.exited() {
            panic!("server exited before serving QUIC ({exit}); log:\n{}", server.log_text());
        }
        match dial_once(addr, name, trust, DIAL_ATTEMPT_GUARD).await {
            Ok(()) => return,
            Err(e) if Instant::now() >= deadline => panic!(
                "no QUIC handshake with {name:?} at {addr} within {BOOT_HANG_GUARD:?} (last error: {e}); log:\n{}",
                server.log_text()
            ),
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
            }
        }
    }
}

fn assert_no_dev_certificate_warning(log: &str) {
    assert!(
        !log.contains(DEV_CERT_WARNING),
        "a configured player certificate must not log the dev-certificate warning; log:
{log}"
    );
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

fn spawn_server(
    dir: &Path,
    mode: &str,
    tls_env: &[(&str, &Path)],
    http_port: u16,
    quic_port: u16,
) -> ServerProcess {
    let log = dir.join("server.log");
    let out = std::fs::File::create(&log).expect("create server log");
    let err = out.try_clone().expect("clone server log handle");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_server"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("TLS_")
            || key.starts_with("PLAYER_")
            || key.starts_with("RATE_LIMIT_")
            || key.starts_with("EDGE_CA_")
            || key.starts_with("ACME_")
            || key.starts_with("MAIL_")
        {
            cmd.env_remove(&key);
        }
    }
    cmd.env("TLS_MODE", mode)
        .env("PORT", format!("127.0.0.1:{http_port}"))
        .env("PLAYER_EDGE_ADDR", format!("127.0.0.1:{quic_port}"))
        .env("DATABASE_URL", testdb::dsn())
        .env("ACCOUNTS_SIGNING_KEK", testdb::DEV_ACCOUNTS_SIGNING_KEK)
        .env("DATABASE_POOL_MAX_CONNECTIONS", "4")
        .env("MAIL_PROVIDER", "log")
        .env("MAIL_FROM", "dev@localhost");
    for (key, path) in tls_env {
        cmd.env(key, path);
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .expect("spawn server binary");
    ServerProcess { child, log }
}

#[tokio::test(flavor = "multi_thread")]
async fn monolith_serves_https_and_refuses_plain_http_under_tls_mode_files() {
    let Some(pool) = testdb::test_pool().await else {
        return;
    };
    drop(pool);

    let dir = fresh_dir("serve");
    let (cert, key, cert_pem) = mint_localhost_cert(&dir);
    let port = free_tcp_port();
    let mut server = spawn_server(
        &dir,
        "files",
        &[("TLS_CERT_PATH", cert.as_path()), ("TLS_KEY_PATH", key.as_path())],
        port,
        free_udp_port(),
    );

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let https = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert_pem.as_bytes()).expect("parse test ca"))
        .resolve("localhost", addr)
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build https client");
    let url = format!("https://localhost:{port}/healthz");

    let deadline = Instant::now() + BOOT_HANG_GUARD;
    let mut backoff = Duration::from_millis(50);
    let status = loop {
        if let Some(exit) = server.exited() {
            panic!("server exited before serving HTTPS ({exit}); log:\n{}", server.log_text());
        }
        match https.get(&url).send().await {
            Ok(resp) => break resp.status(),
            Err(e) if Instant::now() >= deadline => panic!(
                "no HTTPS answer at {url} within {BOOT_HANG_GUARD:?} (last error: {e:?}); log:\n{}",
                server.log_text()
            ),
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
            }
        }
    };
    assert_eq!(status, reqwest::StatusCode::OK, "log:\n{}", server.log_text());

    let plain = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build plain client")
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await;
    assert!(
        plain.is_err(),
        "plain http on the TLS port got an HTTP response ({:?}); log:\n{}",
        plain.map(|r| r.status()),
        server.log_text()
    );
}

#[test]
fn tls_mode_files_without_key_path_fails_startup() {
    let dir = fresh_dir("nokey");
    let (cert, _key, _) = mint_localhost_cert(&dir);
    let mut server = spawn_server(
        &dir,
        "files",
        &[("TLS_CERT_PATH", cert.as_path())],
        free_tcp_port(),
        free_udp_port(),
    );

    let deadline = Instant::now() + EXIT_HANG_GUARD;
    let mut backoff = Duration::from_millis(50);
    let exit = loop {
        if let Some(exit) = server.exited() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "server still running {EXIT_HANG_GUARD:?} after start with TLS_KEY_PATH unset; log:\n{}",
            server.log_text()
        );
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(1));
    };
    let log = server.log_text();
    assert!(!exit.success(), "server exited cleanly with TLS_KEY_PATH unset; log:\n{log}");
    assert!(
        log.contains("TLS_MODE=files requires TLS_KEY_PATH"),
        "server did not name the missing TLS_KEY_PATH; log:\n{log}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn player_front_serves_the_tls_mode_files_certificate_not_the_dev_ca_leaf() {
    let Some(pool) = testdb::test_pool().await else {
        return;
    };
    drop(pool);

    let dir = fresh_dir("quic-files");
    let certs = mint_player_certs(&dir);
    let (ca_cert, ca_key, dev_anchor) = write_dev_ca(&dir);
    let quic_port = free_udp_port();
    let mut server = spawn_server(
        &dir,
        "files",
        &[
            ("TLS_CERT_PATH", certs.cert.as_path()),
            ("TLS_KEY_PATH", certs.key.as_path()),
            ("EDGE_CA_CERT", ca_cert.as_path()),
            ("EDGE_CA_KEY", ca_key.as_path()),
        ],
        free_tcp_port(),
        quic_port,
    );
    let addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    wait_for_handshake(&mut server, addr, PLAYER_NAME, &minted_anchor(&certs)).await;

    let rejected = dial_once(addr, "localhost", &dev_anchor, DIAL_REJECT_GUARD).await;
    assert_certificate_rejected(rejected, "a dev CA anchor on the QUIC front under TLS_MODE=files", &server.log_text());
    assert_no_dev_certificate_warning(&server.log_text());
}

#[tokio::test(flavor = "multi_thread")]
async fn player_tls_files_serve_on_quic_even_with_tls_mode_off() {
    let Some(pool) = testdb::test_pool().await else {
        return;
    };
    drop(pool);

    let dir = fresh_dir("quic-player-files");
    let certs = mint_player_certs(&dir);
    let (ca_cert, ca_key, dev_anchor) = write_dev_ca(&dir);
    let quic_port = free_udp_port();
    let mut server = spawn_server(
        &dir,
        "off",
        &[
            ("PLAYER_TLS_CERT_PATH", certs.cert.as_path()),
            ("PLAYER_TLS_KEY_PATH", certs.key.as_path()),
            ("EDGE_CA_CERT", ca_cert.as_path()),
            ("EDGE_CA_KEY", ca_key.as_path()),
        ],
        free_tcp_port(),
        quic_port,
    );
    let addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    wait_for_handshake(&mut server, addr, PLAYER_NAME, &minted_anchor(&certs)).await;

    let rejected = dial_once(addr, "localhost", &dev_anchor, DIAL_REJECT_GUARD).await;
    assert_certificate_rejected(rejected, "a dev CA anchor on the QUIC front with PLAYER_TLS_* set", &server.log_text());
    assert_no_dev_certificate_warning(&server.log_text());
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_mode_off_without_player_files_keeps_the_dev_ca_leaf() {
    let Some(pool) = testdb::test_pool().await else {
        return;
    };
    drop(pool);

    let dir = fresh_dir("quic-off");
    let (ca_cert, ca_key, dev_anchor) = write_dev_ca(&dir);
    let quic_port = free_udp_port();
    let mut server = spawn_server(
        &dir,
        "off",
        &[("EDGE_CA_CERT", ca_cert.as_path()), ("EDGE_CA_KEY", ca_key.as_path())],
        free_tcp_port(),
        quic_port,
    );
    let addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    wait_for_handshake(&mut server, addr, "localhost", &dev_anchor).await;

    let log = server.log_text();
    assert_eq!(
        log.matches(DEV_CERT_WARNING).count(),
        1,
        "TLS_MODE=off with no PLAYER_TLS_* must log the dev-certificate warning exactly once; log:
{log}"
    );
}

#[test]
fn only_one_player_tls_path_fails_startup() {
    for (set, missing) in [
        ("PLAYER_TLS_CERT_PATH", "PLAYER_TLS_KEY_PATH"),
        ("PLAYER_TLS_KEY_PATH", "PLAYER_TLS_CERT_PATH"),
    ] {
        let dir = fresh_dir(&format!("player-half-{set}"));
        let certs = mint_player_certs(&dir);
        let path = if set == "PLAYER_TLS_CERT_PATH" { &certs.cert } else { &certs.key };
        let mut server =
            spawn_server(&dir, "off", &[(set, path.as_path())], free_tcp_port(), free_udp_port());

        let deadline = Instant::now() + EXIT_HANG_GUARD;
        let mut backoff = Duration::from_millis(50);
        let exit = loop {
            if let Some(exit) = server.exited() {
                break exit;
            }
            assert!(
                Instant::now() < deadline,
                "server still running {EXIT_HANG_GUARD:?} after start with only {set}; log:\n{}",
                server.log_text()
            );
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(1));
        };
        let log = server.log_text();
        assert!(!exit.success(), "server exited cleanly with only {set}; log:\n{log}");
        let expected = format!("{set} is set but {missing} is not");
        assert!(log.contains(&expected), "server did not report {expected:?}; log:\n{log}");
    }
}

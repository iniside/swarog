//! The real monolith binary under `TLS_MODE=files`. Previously-wrong branch: before
//! `cmd/server` passed `TlsFront::from_values` into `Config::with_tls`, the monolith
//! ignored `TLS_MODE` and served plain HTTP — with `.with_tls(tls)` removed, the HTTPS
//! poll below never succeeds and the test fails there (no HTTPS answer).
//!
//! `PORT` is a loopback `host:port` (`127.0.0.1:{port}`), so this also proves a
//! host:port `PORT` binds under TLS (it used to be mangled into `:127.0.0.1:{port}`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BOOT_HANG_GUARD: Duration = Duration::from_secs(60);
const EXIT_HANG_GUARD: Duration = Duration::from_secs(30);

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

fn spawn_server(dir: &Path, tls_env: &[(&str, &Path)], http_port: u16) -> ServerProcess {
    let log = dir.join("server.log");
    let out = std::fs::File::create(&log).expect("create server log");
    let err = out.try_clone().expect("clone server log handle");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_server"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("TLS_") || key.starts_with("ACME_") || key.starts_with("MAIL_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("TLS_MODE", "files")
        .env("PORT", format!("127.0.0.1:{http_port}"))
        .env("PLAYER_EDGE_ADDR", format!("127.0.0.1:{}", free_udp_port()))
        .env("DATABASE_URL", testdb::dsn())
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
        &[("TLS_CERT_PATH", cert.as_path()), ("TLS_KEY_PATH", key.as_path())],
        port,
    );

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
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
    let mut server = spawn_server(&dir, &[("TLS_CERT_PATH", cert.as_path())], free_tcp_port());

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

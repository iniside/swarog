use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use edge::{pem_cert_resolver, PlayerServer, PlayerTlsConfig, RunningServer};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the built `playercli` with `args`, killing it if it outlives a generous guard.
async fn playercli(args: Vec<String>) -> Run {
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playercli"))
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("playercli {args:?} did not exit within the hang guard");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .unwrap()
}

/// A CA and one leaf it signed for `names` only (no IP SANs), written as PEM files.
struct Minted {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn mint(tag: &str, names: &[&str]) -> Minted {
    let dir = std::env::temp_dir().join(format!("playercli-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
        .unwrap()
        .signed_by(&leaf_key, &ca, &ca_key)
        .unwrap();
    let minted = Minted { ca: dir.join("ca.pem"), cert: dir.join("cert.pem"), key: dir.join("key.pem") };
    std::fs::write(&minted.ca, ca.pem()).unwrap();
    std::fs::write(&minted.cert, leaf.pem()).unwrap();
    std::fs::write(&minted.key, leaf_key.serialize_pem()).unwrap();
    minted
}

/// A player server presenting `certs`' leaf whose every call answers `{"status":"Ok"}`.
fn serve(addr: SocketAddr, certs: &Minted) -> RunningServer {
    let srv = PlayerServer::new();
    srv.set_handler(Arc::new(|_method, _token, _key, _payload| {
        Box::pin(async move { Ok(br#"{"status":"Ok"}"#.to_vec()) })
    }));
    srv.listen(addr, PlayerTlsConfig::new(pem_cert_resolver(&certs.cert, &certs.key).unwrap()).unwrap())
        .unwrap()
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| a.to_string()).collect()
}

/// One server per address `localhost` resolves to, all on one port, in resolver order.
fn serve_every_localhost_address(certs: impl Fn(usize) -> Arc<Minted>) -> (u16, Vec<SocketAddr>, Vec<RunningServer>) {
    let first: SocketAddr = ("localhost", 0).to_socket_addrs().unwrap().next().unwrap();
    let mut running = vec![serve(first, &certs(0))];
    let port = running[0].local_addr().port();
    let addrs: Vec<SocketAddr> = ("localhost", port).to_socket_addrs().unwrap().collect();
    assert!(addrs.len() >= 2, "localhost must resolve to at least two addresses for this test, got {addrs:?}");
    assert_eq!(addrs[0], running[0].local_addr());
    for (index, addr) in addrs.iter().enumerate().skip(1) {
        running.push(serve(*addr, &certs(index)));
    }
    (port, addrs, running)
}

/// The stderr line playercli printed for the resolved `addr` of a DNS host; the socket
/// address must appear in it once.
fn failure_line(stderr: &str, prefix: &str, addr: SocketAddr) {
    let lines: Vec<&str> = stderr.lines().filter(|l| l.contains(&addr.to_string())).collect();
    assert_eq!(lines.len(), 1, "exactly one stderr line must name {addr}; stderr:\n{stderr}");
    let line = lines[0];
    assert!(line.starts_with(&format!("{prefix}{addr}: ")), "line for {addr}: {line}");
    assert_eq!(line.matches(&addr.to_string()).count(), 1, "{addr} must appear once: {line}");
    assert!(line.contains("invalid peer certificate"), "{addr} must fail on the certificate: {line}");
}

#[tokio::test]
async fn trust_is_exactly_one_of_ca_or_system_roots() {
    for trust in [&[][..], &["--ca", "ca.pem", "--system-roots"][..]] {
        let mut list = args(&["--addr", "127.0.0.1:9"]);
        list.extend(args(trust));
        list.push("echo".to_string());
        let run = playercli(list).await;
        assert_eq!(run.code, Some(2), "{trust:?}: {}", run.stderr);
        assert!(run.stderr.contains("exactly one of --ca or --system-roots is required"), "{trust:?}: {}", run.stderr);
    }

    let run = playercli(args(&["--addr", "127.0.0.1:9", "--system-roots"])).await;
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("a <method> argument is required"), "{}", run.stderr);

    let run = playercli(args(&["--addr", "localhost", "--system-roots", "echo"])).await;
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("bad --addr \"localhost\": expected host:port"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ip_literal_addr_verifies_the_certificate_as_localhost() {
    let certs = mint("ip-literal", &["localhost"]);
    for listen in ["127.0.0.1:0", "[::1]:0"] {
        let running = serve(listen.parse().unwrap(), &certs);
        let addr = running.local_addr().to_string();
        let run = playercli(args(&["--addr", &addr, "--ca", &path(&certs.ca), "echo"])).await;
        assert_eq!(run.code, Some(0), "{addr}: {}", run.stderr);
        assert_eq!(run.stdout.trim(), r#"{"status":"Ok"}"#);
        running.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_name_flag_replaces_the_default_name() {
    let certs = mint("server-name", &["player.test"]);
    let running = serve("127.0.0.1:0".parse().unwrap(), &certs);
    let addr = running.local_addr().to_string();

    let run = playercli(args(&["--addr", &addr, "--ca", &path(&certs.ca), "echo"])).await;
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(run.stderr.starts_with(&format!("playercli: dial {addr}: {addr}: ")), "{}", run.stderr);
    assert!(run.stderr.contains("invalid peer certificate"), "{}", run.stderr);

    let run = playercli(args(&["--addr", &addr, "--ca", &path(&certs.ca), "--server-name", "player.test", "echo"])).await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"status":"Ok"}"#);
    running.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_roots_flag_rejects_a_self_made_ca() {
    let certs = mint("system-roots", &["localhost"]);
    let running = serve("127.0.0.1:0".parse().unwrap(), &certs);
    let addr = running.local_addr();

    let run = playercli(args(&["--addr", &addr.to_string(), "--ca", &path(&certs.ca), "echo"])).await;
    assert_eq!(run.code, Some(0), "control: the minted CA accepts its leaf: {}", run.stderr);

    let run = playercli(args(&["--addr", &addr.to_string(), "--system-roots", "echo"])).await;
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(run.stderr.starts_with(&format!("playercli: dial {addr}: {addr}: ")), "{}", run.stderr);
    assert!(run.stderr.contains("invalid peer certificate"), "{}", run.stderr);
    running.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_failed_address_of_a_dns_host_is_reported() {
    let served = Arc::new(mint("all-fail-served", &["localhost"]));
    let trusted = mint("all-fail-trusted", &["localhost"]);
    let (port, addrs, running) = serve_every_localhost_address(|_| served.clone());
    let host = format!("localhost:{port}");

    let run = playercli(args(&["--addr", &host, "--ca", &path(&trusted.ca), "echo"])).await;
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    for addr in &addrs {
        failure_line(&run.stderr, &format!("playercli: dial {host}: "), *addr);
    }
    assert!(!run.stderr.contains("warning"), "{}", run.stderr);
    for r in running {
        r.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_address_connecting_turns_earlier_failures_into_warnings() {
    let untrusted = Arc::new(mint("warn-untrusted", &["localhost"]));
    let trusted = Arc::new(mint("warn-trusted", &["localhost"]));
    let (port, addrs, running) =
        serve_every_localhost_address(|index| if index == 0 { untrusted.clone() } else { trusted.clone() });
    let host = format!("localhost:{port}");

    let run = playercli(args(&["--addr", &host, "--ca", &path(&trusted.ca), "echo"])).await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"status":"Ok"}"#);
    failure_line(&run.stderr, &format!("playercli: warning: dial {host}: "), addrs[0]);
    for addr in &addrs[1..] {
        assert!(!run.stderr.contains(&addr.to_string()), "{addr} connected and must not be reported: {}", run.stderr);
    }
    for r in running {
        r.close();
    }
}

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use edge::{pem_cert_resolver, PlayerHandler, PlayerServer, PlayerTlsConfig, RunningServer};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the built `playercli` with `args`, killing it if it outlives a generous guard.
async fn playercli(args: Vec<String>) -> Run {
    playercli_env(args, Vec::new()).await
}

/// [`playercli`] with `env` added to the inherited environment.
async fn playercli_env(args: Vec<String>, env: Vec<(&'static str, PathBuf)>) -> Run {
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playercli"))
            .args(&args)
            .envs(env)
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
    // rcgen gives every cert the same default subject (certificate.rs:98); distinct names
    // keep the leaf from being self-issued (issuer == subject, RFC 5280 6.1).
    ca_params.distinguished_name.push(DnType::CommonName, "playercli test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params =
        CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    leaf_params.distinguished_name.push(DnType::CommonName, "playercli test leaf");
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
    let minted = Minted { ca: dir.join("ca.pem"), cert: dir.join("cert.pem"), key: dir.join("key.pem") };
    std::fs::write(&minted.ca, ca.pem()).unwrap();
    std::fs::write(&minted.cert, leaf.pem()).unwrap();
    std::fs::write(&minted.key, leaf_key.serialize_pem()).unwrap();
    minted
}

/// A player server presenting `certs`' leaf whose every call answers `{"status":"Ok"}`.
fn serve(addr: SocketAddr, certs: &Minted) -> RunningServer {
    serve_with(addr, certs, Arc::new(|_method, _token, _key, _payload| {
        Box::pin(async move { Ok(br#"{"status":"Ok"}"#.to_vec()) })
    }))
}

fn serve_with(addr: SocketAddr, certs: &Minted, handler: PlayerHandler) -> RunningServer {
    let srv = PlayerServer::new();
    srv.set_handler(handler);
    srv.listen(addr, PlayerTlsConfig::new(pem_cert_resolver(&certs.cert, &certs.key).unwrap()).unwrap())
        .unwrap()
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| a.to_string()).collect()
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
    // The macOS verifier's wording has not been observed.
    if cfg!(any(windows, target_os = "linux")) {
        assert!(run.stderr.contains("UnknownIssuer"), "{}", run.stderr);
    }
    running.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omitted_payload_is_sent_as_an_empty_object() {
    let certs = mint("payload", &["localhost"]);
    let running = serve_with(
        "127.0.0.1:0".parse().unwrap(),
        &certs,
        Arc::new(|_method, _token, _key, payload| {
            Box::pin(async move {
                let payload = String::from_utf8(payload).unwrap();
                Ok(format!(r#"{{"status":"Ok","payload":{payload:?}}}"#).into_bytes())
            })
        }),
    );
    let addr = running.local_addr().to_string();

    let run = playercli(args(&["--addr", &addr, "--ca", &path(&certs.ca), "echo"])).await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"status":"Ok","payload":"{}"}"#);

    let run = playercli(args(&["--addr", &addr, "--ca", &path(&certs.ca), "echo", "[1]"])).await;
    assert_eq!(run.code, Some(0), "control: a given payload is sent as given: {}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"status":"Ok","payload":"[1]"}"#);
    running.close();
}

/// On Linux the system store is read through `rustls-native-certs`, which takes
/// `SSL_CERT_FILE` in place of the distribution's bundle, so the minted CA stands in
/// for a public root.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_roots_flag_accepts_a_leaf_under_a_system_root() {
    let certs = mint("system-roots-accept", &["localhost"]);
    let running = serve("127.0.0.1:0".parse().unwrap(), &certs);
    let addr = running.local_addr().to_string();
    let run = playercli_env(args(&["--addr", &addr, "--system-roots", "echo"]), vec![("SSL_CERT_FILE", certs.ca.clone())]).await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"status":"Ok"}"#);
    running.close();
}

use super::*;
use crate::player::{PlayerClient, PlayerServer};
use std::time::Duration;

#[test]
fn generate_builds_configs() {
    let ca = DevCA::generate().unwrap();
    let s = ca.server_tls().unwrap();
    assert_eq!(s.alpn_protocols, vec![ALPN.to_vec()]);
    let c = ca.client_tls().unwrap();
    assert_eq!(c.alpn_protocols, vec![ALPN.to_vec()]);
}

#[test]
fn write_then_load_roundtrips_and_chains() {
    let dir = std::env::temp_dir();
    let cert = dir.join(format!("edgeca-test-{}.crt", std::process::id()));
    let key = dir.join(format!("edgeca-test-{}.key", std::process::id()));
    let ca = DevCA::generate().unwrap();
    ca.write_pem(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();

    let loaded = DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();
    // The loaded CA can still mint leaves and build both configs — proof the
    // key/cert round-tripped and the issuer reconstructed.
    loaded.server_tls().unwrap();
    loaded.client_tls().unwrap();

    let _ = std::fs::remove_file(cert);
    let _ = std::fs::remove_file(key);
}

#[test]
fn public_configs_carry_the_player_alpn() {
    let ca = DevCA::generate().unwrap();
    let s = ca.server_tls_public().unwrap();
    assert_eq!(s.server_config().alpn_protocols, vec![PLAYER_ALPN.to_vec()]);
    let c = ca.client_tls_public().unwrap();
    assert_eq!(c.alpn_protocols, vec![PLAYER_ALPN.to_vec()]);
    // The two planes must never share an ALPN id.
    assert_ne!(ALPN, PLAYER_ALPN);
}

#[test]
fn load_cert_only_builds_a_client_config_without_the_key() {
    let dir = std::env::temp_dir();
    let cert = dir.join(format!("edgeca-anchor-{}.crt", std::process::id()));
    let key = dir.join(format!("edgeca-anchor-{}.key", std::process::id()));
    DevCA::generate()
        .unwrap()
        .write_pem(cert.to_str().unwrap(), key.to_str().unwrap())
        .unwrap();
    // Delete the key BEFORE loading — proof the anchor path never touches it.
    std::fs::remove_file(&key).unwrap();

    let anchor = DevCA::load_cert_only(cert.to_str().unwrap()).unwrap();
    let cfg = anchor.client_tls_public().unwrap();
    assert_eq!(cfg.alpn_protocols, vec![PLAYER_ALPN.to_vec()]);

    let _ = std::fs::remove_file(cert);
}

struct Minted {
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
}

fn mint(name: &str) -> Minted {
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
    Minted { ca_pem: ca.pem(), cert_pem: leaf.pem(), key_pem: leaf_key.serialize_pem() }
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("edge-player-tls-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_all(dir: &Path, m: &Minted) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let (ca, cert, key) = (dir.join("ca.pem"), dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&ca, &m.ca_pem).unwrap();
    std::fs::write(&cert, &m.cert_pem).unwrap();
    std::fs::write(&key, &m.key_pem).unwrap();
    (ca, cert, key)
}

fn anchor(ca: &Path) -> TrustAnchor {
    DevCA::load_cert_only(ca.to_str().unwrap()).unwrap()
}

fn resolver_config(m: &Minted, tag: &str) -> PlayerTlsConfig {
    let dir = fresh_dir(tag);
    let (_, cert, key) = write_all(&dir, m);
    PlayerTlsConfig::new(pem_cert_resolver(&cert, &key).unwrap()).unwrap()
}

fn transfer(from: &mut rustls::Connection, to: &mut rustls::Connection) -> Result<(), String> {
    let mut buf = Vec::new();
    while from.wants_write() {
        from.write_tls(&mut buf).map_err(|e| e.to_string())?;
    }
    if !buf.is_empty() {
        to.read_tls(&mut &buf[..]).map_err(|e| e.to_string())?;
        to.process_new_packets().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// In-memory TLS handshake; returns the ALPN and protocol version the client negotiated.
fn handshake(
    server: Arc<ServerConfig>,
    client: ClientConfig,
    name: &str,
) -> Result<(Option<Vec<u8>>, Option<rustls::ProtocolVersion>), String> {
    let server_name = rustls::pki_types::ServerName::try_from(name.to_string()).unwrap();
    let mut c: rustls::Connection =
        rustls::ClientConnection::new(Arc::new(client), server_name).unwrap().into();
    let mut s: rustls::Connection = rustls::ServerConnection::new(server).unwrap().into();
    for _ in 0..16 {
        if !c.is_handshaking() && !s.is_handshaking() {
            return Ok((c.alpn_protocol().map(<[u8]>::to_vec), c.protocol_version()));
        }
        transfer(&mut c, &mut s)?;
        transfer(&mut s, &mut c)?;
    }
    Err("handshake did not finish".to_string())
}

fn tls12_only_client(roots: RootCertStore) -> ClientConfig {
    let mut cfg = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![PLAYER_ALPN.to_vec()];
    cfg
}

/// The same certificate behind a server that also speaks TLS 1.2: the TLS 1.2 client
/// completes against it, so a failure against the player config is the version alone.
fn tls12_server(certs: Arc<dyn ResolvesServerCert>) -> Arc<ServerConfig> {
    let mut cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(certs);
    cfg.alpn_protocols = vec![PLAYER_ALPN.to_vec()];
    Arc::new(cfg)
}

fn assert_player_invariants(
    cfg: &PlayerTlsConfig,
    client: ClientConfig,
    tls12_client: ClientConfig,
    tls12_control: (Arc<ServerConfig>, ClientConfig),
    name: &str,
) {
    let server = cfg.server_config();
    assert_eq!(server.alpn_protocols, vec![PLAYER_ALPN.to_vec()]);
    assert_eq!(server.max_early_data_size, 0, "0-RTT must stay off");
    let (alpn, version) = handshake(server.clone(), client, name)
        .expect("a TLS 1.3 client presenting no client certificate completes the handshake");
    assert_eq!(alpn.as_deref(), Some(PLAYER_ALPN));
    assert_eq!(version, Some(rustls::ProtocolVersion::TLSv1_3));

    let (control_server, control_client) = tls12_control;
    let (_, control_version) = handshake(control_server, control_client, name)
        .expect("the TLS 1.2 client must complete against a server that allows TLS 1.2");
    assert_eq!(control_version, Some(rustls::ProtocolVersion::TLSv1_2));
    assert!(
        handshake(server, tls12_client, name).is_err(),
        "a TLS 1.2-only client must not complete a handshake with the player config"
    );
}

#[test]
fn resolver_backed_player_config_enforces_the_player_invariants() {
    let m = mint("player.test");
    let cfg = resolver_config(&m, "invariants-resolver");
    let dir = fresh_dir("invariants-resolver-ca");
    let (ca, _, _) = write_all(&dir, &m);
    let client = anchor(&ca).client_tls_public().unwrap();
    let roots = |ca: &Path| {
        let mut store = RootCertStore::empty();
        store.add(CertificateDer::from_pem_slice(&std::fs::read(ca).unwrap()).unwrap()).unwrap();
        store
    };
    let certs = pem_cert_resolver(&dir.join("cert.pem"), &dir.join("key.pem")).unwrap();
    assert_player_invariants(
        &cfg,
        client,
        tls12_only_client(roots(&ca)),
        (tls12_server(certs), tls12_only_client(roots(&ca))),
        "player.test",
    );
}

#[test]
fn dev_ca_player_config_enforces_the_player_invariants() {
    let ca = DevCA::generate().unwrap();
    let cfg = ca.server_tls_public().unwrap();
    let (chain, key) = ca.leaf(true).unwrap();
    let certified = CertifiedKey::from_der(chain, key, &ca.provider).unwrap();
    let control = tls12_server(Arc::new(SingleCertAndKey::from(certified)));
    assert_player_invariants(
        &cfg,
        ca.client_tls_public().unwrap(),
        tls12_only_client((*ca.roots).clone()),
        (control, tls12_only_client((*ca.roots).clone())),
        "localhost",
    );
}

#[test]
fn pem_cert_resolver_loads_a_good_pair() {
    let m = mint("player.test");
    let dir = fresh_dir("good-pair");
    let (_, cert, key) = write_all(&dir, &m);
    assert!(pem_cert_resolver(&cert, &key).is_ok());
}

#[test]
fn pem_cert_resolver_errors_name_the_path() {
    let m = mint("player.test");
    let other = mint("other.test");
    let dir = fresh_dir("errors");
    let (_, cert, key) = write_all(&dir, &m);
    let missing = dir.join("missing.pem");
    let err = |c: &Path, k: &Path| pem_cert_resolver(c, k).err().expect("must fail").to_string();

    let msg = err(&missing, &key);
    assert!(msg.contains(&format!("{missing:?}")), "missing cert: {msg}");
    let msg = err(&cert, &missing);
    assert!(msg.contains(&format!("{missing:?}")), "missing key: {msg}");

    let empty = dir.join("empty.pem");
    std::fs::write(&empty, "").unwrap();
    let msg = err(&empty, &key);
    assert!(msg.contains(&format!("{empty:?}")) && msg.contains("no PEM CERTIFICATE"), "empty chain: {msg}");

    let other_key = dir.join("other-key.pem");
    std::fs::write(&other_key, &other.key_pem).unwrap();
    let msg = err(&cert, &other_key);
    assert!(msg.contains(&format!("{other_key:?}")), "key/cert mismatch: {msg}");

    let two_keys = dir.join("two-keys.pem");
    std::fs::write(&two_keys, format!("{}{}", m.key_pem, other.key_pem)).unwrap();
    let msg = err(&cert, &two_keys);
    assert!(msg.contains(&format!("{two_keys:?}")) && msg.contains("holds 2 private keys"), "two keys: {msg}");

    let msg = err(&cert, &empty);
    assert!(msg.contains(&format!("{empty:?}")) && msg.contains("holds 0 private keys"), "no key: {msg}");
}

/// A dial that failed because the client refused the server's certificate: a handshake
/// failure (`Error::Connection`), not a dial timeout (`Error::Connect`).
fn assert_certificate_rejected<T>(res: Result<T, Error>, what: &str) {
    match res {
        Err(Error::Connection(text)) => assert!(
            text.contains("invalid peer certificate"),
            "{what}: handshake failed but not on the certificate: {text}"
        ),
        Err(other) => panic!("{what}: expected a certificate rejection, got {other:?}"),
        Ok(_) => panic!("{what}: the dial succeeded"),
    }
}

#[tokio::test]
async fn resolver_backed_player_server_presents_the_resolvers_certificate() {
    let m = mint("player.test");
    let dir = fresh_dir("quic-serves");
    let (ca, _, _) = write_all(&dir, &m);
    let running = PlayerServer::new()
        .listen("127.0.0.1:0".parse().unwrap(), resolver_config(&m, "quic-serves-cfg"))
        .unwrap();
    let addr = running.local_addr();
    let guard = Duration::from_secs(30);

    tokio::time::timeout(guard, PlayerClient::dial(addr, "player.test", &anchor(&ca)))
        .await
        .expect("dial hang-guard")
        .expect("trusting the minted CA and the cert's name must handshake");

    let wrong_name = tokio::time::timeout(guard, PlayerClient::dial(addr, "other.test", &anchor(&ca)))
        .await
        .expect("dial hang-guard");
    assert_certificate_rejected(wrong_name, "a server name outside the cert's SANs");

    let dev = DevCA::generate().unwrap().trust_anchor();
    let wrong_anchor = tokio::time::timeout(guard, PlayerClient::dial(addr, "player.test", &dev))
        .await
        .expect("dial hang-guard");
    assert_certificate_rejected(wrong_anchor, "a DevCA anchor accepting the minted certificate");
    running.close();
}

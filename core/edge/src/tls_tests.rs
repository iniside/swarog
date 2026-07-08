use super::*;

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
    assert_eq!(s.alpn_protocols, vec![PLAYER_ALPN.to_vec()]);
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

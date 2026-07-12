use super::mint_dev_ca;

#[test]
fn key_is_private_and_atomically_replaceable() {
    let directory = std::env::temp_dir().join(format!(
        "edgeca-private-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let cert = directory.join("edge-ca.crt");
    let key = directory.join("edge-ca.key");
    mint_dev_ca(&cert, &key).unwrap();
    processctl::validate_private_path(&key).unwrap();
    edge::DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();

    mint_dev_ca(&cert, &key).unwrap();
    processctl::validate_private_path(&key).unwrap();
    edge::DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();
}

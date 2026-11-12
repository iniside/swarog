use super::*;

#[test]
fn ip_literal_addr_dials_its_one_address_as_localhost() {
    let (addrs, name) = resolve("127.0.0.1:9100").unwrap();
    assert_eq!(addrs, vec!["127.0.0.1:9100".parse::<SocketAddr>().unwrap()]);
    assert_eq!(name, "localhost");

    let (addrs, name) = resolve("[::1]:9100").unwrap();
    assert_eq!(addrs, vec!["[::1]:9100".parse::<SocketAddr>().unwrap()]);
    assert_eq!(name, "localhost");
}

#[test]
fn dns_host_addr_names_the_host_and_keeps_every_resolved_address() {
    let (addrs, name) = resolve("LocalHost:9100").unwrap();
    assert_eq!(name, "LocalHost", "the default server name must be the host as given, not a fixed localhost");
    let expected: Vec<SocketAddr> = ("localhost", 9100).to_socket_addrs().unwrap().collect();
    assert_eq!(addrs, expected, "every resolved address, in resolver order");
    assert!(addrs.iter().all(|a| a.ip().is_loopback() && a.port() == 9100), "{addrs:?}");
}

#[test]
fn addr_without_a_port_is_refused() {
    assert_eq!(resolve("localhost").unwrap_err(), "expected host:port");
}

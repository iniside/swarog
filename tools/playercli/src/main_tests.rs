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

fn addrs(ports: &[u16]) -> Vec<SocketAddr> {
    ports.iter().map(|port| SocketAddr::from(([127, 0, 0, 1], *port))).collect()
}

/// Dials `socket_addrs` through `dial_each` with a fake that connects (to its port)
/// exactly on the ports in `connects` and refuses every other one; returns the
/// outcome and every address the fake was asked to dial, in order.
async fn dial_fake(
    socket_addrs: Vec<SocketAddr>,
    connects: &[u16],
) -> (Result<(u16, Vec<String>), Vec<String>>, Vec<SocketAddr>) {
    let mut dialed = Vec::new();
    let outcome = dial_each(socket_addrs, |socket_addr| {
        dialed.push(socket_addr);
        let connected = connects.contains(&socket_addr.port());
        async move {
            if connected {
                Ok::<u16, String>(socket_addr.port())
            } else {
                Err(format!("refused {}", socket_addr.port()))
            }
        }
    })
    .await;
    (outcome, dialed)
}

#[tokio::test]
async fn first_address_connecting_dials_no_other() {
    let (outcome, dialed) = dial_fake(addrs(&[1, 2, 3]), &[1, 2, 3]).await;
    assert_eq!(outcome, Ok((1, Vec::new())));
    assert_eq!(dialed, addrs(&[1]), "a connected address must end the loop");
}

#[tokio::test]
async fn later_address_connecting_returns_earlier_failures_in_order() {
    let (outcome, dialed) = dial_fake(addrs(&[1, 2, 3, 4]), &[3, 4]).await;
    assert_eq!(outcome, Ok((3, vec!["127.0.0.1:1: refused 1".to_string(), "127.0.0.1:2: refused 2".to_string()])));
    assert_eq!(dialed, addrs(&[1, 2, 3]), "addresses after the connected one must not be dialed");
}

#[tokio::test]
async fn every_address_failing_returns_every_failure_in_order() {
    let (outcome, dialed) = dial_fake(addrs(&[3, 1, 2]), &[]).await;
    assert_eq!(
        outcome,
        Err(vec![
            "127.0.0.1:3: refused 3".to_string(),
            "127.0.0.1:1: refused 1".to_string(),
            "127.0.0.1:2: refused 2".to_string(),
        ])
    );
    assert_eq!(dialed, addrs(&[3, 1, 2]));
}

#[tokio::test]
async fn no_address_is_an_empty_failure() {
    let (outcome, dialed) = dial_fake(Vec::new(), &[1]).await;
    assert_eq!(outcome, Err(Vec::new()));
    assert!(dialed.is_empty());
}

#[test]
fn dial_target_names_a_dns_host_as_spelled_unless_overridden() {
    let expected: Vec<SocketAddr> = ("localhost", 9100).to_socket_addrs().unwrap().collect();
    assert_eq!(dial_target("LocalHost:9100", None).unwrap(), (expected.clone(), "LocalHost".to_string()));
    assert_eq!(
        dial_target("LocalHost:9100", Some("player.test".to_string())).unwrap(),
        (expected, "player.test".to_string())
    );
}

#[test]
fn dial_target_names_an_ip_literal_localhost_unless_overridden() {
    let addrs = vec!["127.0.0.1:9100".parse::<SocketAddr>().unwrap()];
    assert_eq!(dial_target("127.0.0.1:9100", None).unwrap(), (addrs.clone(), "localhost".to_string()));
    assert_eq!(
        dial_target("127.0.0.1:9100", Some("player.test".to_string())).unwrap(),
        (addrs, "player.test".to_string())
    );
}

#[test]
fn dial_target_refuses_a_bad_addr_even_with_a_server_name() {
    assert_eq!(dial_target("localhost", Some("player.test".to_string())).unwrap_err(), "expected host:port");
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn dial_report_warns_on_failures_before_a_connect() {
    let (client, lines) = dial_report("play.test:9100", Ok((7u16, strings(&["[::1]:9100: refused", "127.0.0.1:9100: timed out"]))));
    assert_eq!(client, Some(7));
    assert_eq!(
        lines,
        strings(&[
            "playercli: warning: dial play.test:9100: [::1]:9100: refused",
            "playercli: warning: dial play.test:9100: 127.0.0.1:9100: timed out",
        ])
    );

    let (client, lines) = dial_report("play.test:9100", Ok((7u16, Vec::new())));
    assert_eq!(client, Some(7));
    assert!(lines.is_empty(), "{lines:?}");
}

#[test]
fn dial_report_has_no_client_and_reports_every_failure_without_a_connect() {
    let (client, lines) = dial_report::<u16>("play.test:9100", Err(strings(&["[::1]:9100: refused", "127.0.0.1:9100: timed out"])));
    assert_eq!(client, None);
    assert_eq!(
        lines,
        strings(&["playercli: dial play.test:9100: [::1]:9100: refused", "playercli: dial play.test:9100: 127.0.0.1:9100: timed out"])
    );
}

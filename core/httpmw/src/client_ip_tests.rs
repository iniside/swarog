//! Client-IP table tests, ported from Go's `httpmw_test.go`
//! (`TestClientIP_AntiSpoof`, `TestClientIP_UntrustedRemoteAddrIgnoresXFF`) plus
//! `ParseCIDRs` coverage and the malformed-hop refinement.

use super::*;
use std::net::{IpAddr, Ipv4Addr};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[test]
fn parse_cidrs_skips_blanks_and_accepts_cidr_and_bare_ip() {
    assert!(parse_cidrs("").unwrap().is_empty());
    assert!(parse_cidrs("  , ,  ").unwrap().is_empty());
    let nets = parse_cidrs("10.0.0.0/8, 192.168.1.5").unwrap();
    assert_eq!(nets.len(), 2);
    assert!(is_trusted(v4(10, 1, 2, 3), &nets), "inside the /8");
    assert!(is_trusted(v4(192, 168, 1, 5), &nets), "bare IP -> host /32");
    assert!(!is_trusted(v4(192, 168, 1, 6), &nets), "outside the /32");
}

#[test]
fn parse_cidrs_rejects_garbage() {
    assert!(parse_cidrs("not-an-ip").is_err());
    assert!(parse_cidrs("10.0.0.0/8, nonsense").is_err());
}

#[test]
fn anti_spoof_takes_rightmost_untrusted_hop() {
    // Go TestClientIP_AntiSpoof case 1: RemoteAddr is a trusted proxy; XFF ends with a
    // trusted hop appended by the proxy. The attacker-controlled left (1.2.3.4) must be
    // IGNORED; we take the rightmost UNTRUSTED hop.
    let trusted = parse_cidrs("10.0.0.0/8").unwrap();
    let got = client_ip(
        v4(10, 0, 0, 1),
        Some("1.2.3.4, 203.0.113.7, 10.0.0.9"),
        None,
        &trusted,
    );
    assert_eq!(got, v4(203, 0, 113, 7), "must not be 1.2.3.4");
}

#[test]
fn all_trusted_xff_falls_back_to_x_real_ip() {
    // Go TestClientIP_AntiSpoof case 2: every XFF hop is trusted -> fall back to X-Real-IP.
    let trusted = parse_cidrs("10.0.0.0/8").unwrap();
    let got = client_ip(
        v4(10, 0, 0, 1),
        Some("10.0.0.5, 10.0.0.9"),
        Some("198.51.100.2"),
        &trusted,
    );
    assert_eq!(got, v4(198, 51, 100, 2));
}

#[test]
fn untrusted_remote_ignores_forwarding_headers() {
    // Go TestClientIP_UntrustedRemoteAddrIgnoresXFF: RemoteAddr is NOT trusted, so the
    // spoofable forwarding headers are ignored entirely.
    let trusted = parse_cidrs("10.0.0.0/8").unwrap();
    let got = client_ip(v4(203, 0, 113, 50), Some("1.2.3.4"), Some("5.6.7.8"), &trusted);
    assert_eq!(got, v4(203, 0, 113, 50));
}

#[test]
fn all_trusted_and_no_x_real_ip_falls_back_to_remote() {
    let trusted = parse_cidrs("10.0.0.0/8").unwrap();
    let got = client_ip(v4(10, 0, 0, 1), Some("10.0.0.5, 10.0.0.9"), None, &trusted);
    assert_eq!(got, v4(10, 0, 0, 1));
}

#[test]
fn malformed_xff_hop_is_skipped_not_returned() {
    // Refinement over Go (which would have returned the raw garbage string as the client
    // IP): a malformed rightmost hop is skipped, and the next valid untrusted hop wins.
    let trusted = parse_cidrs("10.0.0.0/8").unwrap();
    let got = client_ip(v4(10, 0, 0, 1), Some("203.0.113.7, garbage"), None, &trusted);
    assert_eq!(got, v4(203, 0, 113, 7));
}

#[test]
fn no_trusted_set_means_remote_is_authoritative() {
    // With an empty trusted set, the direct peer is never a trusted proxy, so XFF is
    // always ignored — the split-peer / no-proxy default.
    let trusted: Vec<ipnet::IpNet> = vec![];
    let got = client_ip(v4(203, 0, 113, 9), Some("1.2.3.4"), Some("5.6.7.8"), &trusted);
    assert_eq!(got, v4(203, 0, 113, 9));
}

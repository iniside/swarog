//! Property tests for the XFF walk: over arbitrary chains and remotes, the result is
//! never a trusted address unless the walk fell all the way back to the (trusted)
//! remote — mirroring Go's exact right-to-left, first-untrusted-hop rule.

use super::*;
use proptest::prelude::*;
use std::net::{IpAddr, Ipv4Addr};

/// The fixed trusted set for these properties: the whole `10.0.0.0/8`.
fn trusted() -> Vec<ipnet::IpNet> {
    parse_cidrs("10.0.0.0/8").unwrap()
}

/// Whether a v4 address is inside `10.0.0.0/8` (the property's ground truth, computed
/// independently of the code under test).
fn in_ten(a: Ipv4Addr) -> bool {
    a.octets()[0] == 10
}

fn join(addrs: &[Ipv4Addr]) -> String {
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

proptest! {
    /// A trusted proxy peer: the walk returns the RIGHTMOST untrusted hop, or the remote
    /// when every hop is trusted — and the result is a trusted address only in that
    /// all-trusted fallback case.
    #[test]
    fn trusted_remote_returns_rightmost_untrusted_or_remote(
        hops in proptest::collection::vec(any::<u32>(), 0..8)
    ) {
        let trusted = trusted();
        let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)); // a trusted proxy
        let addrs: Vec<Ipv4Addr> = hops.iter().map(|&n| Ipv4Addr::from(n)).collect();
        let joined = join(&addrs);
        let xff = if addrs.is_empty() { None } else { Some(joined.as_str()) };

        let got = client_ip(remote, xff, None, &trusted);

        let expected = addrs
            .iter()
            .rev()
            .find(|a| !in_ten(**a))
            .map(|a| IpAddr::V4(*a))
            .unwrap_or(remote);
        prop_assert_eq!(got, expected);

        // The security invariant: a trusted address only ever comes back as the
        // remote-fallback (no untrusted hop existed).
        if got != remote {
            prop_assert!(!is_trusted(got, &trusted), "leaked a trusted hop: {}", got);
        }
    }

    /// An untrusted peer: forwarding headers are spoofable and MUST be ignored — the
    /// result is always the remote regardless of the chain or X-Real-IP.
    #[test]
    fn untrusted_remote_always_returns_remote(
        hops in proptest::collection::vec(any::<u32>(), 0..8),
        r in any::<u32>().prop_filter("remote must be outside 10/8", |n| Ipv4Addr::from(*n).octets()[0] != 10),
    ) {
        let trusted = trusted();
        let remote = IpAddr::V4(Ipv4Addr::from(r));
        let addrs: Vec<Ipv4Addr> = hops.iter().map(|&n| Ipv4Addr::from(n)).collect();
        let joined = join(&addrs);
        let xff = if addrs.is_empty() { None } else { Some(joined.as_str()) };

        let got = client_ip(remote, xff, Some("8.8.8.8"), &trusted);
        prop_assert_eq!(got, remote);
    }
}

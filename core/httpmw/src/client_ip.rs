//! Trusted-proxy-aware client-IP resolution (port of Go's `ClientIP` + `ParseCIDRs`).
//!
//! The direct peer (the kernel-observed connection address) is the ground truth.
//! `X-Forwarded-For` is honored ONLY when that peer is itself a trusted proxy; then the
//! chain is walked RIGHT-TO-LEFT and the first hop NOT in the trusted set is returned —
//! never index 0. A reverse proxy APPENDS the real peer on the right, so `XFF[0]` is
//! fully attacker-controlled: trusting it would let a fresh fake per request mint a
//! fresh bucket and bypass the limit. This is the exact spoof the framework XFF
//! extractors (which trust the header unconditionally) do NOT guard.

use std::net::IpAddr;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};

/// Parses a comma-separated trusted-proxy list (`TRUSTED_PROXY_CIDRS`). Blank entries
/// are skipped; an empty/whitespace input yields an empty vec. Accepts both CIDR forms
/// (`10.0.0.0/8`) and — a deliberate, safe superset of Go's `net.ParseCIDR`-only parser
/// — bare host IPs (`10.0.0.1`, treated as a `/32` or `/128`). A genuinely malformed
/// entry is an error, exactly as Go's `ParseCIDRs` propagates one.
pub fn parse_cidrs(csv: &str) -> Result<Vec<IpNet>, String> {
    let mut out = Vec::new();
    for part in csv.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_one(part)?);
    }
    Ok(out)
}

/// Parses one trusted-proxy entry: a CIDR, or a bare IP promoted to a host network.
fn parse_one(part: &str) -> Result<IpNet, String> {
    if let Ok(net) = part.parse::<IpNet>() {
        return Ok(net);
    }
    match part.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Ok(IpNet::V4(Ipv4Net::new(v4, 32).expect("32 is a valid v4 prefix"))),
        Ok(IpAddr::V6(v6)) => {
            Ok(IpNet::V6(Ipv6Net::new(v6, 128).expect("128 is a valid v6 prefix")))
        }
        Err(_) => Err(format!("invalid trusted proxy CIDR or IP: {part:?}")),
    }
}

/// Whether `ip` falls within any trusted CIDR.
fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    trusted.iter().any(|net| net.contains(&ip))
}

/// Resolves the trustworthy client IP for `remote` (the direct peer), consulting the
/// forwarding headers ONLY when `remote` is itself a trusted proxy. Port of Go's
/// `ClientIP`, refined to a typed [`IpAddr`]:
///
/// - `remote` not trusted → return `remote`; the forwarding headers are spoofable and
///   ignored.
/// - `remote` trusted → walk `xff` right-to-left; return the first hop that PARSES and
///   is NOT trusted. A malformed hop is skipped (Go would have returned its raw string;
///   returning a typed address means we fall through instead — strictly safer).
/// - no untrusted hop found → `x_real_ip` if it parses, else `remote`.
pub fn client_ip(
    remote: IpAddr,
    xff: Option<&str>,
    x_real_ip: Option<&str>,
    trusted: &[IpNet],
) -> IpAddr {
    if !is_trusted(remote, trusted) {
        return remote;
    }
    if let Some(xff) = xff {
        for hop in xff.split(',').rev() {
            let hop = hop.trim();
            if hop.is_empty() {
                continue;
            }
            match hop.parse::<IpAddr>() {
                Ok(ip) if !is_trusted(ip, trusted) => return ip,
                // A trusted hop (keep walking left) or a malformed one (skip).
                _ => continue,
            }
        }
    }
    if let Some(xr) = x_real_ip {
        if let Ok(ip) = xr.trim().parse::<IpAddr>() {
            return ip;
        }
    }
    remote
}

#[cfg(test)]
#[path = "client_ip_tests.rs"]
mod client_ip_tests;

#[cfg(test)]
#[path = "client_ip_prop_tests.rs"]
mod client_ip_prop_tests;

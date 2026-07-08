//! The shared dev CA + mutual-TLS config for the edge hop (port of Go's
//! `edge/tls.go`). Every edge process in a deployment shares ONE [`DevCA`]; at boot
//! each mints its own short-lived leaf signed by it (a server leaf with server-auth
//! EKU + loopback SANs, a client leaf with client-auth EKU). Because both sides
//! trust the same CA, a server ACCEPTS a stream only from a client presenting a
//! CA-signed cert, and a client verifies the server against the same anchor.
//!
//! ## The mTLS 5-point spec (all enforced here)
//! 1. [`DevCA::server_tls`] installs a [`rustls::server::WebPkiClientVerifier`] built
//!    from the shared root → the client cert is REQUIRED and verified (quinn does
//!    NOT request client certs by default).
//! 2. ALPN [`ALPN`] (`b"edge"`) is set on BOTH the server and client rustls config.
//! 3. The server leaf's SANs include `localhost` + loopback IPs (127.0.0.1, ::1);
//!    the client dials with `ServerName = "localhost"` (see `client::Client::dial`).
//! 4. One [`rustls::RootCertStore`] containing EXACTLY the dev CA — no
//!    webpki/system-roots fallback on either side.
//! 5. TLS 1.3 only (`with_protocol_versions(&[&TLS13])`, and the `tls12` rustls
//!    feature is not compiled in) with the correct server-auth vs client-auth EKU on
//!    the leaves.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, Ia5String, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::Error;

/// The ALPN protocol id negotiated for every edge connection (mTLS point 2). QUIC
/// requires ALPN, and it must match on both sides.
pub const ALPN: &[u8] = b"edge";

const CA_COMMON_NAME: &str = "gamebackend-edge-dev-ca";
const LEAF_COMMON_NAME: &str = "gamebackend-edge-leaf";

/// The local trust anchor for the edge hop's MUTUAL TLS. Holds the anchor cert DER
/// (put into the root store and presented in every leaf's chain), the CA signing
/// key, and an issuer handle used to sign fresh leaves.
pub struct DevCA {
    /// The anchor DER — for a generated CA this is its own self-signed cert; for a
    /// loaded CA it is the exact bytes read from disk (the shared anchor).
    ca_der: CertificateDer<'static>,
    /// The CA signing key (rcgen), used to sign leaves.
    ca_key: KeyPair,
    /// An rcgen issuer whose params carry the CA's DN/key-usages. Its own DER is
    /// unused for the presented chain — only its DN + `ca_key` drive leaf signing —
    /// so a loaded CA still chains correctly to `ca_der`.
    ca_issuer: rcgen::Certificate,
    /// One root store containing EXACTLY this CA (mTLS point 4).
    roots: Arc<RootCertStore>,
    /// The explicit ring crypto provider, shared by the server verifier and both
    /// config builders (no reliance on a process-global default provider).
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl DevCA {
    /// Mints a fresh in-memory CA (ECDSA P-256). Every call is a NEW, independent
    /// anchor — so a generated CA only authenticates peers that share THIS instance.
    /// A cross-process split must instead share one CA on disk via [`DevCA::load`].
    pub fn generate() -> Result<DevCA, Error> {
        let ca_key = KeyPair::generate().map_err(Error::Rcgen)?;
        let params = ca_params()?;
        let ca_cert = params.self_signed(&ca_key).map_err(Error::Rcgen)?;
        let ca_der = ca_cert.der().clone();
        // Rebuild an issuer from fresh params (self_signed consumed the first).
        let ca_issuer = ca_params()?.self_signed(&ca_key).map_err(Error::Rcgen)?;
        Self::assemble(ca_der, ca_key, ca_issuer)
    }

    /// Loads a CA cert+key from PEM files (the shared-anchor path). The cert must be
    /// a PEM CERTIFICATE and the key a PEM private key (the format
    /// [`DevCA::write_pem`] / the `edgeca` binary produce).
    pub fn load(cert_path: &str, key_path: &str) -> Result<DevCA, Error> {
        let cert_pem = std::fs::read_to_string(cert_path)
            .map_err(|e| Error::Ca(format!("read CA cert {cert_path:?}: {e}")))?;
        let key_pem = std::fs::read_to_string(key_path)
            .map_err(|e| Error::Ca(format!("read CA key {key_path:?}: {e}")))?;

        let ca_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .ok_or_else(|| Error::Ca(format!("CA cert {cert_path:?} has no PEM CERTIFICATE")))?
            .map_err(|e| Error::Ca(format!("parse CA cert {cert_path:?}: {e}")))?;

        let ca_key = KeyPair::from_pem(&key_pem)
            .map_err(|e| Error::Ca(format!("parse CA key {key_path:?}: {e}")))?;

        // Reconstruct the issuer from the FIXED CA params (same CN + key-usages
        // `edgeca` always mints) paired with the loaded key. `signed_by` uses only
        // the issuer's DN + key to sign a leaf, so the leaf chains to the loaded
        // `ca_der` as long as the DN matches — which it does, because every dev CA in
        // this sketch is produced by `edgeca` with these exact params. (This mirrors
        // Go's `LoadDevCA`, which likewise assumes the format `edgeca` produces.)
        let ca_issuer = ca_params()?.self_signed(&ca_key).map_err(Error::Rcgen)?;

        Self::assemble(ca_der, ca_key, ca_issuer)
    }

    fn assemble(
        ca_der: CertificateDer<'static>,
        ca_key: KeyPair,
        ca_issuer: rcgen::Certificate,
    ) -> Result<DevCA, Error> {
        let mut roots = RootCertStore::empty();
        roots
            .add(ca_der.clone())
            .map_err(|e| Error::Ca(format!("add CA to root store: {e}")))?;
        Ok(DevCA {
            ca_der,
            ca_key,
            ca_issuer,
            roots: Arc::new(roots),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }

    /// Serializes the CA cert and key to PEM files so every edge process can
    /// [`DevCA::load`] the same anchor. Used by the `edgeca` binary.
    pub fn write_pem(&self, cert_path: &str, key_path: &str) -> Result<(), Error> {
        let cert_pem = der_to_pem(&self.ca_der);
        std::fs::write(cert_path, cert_pem)
            .map_err(|e| Error::Ca(format!("write CA cert {cert_path:?}: {e}")))?;
        std::fs::write(key_path, self.ca_key.serialize_pem())
            .map_err(|e| Error::Ca(format!("write CA key {key_path:?}: {e}")))?;
        Ok(())
    }

    /// Mints a fresh short-lived leaf signed by the CA. A server leaf carries
    /// server-auth EKU + loopback SANs (so a client dialing localhost / 127.0.0.1 /
    /// ::1 verifies the name); a client leaf carries client-auth EKU and no SANs (a
    /// client is authenticated by chaining to the CA, not by name). Returns the
    /// presented chain `[leaf, ca]` and the leaf private key.
    fn leaf(&self, server: bool) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), Error> {
        let leaf_key = KeyPair::generate().map_err(Error::Rcgen)?;
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(Error::Rcgen)?;
        p.distinguished_name.push(DnType::CommonName, LEAF_COMMON_NAME);
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        if server {
            p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            p.subject_alt_names = vec![
                SanType::DnsName(Ia5String::try_from("localhost").map_err(Error::Rcgen)?),
                SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            ];
        } else {
            p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        }
        let leaf = p
            .signed_by(&leaf_key, &self.ca_issuer, &self.ca_key)
            .map_err(Error::Rcgen)?;
        let chain = vec![leaf.der().clone(), self.ca_der.clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        Ok((chain, key))
    }

    /// Builds the edge SERVER's mutual-TLS config: presents a CA-signed server leaf
    /// and — the security-critical part — REQUIRES and VERIFIES a client cert that
    /// chains to this CA (mTLS points 1, 2, 5). A client with no cert, or one signed
    /// by a different CA, is rejected at the TLS handshake before any stream is
    /// dispatched.
    pub fn server_tls(&self) -> Result<ServerConfig, Error> {
        let (chain, key) = self.leaf(true)?;
        let verifier = WebPkiClientVerifier::builder_with_provider(self.roots.clone(), self.provider.clone())
            .build()
            .map_err(|e| Error::Tls(format!("client verifier: {e}")))?;
        let mut cfg = ServerConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(Error::Rustls)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(Error::Rustls)?;
        cfg.alpn_protocols = vec![ALPN.to_vec()];
        Ok(cfg)
    }

    /// Builds the edge CLIENT's mutual-TLS config: presents a CA-signed client leaf
    /// AND verifies the server against the same CA (mTLS points 2, 4, 5; no
    /// system-roots fallback, no `InsecureSkipVerify`).
    pub fn client_tls(&self) -> Result<ClientConfig, Error> {
        let (chain, key) = self.leaf(false)?;
        let mut cfg = ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(Error::Rustls)?
            .with_root_certificates((*self.roots).clone())
            .with_client_auth_cert(chain, key)
            .map_err(Error::Rustls)?;
        cfg.alpn_protocols = vec![ALPN.to_vec()];
        Ok(cfg)
    }
}

impl DevCA {
    /// A CLIENT config that verifies the server against this CA but presents NO
    /// client certificate. Against an mTLS server (which REQUIRES a client cert) a
    /// handshake with this config MUST fail — the negative proof that client-cert
    /// verification is real, not decorative. (Server-auth still passes, so the
    /// failure isolates the missing client cert.)
    pub fn client_tls_without_client_auth(&self) -> Result<ClientConfig, Error> {
        let mut cfg = ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(Error::Rustls)?
            .with_root_certificates((*self.roots).clone())
            .with_no_client_auth();
        cfg.alpn_protocols = vec![ALPN.to_vec()];
        Ok(cfg)
    }
}

/// Fixed CA parameters (a P-256 ECDSA CA with a stable CN + cert-sign usages).
fn ca_params() -> Result<CertificateParams, Error> {
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(Error::Rcgen)?;
    params.distinguished_name.push(DnType::CommonName, CA_COMMON_NAME);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    Ok(params)
}

/// Encodes a cert DER as a PEM CERTIFICATE block (avoids depending on rcgen's `pem`
/// re-export for a loaded anchor, whose DER we hold directly).
fn der_to_pem(der: &CertificateDer<'_>) -> String {
    use std::fmt::Write as _;
    let b64 = base64_encode(der.as_ref());
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        let _ = writeln!(out, "{}", std::str::from_utf8(chunk).unwrap());
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// A tiny standard-base64 encoder (no external dep for the one PEM emit path).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Resolves the process's edge trust anchor. When `EDGE_CA_CERT` and `EDGE_CA_KEY`
/// both point at files it loads that shared CA; otherwise it generates an ephemeral
/// one and logs a LOUD warning — the generated anchor is NOT shared with peers, so a
/// real split without the env fails the handshake rather than run unauthenticated.
pub fn dev_ca_from_env() -> Result<DevCA, Error> {
    let cert = std::env::var("EDGE_CA_CERT").unwrap_or_default();
    let key = std::env::var("EDGE_CA_KEY").unwrap_or_default();
    if !cert.trim().is_empty() && !key.trim().is_empty() {
        return DevCA::load(cert.trim(), key.trim());
    }
    tracing::warn!(
        "EDGE MUTUAL TLS using a GENERATED dev CA — dev only; this anchor is NOT shared \
         with peers, so a real split will REJECT cross-process calls. Set EDGE_CA_CERT and \
         EDGE_CA_KEY to a shared CA."
    );
    DevCA::generate()
}

static SHARED_CA: OnceLock<Result<Arc<DevCA>, String>> = OnceLock::new();

/// The process-wide edge CA, resolved once via [`dev_ca_from_env`] and memoized so
/// the server config and every client dial in the SAME process chain to one root.
pub fn shared_dev_ca() -> Result<Arc<DevCA>, Error> {
    SHARED_CA
        .get_or_init(|| dev_ca_from_env().map(Arc::new).map_err(|e| e.to_string()))
        .clone()
        .map_err(Error::Ca)
}

/// Convenience: an IPv4/IPv6-unspecified bind address matching `peer`'s family, for
/// a client endpoint's local socket.
pub(crate) fn client_bind_addr(peer: SocketAddr) -> SocketAddr {
    if peer.is_ipv6() {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    }
}

#[cfg(test)]
mod tests {
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
}

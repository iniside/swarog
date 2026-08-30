//! The loopback OIDC fixture the federated assertions verify against.
//!
//! A promotion ([A8]/[WL8]) requires linking a NON-guest identity, and every non-guest
//! provider this build ships verifies a signed id_token — so the harness becomes the
//! identity provider: one RSA key generated per run, its JWKS served at
//! [`PROOF_OIDC_ISSUER`]'s origin, and RS256 tokens minted for the `epic` provider the
//! `Proof` fleet points there. Nothing here contacts a real IdP, and the key never
//! outlives the process.

use anyhow::{Context, Result};
use base64::Engine as _;
use processctl::{proof_oidc_port, PROOF_OIDC_CLIENT_ID, PROOF_OIDC_ISSUER};
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;

const KID: &str = "splitproof-1";

pub struct Idp {
    encoding: jsonwebtoken::EncodingKey,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Idp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Idp {
    /// Binds the issuer's port before the fleet starts. The bind is deliberately not
    /// best-effort: a taken port means a leftover process from an earlier run would
    /// answer the fleet's JWKS fetch with keys this run cannot sign for.
    pub async fn start() -> Result<Idp> {
        let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048)
            .context("generate the fixture RSA key")?;
        let pem = key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .context("encode the fixture key as PKCS#8")?;
        let encoding = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
            .context("load the fixture key into jsonwebtoken")?;
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "kid": KID,
                "use": "sig",
                "alg": "RS256",
                "n": b64(&key.n().to_bytes_be()),
                "e": b64(&key.e().to_bytes_be()),
            }]
        })
        .to_string();

        let app = axum::Router::new().route(
            "/jwks",
            axum::routing::get(move || {
                let jwks = jwks.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        jwks,
                    )
                }
            }),
        );
        let port = proof_oidc_port();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind the OIDC fixture on :{port}"))?;
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Idp { encoding, server })
    }

    /// An id_token the Proof fleet's `epic` verifier accepts for `subject`.
    pub fn token(&self, subject: &str) -> Result<String> {
        let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before the unix epoch")?
            .as_secs();
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(KID.to_string());
        let claims = serde_json::json!({
            "iss": PROOF_OIDC_ISSUER,
            "aud": PROOF_OIDC_CLIENT_ID,
            "sub": subject,
            "exp": exp,
        });
        jsonwebtoken::encode(&header, &claims, &self.encoding).context("sign the fixture id_token")
    }
}

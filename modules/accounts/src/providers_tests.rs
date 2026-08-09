//! Tests for `providers.rs`'s validating parse authority and the three-way registry
//! it feeds. Table-driven over `ProviderConfig::from_vars(&BTreeMap)` — no process
//! env is read or mutated, so failing branches are provable with zero shared state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine as _;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;

use crate::epic::OidcVerifier;
use crate::providers::{epic_credentials, ProviderConfig, Providers, Resolution};

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn err_msg(pairs: &[(&str, &str)]) -> String {
    match ProviderConfig::from_vars(&vars(pairs)) {
        Ok(_) => panic!("expected from_vars to reject {pairs:?}, got Ok"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn absent_epic_yields_ok_with_no_entry() {
    let cfg = ProviderConfig::from_vars(&BTreeMap::new()).unwrap();
    assert!(cfg.epic.is_none());
}

#[test]
fn minimal_valid_config_is_accepted() {
    let cfg = ProviderConfig::from_vars(&vars(&[("EPIC_CLIENT_ID", "client-1")])).unwrap();
    let epic = cfg.epic.expect("epic present when EPIC_CLIENT_ID is set");
    assert_eq!(epic.client_id, "client-1");
    assert!(epic.oauth.is_none(), "no EPIC_CLIENT_SECRET => no oauth config");
}

#[test]
fn loopback_redirect_uri_accepted_with_insecure_cookie() {
    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_CLIENT_SECRET", "shh"),
        ("EPIC_REDIRECT_URI", "http://localhost:8080/accounts/epic/callback"),
    ]))
    .unwrap();
    let oauth = cfg.epic.unwrap().oauth.expect("client secret set => oauth present");
    assert!(!oauth.cookie_secure, "loopback http must not set Secure");
}

#[test]
fn https_redirect_uri_accepted_with_secure_cookie() {
    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_CLIENT_SECRET", "shh"),
        ("EPIC_REDIRECT_URI", "https://game.example/accounts/epic/callback"),
    ]))
    .unwrap();
    let oauth = cfg.epic.unwrap().oauth.expect("client secret set => oauth present");
    assert!(oauth.cookie_secure, "https must set Secure");
}

#[test]
fn reject_unparseable_jwks_url() {
    let msg = err_msg(&[("EPIC_CLIENT_ID", "client-1"), ("EPIC_JWKS_URL", "hunter2")]);
    assert!(msg.contains("EPIC_JWKS_URL"), "message did not name the offending var: {msg}");
}

#[test]
fn reject_non_https_non_loopback_scheme() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_JWKS_URL", "ftp://example.com/jwks.json"),
    ]);
    assert!(msg.contains("EPIC_JWKS_URL"), "message did not name the offending var: {msg}");
}

#[test]
fn reject_set_but_empty_variable() {
    let msg = err_msg(&[("EPIC_CLIENT_ID", "")]);
    assert!(msg.contains("EPIC_CLIENT_ID"), "message did not name the offending var: {msg}");
    assert!(msg.contains("set but empty"), "message did not describe the empty-value rule: {msg}");
}

#[test]
fn reject_redirect_uri_wrong_path() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_REDIRECT_URI", "https://game.example/wrong/path"),
    ]);
    assert!(msg.contains("EPIC_REDIRECT_URI"), "message did not name the offending var: {msg}");
    assert!(msg.contains("path"), "message did not describe the path rule: {msg}");
}

#[test]
fn reject_redirect_uri_with_fragment() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_REDIRECT_URI", "https://game.example/accounts/epic/callback#frag"),
    ]);
    assert!(msg.contains("EPIC_REDIRECT_URI"), "message did not name the offending var: {msg}");
    assert!(msg.contains("fragment"), "message did not describe the fragment rule: {msg}");
}

#[test]
fn reject_truncated_issuer_prefix() {
    let msg = err_msg(&[("EPIC_CLIENT_ID", "client-1"), ("EPIC_ISSUER_PREFIX", "h")]);
    assert!(msg.contains("EPIC_ISSUER_PREFIX"), "message did not name the offending var: {msg}");
}

#[test]
fn reject_oauth_var_without_client_secret() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_REDIRECT_URI", "https://game.example/accounts/epic/callback"),
    ]);
    assert!(msg.contains("EPIC_REDIRECT_URI"), "message did not name the offending var: {msg}");
    assert!(msg.contains("EPIC_CLIENT_SECRET"), "message did not name the missing var: {msg}");
}

#[test]
fn reject_epic_var_without_client_id() {
    let msg = err_msg(&[(
        "EPIC_JWKS_URL",
        "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json",
    )]);
    assert!(msg.contains("EPIC_JWKS_URL"), "message did not name the triggering var: {msg}");
    assert!(msg.contains("EPIC_CLIENT_ID"), "message did not name the missing var: {msg}");
}

/// Pins the ordering fix landed in `38c1a7f`: per-field validation of a malformed
/// value must fire BEFORE the missing-`EPIC_CLIENT_ID` completeness bail, even when
/// both conditions hold simultaneously — a naive reversal still produces an `Err`,
/// but names the wrong variable.
#[test]
fn malformed_value_fails_on_its_own_rule_even_without_client_id() {
    let msg = err_msg(&[("EPIC_JWKS_URL", "hunter2")]);
    assert!(
        msg.contains("invalid EPIC_JWKS_URL"),
        "expected the per-field JWKS validation error, got: {msg}"
    );
    assert!(
        !msg.contains("EPIC_CLIENT_ID"),
        "field validation must win over the completeness bail, got: {msg}"
    );
}

#[test]
#[should_panic(expected = "accounts: provider \"epic\" registered twice")]
fn insert_panics_on_duplicate_name() {
    let verifier = Arc::new(
        OidcVerifier::new(
            "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json",
            "https://api.epicgames.dev/epic/oauth/v1",
            "client-1",
        )
        .unwrap(),
    );
    let mut providers = Providers::default();
    providers.insert("epic", epic_credentials(verifier.clone()));
    providers.insert("epic", epic_credentials(verifier));
}

#[test]
fn resolve_unknown_name_is_unknown() {
    let providers = Providers::default();
    match providers.resolve("not-a-real-provider") {
        Resolution::Unknown => {}
        _ => panic!("expected Unknown"),
    }
}

#[test]
fn resolve_known_but_unconfigured_name_is_known_but_unconfigured() {
    let providers = Providers::default();
    match providers.resolve("apple") {
        Resolution::KnownButUnconfigured => {}
        _ => panic!("expected KnownButUnconfigured"),
    }
}

#[test]
fn resolve_configured_name_via_the_production_path() {
    let cfg = ProviderConfig::from_vars(&vars(&[("EPIC_CLIENT_ID", "client-1")])).unwrap();
    let providers = cfg.providers();
    match providers.resolve("epic") {
        Resolution::Configured(_) => {}
        _ => panic!("expected Configured, from_vars -> providers() did not register epic"),
    }
}

// --- Epic end-to-end through the production `from_vars -> providers() -> resolve`
// path: proves the parse yields a verifier that actually verifies, not merely one
// that satisfies field validation. The JWKS/token helpers are duplicated from
// `epic_tests.rs` (its helpers are private to that module, and that file already
// notes the same duplication for the same reason: staying self-contained).

fn test_key(kid: &str) -> (jsonwebtoken::EncodingKey, String) {
    let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
    let pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
    let enc = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let jwks = serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "alg": "RS256",
            "n": b64(&key.n().to_bytes_be()),
            "e": b64(&key.e().to_bytes_be()),
        }]
    })
    .to_string();
    (enc, jwks)
}

async fn serve_counting_jwks(body: String) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let body = body.clone();
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/jwks"), hits)
}

fn token_with_kid(enc: &jsonwebtoken::EncodingKey, issuer: &str, audience: &str, kid: &str) -> String {
    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let claims = serde_json::json!({
        "iss": format!("{issuer}/x"), "aud": audience, "sub": "puid-1", "exp": exp,
    });
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_verifier_from_from_vars_verifies_a_real_token() {
    let issuer = "https://issuer.example";
    let (enc, jwks) = test_key("real-kid");
    let (jwks_url, _hits) = serve_counting_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_JWKS_URL", &jwks_url),
        ("EPIC_ISSUER_PREFIX", issuer),
    ]))
    .unwrap();
    let providers = cfg.providers();
    let Resolution::Configured(verifier) = providers.resolve("epic") else {
        panic!("expected Configured");
    };

    let token = token_with_kid(&enc, issuer, "client-1", "real-kid");
    let verified = verifier.verify(&token).await.unwrap();
    assert_eq!(verified.subject, "puid-1");
    assert_eq!(verified.display_name, format!("epic:{}", "puid-1"));
}

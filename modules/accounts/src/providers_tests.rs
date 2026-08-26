//! Tests for `providers.rs`'s validating parse authority and the three-way registry
//! it feeds. Table-driven over `ProviderConfig::from_vars(&BTreeMap)` — no process
//! env is read or mutated, so failing branches are provable with zero shared state.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;

use crate::oidc::{IssuerMatch, OidcVerifier};
use crate::providers::{oidc_credentials, ProviderConfig, Providers, Resolution, VerifyError};

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A pool handle for the registry construction. `connect_lazy` opens no connection but
/// registers its idle reaper on the current Tokio runtime, so every caller is an async
/// test; none of them reaches the store (the guest verifier is registered, never
/// invoked here).
fn lazy_pool() -> sqlx::PgPool {
    sqlx::PgPool::connect_lazy(
        "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable",
    )
    .expect("lazy pool from a well-formed DSN")
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
    assert_eq!(
        epic.jwks_url,
        "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json"
    );
    assert!(epic.oauth.is_none(), "no EPIC_CLIENT_SECRET => no oauth config");
}

#[test]
fn defaulted_oauth_urls_are_the_documented_epic_endpoints() {
    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_CLIENT_SECRET", "shh"),
    ]))
    .unwrap();
    let oauth = cfg.epic.unwrap().oauth.expect("client secret set => oauth present");
    assert_eq!(oauth.redirect_uri, "http://localhost:8080/accounts/epic/callback");
    assert_eq!(oauth.authorize_url, "https://www.epicgames.com/id/authorize");
    assert_eq!(oauth.token_url, "https://api.epicgames.dev/epic/oauth/v1/token");
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
fn reject_set_but_empty_client_secret() {
    let msg = err_msg(&[("EPIC_CLIENT_ID", "c"), ("EPIC_CLIENT_SECRET", "")]);
    assert!(msg.contains("EPIC_CLIENT_SECRET"), "message did not name the offending var: {msg}");
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
fn reject_malformed_authorize_url() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_CLIENT_SECRET", "shh"),
        ("EPIC_AUTHORIZE_URL", "hunter2"),
    ]);
    assert!(
        msg.contains("invalid EPIC_AUTHORIZE_URL"),
        "message did not name the offending var: {msg}"
    );
}

#[test]
fn reject_malformed_token_url() {
    let msg = err_msg(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_CLIENT_SECRET", "shh"),
        ("EPIC_TOKEN_URL", "hunter2"),
    ]);
    assert!(
        msg.contains("invalid EPIC_TOKEN_URL"),
        "message did not name the offending var: {msg}"
    );
}

/// Deleting `check_endpoint("EPIC_AUTHORIZE_URL", ...)` from `EpicOAuthConfig::new`
/// would still produce an `Err` here (the missing-client-id completeness bail), so
/// the `invalid ` prefix — not mere `is_err()` — is what proves the field check ran.
#[test]
fn malformed_authorize_url_fails_on_its_own_rule_even_without_client_id() {
    let msg = err_msg(&[("EPIC_AUTHORIZE_URL", "hunter2")]);
    assert!(
        msg.contains("invalid EPIC_AUTHORIZE_URL"),
        "expected the per-field authorize-url validation error, got: {msg}"
    );
    assert!(
        !msg.contains("EPIC_CLIENT_ID"),
        "field validation must win over the completeness bail, got: {msg}"
    );
}

/// Same shape as the authorize-url ordering pin, for the token endpoint.
#[test]
fn malformed_token_url_fails_on_its_own_rule_even_without_client_id() {
    let msg = err_msg(&[("EPIC_TOKEN_URL", "hunter2")]);
    assert!(
        msg.contains("invalid EPIC_TOKEN_URL"),
        "expected the per-field token-url validation error, got: {msg}"
    );
    assert!(
        !msg.contains("EPIC_CLIENT_ID"),
        "field validation must win over the completeness bail, got: {msg}"
    );
}

#[test]
fn reject_issuer_prefix_without_a_host() {
    let msg = err_msg(&[("EPIC_CLIENT_ID", "client-1"), ("EPIC_ISSUER_PREFIX", "file:///x")]);
    assert!(msg.contains("EPIC_ISSUER_PREFIX"), "message did not name the offending var: {msg}");
    assert!(msg.contains("host is required"), "message did not describe the host rule: {msg}");
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
            IssuerMatch::prefix("issuer", "https://api.epicgames.dev/epic/oauth/v1").unwrap(),
            vec!["client-1".to_string()],
        )
        .unwrap(),
    );
    let mut providers = Providers::default();
    providers.insert("epic", oidc_credentials("epic", verifier.clone()));
    providers.insert("epic", oidc_credentials("epic", verifier));
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
    match providers.resolve("epic") {
        Resolution::KnownButUnconfigured => {}
        _ => panic!("expected KnownButUnconfigured"),
    }
}

#[tokio::test]
async fn resolve_configured_name_via_the_production_path() {
    let cfg = ProviderConfig::from_vars(&vars(&[("EPIC_CLIENT_ID", "client-1")])).unwrap();
    let providers = cfg.providers(&lazy_pool());
    match providers.resolve("epic") {
        Resolution::Configured(_) => {}
        _ => panic!("expected Configured, from_vars -> providers() did not register epic"),
    }
}

// --- Epic end-to-end through the production `from_vars -> providers() -> resolve`
// path: proves the parse yields a verifier that actually verifies, not merely one
// that satisfies field validation. The JWKS/token helpers are duplicated from
// `oidc_tests.rs` (its helpers are private to that module, and that file already
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

async fn serve_jwks(body: String) -> String {
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let body = body.clone();
            async move {
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
    format!("http://{addr}/jwks")
}

fn token_with_kid(
    enc: &jsonwebtoken::EncodingKey,
    issuer: &str,
    audience: &str,
    kid: &str,
    subject: &str,
) -> String {
    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let claims = serde_json::json!({
        "iss": format!("{issuer}/x"), "aud": audience, "sub": subject, "exp": exp,
    });
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_verifier_from_from_vars_verifies_a_real_token() {
    let issuer = "https://issuer.example";
    let (enc, jwks) = test_key("real-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_JWKS_URL", &jwks_url),
        ("EPIC_ISSUER_PREFIX", issuer),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("epic") else {
        panic!("expected Configured");
    };

    let subject = "puid-account-1234567890";
    let token = token_with_kid(&enc, issuer, "client-1", "real-kid", subject);
    let verified = verifier.verify(&token).await.unwrap();
    assert_eq!(verified.subject, subject);
    assert_eq!(verified.display_name, format!("epic:{}", &subject[..8]));
}

// --- Google config parse (Step 4). Reuses the `test_key`/`serve_jwks` fixtures
// above; needs its own token helper because `token_with_kid` always appends
// `/x` to the issuer, and Google's issuer is matched EXACTLY.

#[test]
fn absent_google_yields_ok_with_no_entry() {
    let cfg = ProviderConfig::from_vars(&BTreeMap::new()).unwrap();
    assert!(cfg.google.is_none());
}

#[test]
fn reject_google_jwks_url_without_client_ids() {
    let msg = err_msg(&[("GOOGLE_JWKS_URL", "https://www.googleapis.com/oauth2/v3/certs")]);
    assert!(
        msg.contains("is set but GOOGLE_CLIENT_IDS is not"),
        "message did not describe the missing-client-ids rule: {msg}"
    );
}

#[test]
fn reject_google_client_ids_with_empty_entries() {
    for raw in ["a,,b", "a, ", ",", " "] {
        let msg = err_msg(&[("GOOGLE_CLIENT_IDS", raw)]);
        assert!(msg.contains("GOOGLE_CLIENT_IDS"), "input {raw:?}: message did not name the var: {msg}");
        assert!(
            msg.contains("empty entry"),
            "input {raw:?}: message did not describe the empty-entry rule: {msg}"
        );
    }
}

#[test]
fn google_client_ids_trims_whitespace() {
    let cfg = ProviderConfig::from_vars(&vars(&[("GOOGLE_CLIENT_IDS", " a , b ")])).unwrap();
    let google = cfg.google.expect("google present when GOOGLE_CLIENT_IDS is set");
    assert_eq!(google.client_ids, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn reject_set_but_empty_google_var() {
    let msg = err_msg(&[("GOOGLE_CLIENT_IDS", "")]);
    assert!(msg.contains("GOOGLE_CLIENT_IDS"), "message did not name the offending var: {msg}");
    assert!(msg.contains("set but empty"), "message did not describe the empty-value rule: {msg}");
}

/// Full control over `iss`/`aud`, unlike `token_with_kid` above which always
/// appends `/x` to the issuer — Google's issuer is matched EXACTLY, so the
/// token here must carry the real issuer spelling byte-for-byte.
fn token_with_exact_claims(
    enc: &jsonwebtoken::EncodingKey,
    issuer: &str,
    audience: &str,
    kid: &str,
    subject: &str,
) -> String {
    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let claims = serde_json::json!({
        "iss": issuer, "aud": audience, "sub": subject, "exp": exp,
    });
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

/// The production path end-to-end, proving `oidc_credentials`' provider-name
/// parameter is actually wired per-provider (`google:`, not left over as
/// `epic:`) and that a real Google issuer spelling verifies against the
/// hardcoded `GOOGLE_ISSUERS` constant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_google_verifier_from_from_vars_verifies_a_real_token() {
    let (enc, jwks) = test_key("g-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("GOOGLE_CLIENT_IDS", "web-client,ios-client"),
        ("GOOGLE_JWKS_URL", &jwks_url),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("google") else {
        panic!("expected Configured, from_vars -> providers() did not register google");
    };

    let subject = "sub-account-1234567890";
    let token =
        token_with_exact_claims(&enc, "https://accounts.google.com", "ios-client", "g-kid", subject);
    let verified = verifier.verify(&token).await.unwrap();
    assert_eq!(verified.subject, subject);
    assert_eq!(verified.display_name, format!("google:{}", &subject[..8]));
}

/// The security claim through the PRODUCTION `from_vars -> providers() -> resolve`
/// path, not a test-local `IssuerMatch`: a lookalike issuer that appends a suffix to
/// the real one is rejected. Since `1b328e2` made `prefix` path-boundary-safe too,
/// this specific lookalike no longer distinguishes `exact` from `prefix` (both
/// reject it) — see `resolved_google_verifier_from_from_vars_rejects_a_google_subpath`
/// below for the assertion that actually goes red under that rewire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_google_verifier_from_from_vars_rejects_lookalike_issuer() {
    let (enc, jwks) = test_key("g-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("GOOGLE_CLIENT_IDS", "web-client"),
        ("GOOGLE_JWKS_URL", &jwks_url),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("google") else {
        panic!("expected Configured, from_vars -> providers() did not register google");
    };

    let token = token_with_exact_claims(
        &enc,
        "https://accounts.google.com.evil.test",
        "web-client",
        "g-kid",
        "sub-1",
    );
    let err = match verifier.verify(&token).await {
        Ok(_) => panic!("lookalike issuer must be rejected"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

/// The assertion that DOES go red if `google_from_vars` were ever rewired from
/// `IssuerMatch::exact` to `IssuerMatch::prefix`: Google issues exactly the two
/// listed spellings, never a subpath under `accounts.google.com` — `exact` rejects
/// one, a path-boundary-safe `prefix` on the same base would ACCEPT it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_google_verifier_from_from_vars_rejects_a_google_subpath() {
    let (enc, jwks) = test_key("g-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("GOOGLE_CLIENT_IDS", "web-client"),
        ("GOOGLE_JWKS_URL", &jwks_url),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("google") else {
        panic!("expected Configured, from_vars -> providers() did not register google");
    };

    let token = token_with_exact_claims(
        &enc,
        "https://accounts.google.com/not-a-real-path",
        "web-client",
        "g-kid",
        "sub-1",
    );
    let err = match verifier.verify(&token).await {
        Ok(_) => panic!("a subpath of the Google issuer is not one of the two exact spellings"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

/// The legacy scheme-less Google issuer spelling, driven through the PRODUCTION
/// `from_vars -> providers() -> resolve` path rather than a test-local literal list:
/// this is the one test that goes red if `"accounts.google.com"` were ever dropped
/// from `GOOGLE_ISSUERS`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_google_verifier_from_from_vars_accepts_the_scheme_less_spelling() {
    let (enc, jwks) = test_key("g-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("GOOGLE_CLIENT_IDS", "web-client"),
        ("GOOGLE_JWKS_URL", &jwks_url),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("google") else {
        panic!("expected Configured, from_vars -> providers() did not register google");
    };

    let subject = "sub-legacy-1234567890";
    let token = token_with_exact_claims(&enc, "accounts.google.com", "web-client", "g-kid", subject);
    let verified = verifier.verify(&token).await.unwrap();
    assert_eq!(verified.subject, subject);
}

/// The operator-override path end-to-end: a bare-host `EPIC_ISSUER_PREFIX` (no path
/// component) still enforces the path-boundary rule where an operator can actually
/// reach it, not just at the `IssuerMatch::prefix` constructor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolved_epic_verifier_with_bare_host_issuer_prefix_rejects_lookalike() {
    let (enc, jwks) = test_key("real-kid");
    let jwks_url = serve_jwks(jwks).await;

    let cfg = ProviderConfig::from_vars(&vars(&[
        ("EPIC_CLIENT_ID", "client-1"),
        ("EPIC_JWKS_URL", &jwks_url),
        ("EPIC_ISSUER_PREFIX", "https://issuer.example"),
    ]))
    .unwrap();
    let providers = cfg.providers(&lazy_pool());
    let Resolution::Configured(verifier) = providers.resolve("epic") else {
        panic!("expected Configured");
    };

    let token = token_with_exact_claims(
        &enc,
        "https://issuer.example.evil.test",
        "client-1",
        "real-kid",
        "sub-1",
    );
    let err = match verifier.verify(&token).await {
        Ok(_) => panic!("lookalike issuer must be rejected"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

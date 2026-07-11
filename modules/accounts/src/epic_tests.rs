//! JWKS singleflight/cooldown + error-taxonomy tests for `epic.rs`. All DB-free:
//! stub JWKS endpoints on `127.0.0.1:0` count their hits; tokens are self-minted
//! RS256 JWTs (no live Epic). The `kid` header is ATTACKER-CONTROLLED input on an
//! unauthenticated path, so the amplification bound (one fetch per cooldown, not
//! one per bogus token) and the 503-vs-401 split are security behavior, pinned here.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use accountsapi::Auth as _;
use base64::Engine as _;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;
use sqlx::PgPool;

use crate::epic::{OidcVerifier, VerifyError};
use crate::password::ArgonVerifier;
use crate::store::Store;
use crate::Service;

const CLIENT_ID: &str = "client-epic-tests";
const ISSUER: &str = "https://eas.example";
const DSN: &str = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A fresh RSA test key: the encoding key for signing and the JWKS document for
/// verifying (the same shape as `tests::test_key`, duplicated so this file stays
/// self-contained — `tests`' helpers are private to that module).
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

/// Serves `body` with `status` at `/jwks` on an ephemeral port, counting every hit.
async fn serve_counting_jwks(status: u16, body: String) -> (String, Arc<AtomicUsize>) {
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
                    axum::http::StatusCode::from_u16(status).unwrap(),
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

/// A structurally valid RS256 token whose header names `kid` — enough to reach the
/// JWKS lookup (the signature never gets checked when the kid is unknown).
fn token_with_kid(enc: &jsonwebtoken::EncodingKey, kid: &str) -> String {
    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let claims = serde_json::json!({
        "iss": format!("{ISSUER}/x"), "aud": CLIENT_ID, "sub": "puid", "exp": exp,
    });
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

/// A lazy-pool service with the epic provider configured — for the `login_epic`
/// status-mapping tests (verify fails before any DB access).
fn epic_service(verifier: OidcVerifier) -> Arc<Service> {
    let epic = OnceLock::new();
    epic.set(Arc::new(verifier)).ok().unwrap();
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DSN).unwrap(),
        },
        bus: Arc::new(bus::Bus::new()),
        dev_auth: false,
        epic,
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    })
}

/// N concurrent verifies with unknown kids cost the IdP EXACTLY ONE fetch: the
/// singleflight mutex coalesces the burst (queued misses re-check the winner's
/// cache), the cooldown suppresses refetches after it, and every caller gets the
/// definitive `Rejected` (their kid is absent from a fresh set) — never `Infra`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_unknown_kids_cost_one_jwks_fetch() {
    let (enc, jwks) = test_key("real-kid");
    let (url, hits) = serve_counting_jwks(200, jwks).await;
    let v = Arc::new(OidcVerifier::new(&url, ISSUER, CLIENT_ID).unwrap());

    let verifies: Vec<_> = (0..8)
        .map(|i| {
            let v = v.clone();
            let token = token_with_kid(&enc, &format!("ghost-{i}"));
            tokio::spawn(async move { v.verify(&token).await })
        })
        .collect();
    for verify in verifies {
        let err = verify.await.unwrap().expect_err("unknown kid must be rejected");
        assert!(
            matches!(err, VerifyError::Rejected(_)),
            "unknown kid with a fresh key set is a bad token, not an outage: {err}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one fetch per cooldown, not per token");

    // One more bogus kid during the cooldown: still Rejected (≥1 successful fetch
    // is cached), still no second fetch.
    let err = v
        .verify(&token_with_kid(&enc, "ghost-late"))
        .await
        .expect_err("unknown kid must be rejected");
    assert!(matches!(err, VerifyError::Rejected(_)));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A KNOWN kid still verifies from the cache during the cooldown.
    assert_eq!(v.verify(&token_with_kid(&enc, "real-kid")).await.unwrap(), "puid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// A 500-answering JWKS endpoint is an INFRA failure (no verdict on the caller's
/// token), and `login_epic` maps it to `Unavailable` (503) — never the 401 that
/// would read as bad credentials (the `verify_session` 503-not-401 precedent).
/// During the post-failure cooldown, with NO successful fetch ever, the outcome
/// stays `Infra` — and the down IdP is not hammered.
#[tokio::test(flavor = "multi_thread")]
async fn jwks_500_is_infra_and_maps_to_unavailable() {
    let (enc, _jwks) = test_key("k");
    let (url, hits) = serve_counting_jwks(500, "server error".into()).await;
    let v = OidcVerifier::new(&url, ISSUER, CLIENT_ID).unwrap();

    let err = v
        .verify(&token_with_kid(&enc, "k"))
        .await
        .expect_err("fetch failure must not verify");
    assert!(matches!(err, VerifyError::Infra(_)), "a 500 JWKS answer is an outage: {err}");

    // Cooldown after the FAILED attempt: never-fetched → still Infra, and only the
    // one fetch went out.
    let err = v.verify(&token_with_kid(&enc, "k")).await.expect_err("still no verdict");
    assert!(
        matches!(err, VerifyError::Infra(_)),
        "no successful fetch ever → cooldown miss stays Infra: {err}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a down IdP is not hammered during cooldown");

    // The service-level mapping: Infra → 503, not 401.
    let svc = epic_service(OidcVerifier::new(&url, ISSUER, CLIENT_ID).unwrap());
    let e = svc.login_epic(token_with_kid(&enc, "k")).await.unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Unavailable,
        "an IdP outage must answer 503, never 401 (bad-credentials)"
    );
}

/// The Rejected side of the mapping: a demonstrably bad token (unknown kid after a
/// fresh successful fetch) stays `Unauthorized` (401) through `login_epic`.
#[tokio::test(flavor = "multi_thread")]
async fn rejected_token_maps_to_unauthorized() {
    let (enc, jwks) = test_key("real-kid");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let svc = epic_service(OidcVerifier::new(&url, ISSUER, CLIENT_ID).unwrap());

    let e = svc.login_epic(token_with_kid(&enc, "ghost")).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);
}

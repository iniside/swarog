//! JWKS singleflight/cooldown + error-taxonomy tests for `oidc.rs`. All DB-free:
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

use crate::oidc::{short_id, IssuerMatch, OidcVerifier};
use crate::password::ArgonVerifier;
use crate::providers::{oidc_credentials, Providers, VerifyError};
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

/// Like `serve_counting_jwks` (always 200) but the body is swappable at runtime via
/// the returned handle, so a test can rotate the key set the endpoint returns between
/// fetches (exercising the TTL-triggered refetch).
async fn serve_switchable_jwks(
    initial: String,
) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<String>>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let body = Arc::new(std::sync::Mutex::new(initial));
    let counter = hits.clone();
    let body_for_handler = body.clone();
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let counter = counter.clone();
            let body = body_for_handler.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let current = body.lock().unwrap().clone();
                (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    current,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/jwks"), hits, body)
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

/// The verifier under test: the fixture JWKS endpoint, issuer prefix and audience.
fn verifier(url: &str) -> OidcVerifier {
    OidcVerifier::new(
        url,
        IssuerMatch::prefix("issuer", ISSUER).unwrap(),
        vec![CLIENT_ID.to_string()],
    )
    .unwrap()
}

/// A lazy-pool service with the epic provider configured — for the `login_epic`
/// status-mapping tests (verify fails before any DB access).
fn epic_service(verifier: OidcVerifier) -> Arc<Service> {
    let mut registry = Providers::default();
    registry.insert("epic", oidc_credentials("epic", Arc::new(verifier)));
    let providers = OnceLock::new();
    providers.set(Arc::new(registry)).ok().unwrap();
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DSN).unwrap(),
        },
        bus: Arc::new(bus::Bus::new()),
        dev_auth: false,
        providers,
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
    let v = Arc::new(verifier(&url));

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
    let v = verifier(&url);

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
    let svc = epic_service(verifier(&url));
    let e = svc.login_epic(token_with_kid(&enc, "k")).await.unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Unavailable,
        "an IdP outage must answer 503, never 401 (bad-credentials)"
    );
}

/// A stale cache triggers a refetch, and the full-set swap EXPIRES a rotated-out
/// kid: once the set is older than `JWKS_CACHE_TTL`, a token whose kid Epic rotated
/// out is rejected on the next verify (the fix's whole point) — while the newly
/// rotated-in kid verifies from the freshly fetched set, no restart needed.
#[tokio::test(flavor = "multi_thread")]
async fn stale_cache_refetches_and_rotated_out_kid_is_rejected() {
    let (enc1, jwks1) = test_key("kid-1");
    let (enc2, jwks2) = test_key("kid-2");
    let (url, hits, body) = serve_switchable_jwks(jwks1).await;
    let v = verifier(&url);

    // Warm the cache with the current key.
    assert_eq!(v.verify(&token_with_kid(&enc1, "kid-1")).await.unwrap(), "puid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Epic rotates: the endpoint now serves ONLY kid-2. Age the cache past the TTL
    // and clear the warm-up cooldown so the stale hit is permitted to refetch (the
    // stale-under-cooldown degrade-open path is covered by its own test).
    *body.lock().unwrap() = jwks2;
    v.expire_cache_for_test().await;
    v.reset_cooldown_for_test().await;

    // The stale hit refetches; kid-1 is absent from the fresh set → Rejected (not
    // served from the stale cache), and one refetch went out.
    let err = v
        .verify(&token_with_kid(&enc1, "kid-1"))
        .await
        .expect_err("a rotated-out kid must stop being accepted");
    assert!(
        matches!(err, VerifyError::Rejected(_)),
        "rotated-out kid absent from the fresh set is a bad token: {err}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2, "the stale hit forced a refetch");

    // The rotated-in kid verifies from the now-fresh cache — no further fetch (the
    // refetch was < MIN_REFRESH_INTERVAL ago, but the set is fresh and answers it).
    assert_eq!(v.verify(&token_with_kid(&enc2, "kid-2")).await.unwrap(), "puid");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

/// A FRESH cached hit never refetches: repeated verifies within `JWKS_CACHE_TTL`
/// cost exactly the one warm-up fetch.
#[tokio::test(flavor = "multi_thread")]
async fn fresh_cache_hit_does_not_refetch() {
    let (enc, jwks) = test_key("kid-1");
    let (url, hits) = serve_counting_jwks(200, jwks).await;
    let v = verifier(&url);

    for _ in 0..3 {
        assert_eq!(v.verify(&token_with_kid(&enc, "kid-1")).await.unwrap(), "puid");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh cached kid never refetches");
}

/// Freshness degrades OPEN under the cooldown: a set that is stale but still answers
/// the kid is SERVED (a valid login is not rejected), and the refresh cooldown still
/// bounds the fetch rate — no second fetch goes out while the cooldown is active.
#[tokio::test(flavor = "multi_thread")]
async fn stale_cache_under_cooldown_serves_stale_without_refetch() {
    let (enc, jwks) = test_key("kid-1");
    let (url, hits) = serve_counting_jwks(200, jwks).await;
    let v = verifier(&url);

    // Warm-up fetch stamps the refresh cooldown AND caches kid-1 fresh.
    assert_eq!(v.verify(&token_with_kid(&enc, "kid-1")).await.unwrap(), "puid");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Age the cache past the TTL — but the refresh attempt is still within
    // MIN_REFRESH_INTERVAL, so the cooldown forbids a fetch.
    v.expire_cache_for_test().await;

    // The stale set still answers kid-1: served (degrade-open), no refetch.
    assert_eq!(
        v.verify(&token_with_kid(&enc, "kid-1")).await.unwrap(),
        "puid",
        "a stale-but-answering set is served rather than rejecting a valid login"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the cooldown bounds the fetch rate");
}

/// The Rejected side of the mapping: a demonstrably bad token (unknown kid after a
/// fresh successful fetch) stays `Unauthorized` (401) through `login_epic`.
#[tokio::test(flavor = "multi_thread")]
async fn rejected_token_maps_to_unauthorized() {
    let (enc, jwks) = test_key("real-kid");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let svc = epic_service(verifier(&url));

    let e = svc.login_epic(token_with_kid(&enc, "ghost")).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);
}

// ============================================================================
// Step 4 — issuer/audience semantics, short_id truncation
// ============================================================================

/// Full control over `iss`/`aud`/`sub`, unlike `token_with_kid` above which pins
/// `ISSUER`/`CLIENT_ID` and always appends `/x` to the issuer — the exact-issuer
/// cases below need the issuer string byte-for-byte as configured.
fn token_with_claims(enc: &jsonwebtoken::EncodingKey, iss: &str, aud: &str, sub: &str, kid: &str) -> String {
    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let claims = serde_json::json!({"iss": iss, "aud": aud, "sub": sub, "exp": exp});
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

/// Both real Google issuer spellings — the scheme-ful and the legacy scheme-less
/// form — must verify under `IssuerMatch::exact`.
#[tokio::test(flavor = "multi_thread")]
async fn exact_issuer_accepts_both_google_spellings() {
    let (enc, jwks) = test_key("g-kid");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let issuer =
        IssuerMatch::exact("issuer", &["https://accounts.google.com", "accounts.google.com"])
            .unwrap();
    let v = OidcVerifier::new(&url, issuer, vec!["client".to_string()]).unwrap();

    for iss in ["https://accounts.google.com", "accounts.google.com"] {
        let token = token_with_claims(&enc, iss, "client", "sub-1", "g-kid");
        assert_eq!(v.verify(&token).await.unwrap(), "sub-1", "exact issuer {iss} must verify");
    }
}

/// The headline regression this step closes: `exact` must reject a lookalike host
/// that appends a suffix to a legitimate issuer.
#[tokio::test(flavor = "multi_thread")]
async fn exact_issuer_rejects_lookalike_suffix() {
    let (enc, jwks) = test_key("g-kid");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let issuer = IssuerMatch::exact("issuer", &["https://accounts.google.com"]).unwrap();
    let v = OidcVerifier::new(&url, issuer, vec!["client".to_string()]).unwrap();

    let token =
        token_with_claims(&enc, "https://accounts.google.com.evil.test", "client", "sub-1", "g-kid");
    let err = v.verify(&token).await.expect_err("lookalike issuer must be rejected under exact");
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

/// The same lookalike is rejected under `prefix` too — the rule matches at a path
/// boundary, so a bare-host prefix cannot be widened into a neighbouring domain.
/// Google's reason for `exact` is what `prefix` cannot EXPRESS: the scheme-less
/// spelling `accounts.google.com`, which the absolute-URL floor rejects.
#[tokio::test(flavor = "multi_thread")]
async fn prefix_issuer_rejects_google_lookalike_at_the_path_boundary() {
    let (enc, jwks) = test_key("g-kid");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let issuer = IssuerMatch::prefix("issuer", "https://accounts.google.com").unwrap();
    let v = OidcVerifier::new(&url, issuer, vec!["client".to_string()]).unwrap();

    let token =
        token_with_claims(&enc, "https://accounts.google.com.evil.test", "client", "sub-1", "g-kid");
    let err = v.verify(&token).await.expect_err("lookalike issuer must be rejected under prefix");
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

/// `prefix` accepts at a path boundary: the configured value itself and any deeper
/// path verify, while a host that merely STARTS WITH it — the lookalike shape — does
/// not, and neither does a foreign issuer.
#[tokio::test(flavor = "multi_thread")]
async fn prefix_issuer_accepts_the_issuer_and_its_paths_but_not_a_lookalike_host() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let v = verifier(&url);

    for iss in [ISSUER, &format!("{ISSUER}/v2/token")] {
        let token = token_with_claims(&enc, iss, CLIENT_ID, "sub-1", "k");
        assert_eq!(v.verify(&token).await.unwrap(), "sub-1", "prefix must accept {iss}");
    }

    for iss in ["https://not-eas.example", &format!("{ISSUER}.evil.test")] {
        let token = token_with_claims(&enc, iss, CLIENT_ID, "sub-1", "k");
        let err = v.verify(&token).await.expect_err("lookalike/foreign issuer must be rejected");
        assert!(matches!(&err, VerifyError::Rejected(_)), "issuer {iss}: wrong error {err}");
    }
}

/// The Step-1 erratum's asymmetry: `prefix` keeps the absolute-URL floor (a
/// truncated value or a hostless URL is `Err`), while `exact` accepts a bare
/// host — a truncated exact comparison cannot be widened, so the floor does not
/// apply to it.
#[test]
fn issuer_match_constructor_asymmetry_from_the_step3_erratum() {
    assert!(IssuerMatch::prefix("K", "h").is_err(), "prefix must keep the absolute-URL floor");
    assert!(
        IssuerMatch::prefix("K", "file:///x").is_err(),
        "prefix must reject a hostless absolute URL"
    );
    assert!(
        IssuerMatch::exact("K", &["accounts.google.com"]).is_ok(),
        "exact must accept a bare host — truncation cannot widen an exact match"
    );
    assert!(IssuerMatch::exact("K", &[]).is_err(), "exact must reject an empty issuer list");
    assert!(IssuerMatch::exact("K", &[""]).is_err(), "exact must reject an empty issuer value");
}

/// Trailing slashes are trimmed at construction time, so a configured value with or
/// without one is the SAME rule: both the bare (trimmed) issuer and a deeper path
/// verify.
#[tokio::test(flavor = "multi_thread")]
async fn prefix_trims_a_configured_trailing_slash() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let issuer = IssuerMatch::prefix("issuer", "https://host/v1/").unwrap();
    let v = OidcVerifier::new(&url, issuer, vec!["client".to_string()]).unwrap();

    for iss in ["https://host/v1", "https://host/v1/x"] {
        let token = token_with_claims(&enc, iss, "client", "sub-1", "k");
        assert_eq!(
            v.verify(&token).await.unwrap(),
            "sub-1",
            "trailing-slash-trimmed prefix must accept {iss}"
        );
    }
}

/// The empty-`rest` boundary arm without a dot in the lookalike: `hostage`
/// continues `host` with `age`, not with `/`, so the string-prefix match must still
/// be rejected even though nothing here looks like a subdomain suffix.
#[tokio::test(flavor = "multi_thread")]
async fn prefix_rejects_a_no_dot_string_prefix_without_a_path_boundary() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let issuer = IssuerMatch::prefix("issuer", "https://host").unwrap();
    let v = OidcVerifier::new(&url, issuer, vec!["client".to_string()]).unwrap();

    let token = token_with_claims(&enc, "https://hostage", "client", "sub-1", "k");
    let err = v
        .verify(&token)
        .await
        .expect_err("a string prefix without a path boundary must be rejected");
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("unexpected issuer")),
        "wrong rejection reason: {err}"
    );
}

/// A token whose audience is the MIDDLE entry of a multi-audience list verifies
/// — pins the `audience: String` → `audiences: Vec<String>` generalization.
#[tokio::test(flavor = "multi_thread")]
async fn verify_accepts_an_audience_from_the_middle_of_the_list() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let audiences = vec!["aud-a".to_string(), "aud-b".to_string(), "aud-c".to_string()];
    let v = OidcVerifier::new(&url, IssuerMatch::prefix("issuer", ISSUER).unwrap(), audiences).unwrap();

    let token = token_with_claims(&enc, &format!("{ISSUER}/x"), "aud-b", "sub-1", "k");
    assert_eq!(v.verify(&token).await.unwrap(), "sub-1");
}

/// A token whose audience is outside the configured list is rejected. This alone
/// does not pin `set_audience`: `jsonwebtoken` reaches the same `InvalidAudience`
/// on the `(Parsed(_), None)` arm, so it is the ACCEPT sibling above
/// (`verify_accepts_an_audience_from_the_middle_of_the_list`) that is the real proof
/// `set_audience` ran.
#[tokio::test(flavor = "multi_thread")]
async fn verify_rejects_an_audience_outside_the_list() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let audiences = vec!["aud-a".to_string(), "aud-b".to_string(), "aud-c".to_string()];
    let v = OidcVerifier::new(&url, IssuerMatch::prefix("issuer", ISSUER).unwrap(), audiences).unwrap();

    let token = token_with_claims(&enc, &format!("{ISSUER}/x"), "aud-z", "sub-1", "k");
    let err = v.verify(&token).await.expect_err("out-of-list audience must be rejected");
    assert!(
        matches!(&err, VerifyError::Rejected(e) if e.to_string().contains("InvalidAudience")),
        "wrong rejection reason: {err}"
    );
}

/// `aud` as a JSON array reaches `jsonwebtoken`'s `Audience::Multiple` branch
/// (`is_subset`), a different code path than the single-string `aud` every other
/// test in this file uses.
#[tokio::test(flavor = "multi_thread")]
async fn verify_accepts_a_multi_valued_audience_array() {
    let (enc, jwks) = test_key("k");
    let (url, _hits) = serve_counting_jwks(200, jwks).await;
    let audiences = vec!["aud-a".to_string(), "aud-b".to_string()];
    let v = OidcVerifier::new(&url, IssuerMatch::prefix("issuer", ISSUER).unwrap(), audiences).unwrap();

    let exp = (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("k".to_string());
    let claims = serde_json::json!({
        "iss": format!("{ISSUER}/x"), "aud": ["aud-a", "aud-b"], "sub": "sub-1", "exp": exp,
    });
    let token = jsonwebtoken::encode(&header, &claims, &enc).unwrap();
    assert_eq!(v.verify(&token).await.unwrap(), "sub-1");
}

/// The erratum's fail-CLOSED reasoning for empty audiences: `jsonwebtoken`'s
/// `set_audience` stores `Some(set)` unconditionally and validation is
/// `!correct_aud.contains(aud)`, so an EMPTY set matches no token — the guard
/// exists because such a verifier could never verify anything, not because an
/// empty list would mean "any" (the inverse claim already shipped once and was
/// corrected).
#[test]
fn empty_audiences_is_rejected_as_a_verifier_that_could_never_verify() {
    let err = match OidcVerifier::new(
        "https://example.test/jwks",
        IssuerMatch::prefix("issuer", ISSUER).unwrap(),
        vec![],
    ) {
        Ok(_) => panic!("an empty audience set must be rejected at construction"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("no audiences configured"), "wrong rejection reason: {err}");
}

/// A subject whose 8th BYTE falls inside a multibyte character:
/// `"aaaaaaa€xyz"` is 7 ASCII bytes then the euro sign's 3-byte UTF-8 encoding, so
/// byte offset 8 lands mid-`€` — the cut must be char-based, not byte-based.
#[test]
fn short_id_cuts_on_a_character_boundary_not_a_byte_boundary() {
    assert_eq!(short_id("aaaaaaa€xyz"), "aaaaaaa€");
}

/// Pass-through for a subject at or under the threshold; plain truncation for a
/// longer all-ASCII subject.
#[test]
fn short_id_passthrough_and_plain_truncation() {
    assert_eq!(short_id("abcdefgh"), "abcdefgh", "exactly 8 chars must pass through");
    assert_eq!(short_id("abc"), "abc", "under 8 chars must pass through");
    assert_eq!(short_id("abcdefghij"), "abcdefgh", "over 8 ASCII chars must cut at 8");
}

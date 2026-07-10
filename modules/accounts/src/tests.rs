use super::*;
use accountsapi::{Auth as _, Sessions as _};
use base64::Engine as _;

mod dev_auth_gate;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;
use sqlx::PgPool;
use std::time::Duration;

/// Fallback DSN for the lazy-pool unit tests (the live tests read `DATABASE_URL`).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// ============================================================================
// Unit: argon2id (Go parity)
// ============================================================================

#[test]
fn argon2_roundtrip() {
    let h = hash_password("hunter2").unwrap();
    assert!(verify_password(&h, "hunter2"), "correct password rejected");
    assert!(!verify_password(&h, "wrong"), "wrong password accepted");
    assert!(!verify_password("not-a-hash", "hunter2"), "garbage hash accepted");
}

/// The encoded shape must be Go's exact `$argon2id$v=19$m=65536,t=1,p=4$salt$hash`
/// prefix — the cross-implementation compatibility contract.
#[test]
fn argon2_encoded_format_matches_go() {
    let h = hash_password("pw").unwrap();
    assert!(
        h.starts_with("$argon2id$v=19$m=65536,t=1,p=4$"),
        "unexpected PHC prefix: {h}"
    );
    assert_eq!(h.split('$').count(), 6, "PHC field count: {h}");
}

/// A REAL hash produced by the Go sketch's `hashPassword("hunter2")`
/// (golang.org/x/crypto/argon2, RawStdEncoding) must verify here — the byte-level
/// parity proof that a password set under the Go backend survives the port.
#[test]
fn argon2_verifies_go_produced_hash() {
    const GO_HASH: &str =
        "$argon2id$v=19$m=65536,t=1,p=4$dl0qc2iuyVUh8PrA1R5VxQ$9RjPvA70ZfhJNxRxgi5YNn5udljM7UDx8DA7hhrSDcI";
    assert!(verify_password(GO_HASH, "hunter2"), "Go-produced hash rejected");
    assert!(!verify_password(GO_HASH, "wrong"), "Go hash verified a wrong password");
}

/// Session tokens are 32 random bytes in unpadded base64url — 43 chars, url-safe.
#[test]
fn token_is_32_bytes_base64url() {
    let t = store::new_token();
    assert_eq!(t.len(), 43);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&t)
        .expect("token must be valid base64url");
    assert_eq!(bytes.len(), 32);
    assert_ne!(store::new_token(), t, "tokens must not repeat");
}

// ============================================================================
// Unit: OIDC verifier against a LOCAL JWKS (proves the Epic federation logic with
// no dependency on Epic) — the port of Go's TestOIDCVerifier.
// ============================================================================

/// A fresh RSA test key: the PKCS#8 PEM for signing (jsonwebtoken) and the JWKS
/// document for verifying (the Go `buildJWKS` twin).
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

/// Serves `body` at `/jwks` on an ephemeral local port; returns the URL.
async fn serve_jwks(body: String) -> String {
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let body = body.clone();
            async move { ([(axum::http::header::CONTENT_TYPE, "application/json")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/jwks")
}

fn sign(enc: &jsonwebtoken::EncodingKey, kid: &str, claims: serde_json::Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(&header, &claims, enc).unwrap()
}

fn future_exp() -> i64 {
    (std::time::SystemTime::now() + Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn oidc_verifier_accepts_valid_and_rejects_bad_claims() {
    const KID: &str = "test-key";
    let (enc, jwks) = test_key(KID);
    let url = serve_jwks(jwks).await;
    let v = OidcVerifier::new(&url, "https://api.epicgames.dev", "client-123").unwrap();
    let exp = future_exp();

    // Valid token → the subject comes back.
    let good = sign(
        &enc,
        KID,
        serde_json::json!({"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "PUID-1", "exp": exp}),
    );
    assert_eq!(v.verify(&good).await.unwrap(), "PUID-1");

    // Each corrupted claim must be rejected.
    let bad = [
        ("wrong aud", serde_json::json!({"iss": "https://api.epicgames.dev/x", "aud": "other", "sub": "s", "exp": exp})),
        ("expired", serde_json::json!({"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "s", "exp": exp - 7200})),
        ("bad issuer", serde_json::json!({"iss": "https://evil.example/x", "aud": "client-123", "sub": "s", "exp": exp})),
        ("missing sub", serde_json::json!({"iss": "https://api.epicgames.dev/x", "aud": "client-123", "exp": exp})),
        ("missing exp", serde_json::json!({"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "s"})),
    ];
    for (name, claims) in bad {
        assert!(
            v.verify(&sign(&enc, KID, claims)).await.is_err(),
            "{name}: token accepted, want rejected"
        );
    }
}

/// `alg=none` (and any non-RS256/ES256 alg) must be refused — the classic JWT
/// downgrade attack. An unsigned token is crafted by hand (jsonwebtoken cannot
/// mint one, which is the point).
#[tokio::test]
async fn oidc_verifier_rejects_alg_none() {
    const KID: &str = "k";
    let (_enc, jwks) = test_key(KID);
    let url = serve_jwks(jwks).await;
    let v = OidcVerifier::new(&url, "https://api.epicgames.dev", "client-123").unwrap();

    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header = b64(br#"{"alg":"none","typ":"JWT"}"#);
    let claims = b64(
        format!(
            r#"{{"iss":"https://api.epicgames.dev/x","aud":"client-123","sub":"s","exp":{}}}"#,
            future_exp()
        )
        .as_bytes(),
    );
    let none_token = format!("{header}.{claims}.");
    assert!(v.verify(&none_token).await.is_err(), "alg=none accepted, want rejected");
}

// ============================================================================
// Unit: service validation paths that return BEFORE any DB work.
// ============================================================================

/// A service over a lazy pool + a transport-less bus — for the validation tests.
fn lazy_service() -> Arc<Service> {
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
        dev_auth: true,
        epic: OnceLock::new(),
    })
}

#[tokio::test]
async fn register_requires_email_and_password() {
    let svc = lazy_service();
    let e = svc
        .register(String::new(), "pw".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
    let e = svc
        .register("a@x.io".into(), String::new(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
}

#[tokio::test]
async fn login_epic_requires_token_and_configured_provider() {
    let svc = lazy_service();
    let e = svc.login_epic(String::new()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
    // Provider not configured (epic OnceLock empty) → typed Unavailable, no panic.
    let e = svc.login_epic("some.jwt.here".into()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unavailable);
}

#[tokio::test]
async fn me_requires_identity() {
    let svc = lazy_service();
    let e = svc.me(Identity::none()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
}

// ============================================================================
// Live Postgres integration (the local DB is the test DB) — port of Go's
// TestStoreRegisterLoginSession / TestStoreFindOrCreateExternal /
// TestEpicOAuthLinkFlow, plus the durable player.registered assertions.
// ============================================================================

/// Opens the local Postgres; returns `None` (printing a skip line) when
/// unreachable, so the suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — accounts DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the asyncevents (durable plane's event log) and accounts schemas
/// EXACTLY ONCE per test binary (concurrent idempotent DDL deadlocks on catalog
/// locks — same serialization as the characters tests).
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
            let ctx = Context::with_db(pool.clone());
            let a = Accounts::new();
            a.register(&ctx).unwrap();
            a.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// Builds a real durable plane over the live pool: schemas migrated once, then the
/// asyncevents `bus::Transport` is injected at `Context` construction (needed before any
/// `emit_tx`), and accounts registers against the same ctx.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());

    // Build the service directly with dev-auth forced ON — the fixture must NOT ride
    // the `ACCOUNTS_DEV_AUTH` env default, which is now fail-closed (OFF). Same struct
    // route as `dev_auth_gate::gated_service`, but wired to the ctx's transport-backed
    // bus so `register`'s durable `emit_tx` lands in the shared event log.
    let svc = Arc::new(Service {
        store: Store { pool: pool.clone() },
        bus: ctx.bus().clone(),
        dev_auth: true,
        epic: OnceLock::new(),
    });
    (ctx, svc)
}

/// A unique suffix per test so parallel runs never collide on the email/subject
/// unique keys.
fn suffix() -> String {
    store::new_token()[..12].to_string()
}

async fn cleanup_player(pool: &PgPool, player_id: &str) {
    // players CASCADEs to identities + sessions inside the accounts schema.
    let _ = sqlx::query("DELETE FROM accounts.players WHERE id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await;
    let _ = asyncevents::testing::cleanup_events(pool, "player_id", player_id).await;
}

async fn registered_events(pool: &PgPool, player_id: &str) -> i64 {
    asyncevents::testing::events_count(pool, "player.registered", "player_id", player_id)
        .await
        .unwrap()
}

/// THE ATOMIC EMIT PROOF + the register/login/session round-trip: register writes
/// the player+identity rows AND the `player.registered` log event in one tx;
/// login verifies the stored argon2 hash; the minted session resolves back to the
/// player; garbage tokens resolve to nobody; a duplicate email is Conflict.
#[tokio::test]
async fn register_login_session_roundtrip_with_durable_event() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("u-{}@test.local", suffix());

    let sess = svc
        .register(email.clone(), "secret".into(), "Tester".into())
        .await
        .unwrap();
    assert!(!sess.player_id.is_empty());
    assert_eq!(sess.token.len(), 43);

    // Durable rule: exactly one player.registered log event, provider dev.
    assert_eq!(registered_events(&pool, &sess.player_id).await, 1);

    // Duplicate email → Conflict, and no second event.
    let e = svc
        .register(email.clone(), "other".into(), "Tester".into())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Conflict);

    // Login with the right/wrong password; unknown email is the same 401.
    let s2 = svc.login(email.clone(), "secret".into()).await.unwrap();
    assert_eq!(s2.player_id, sess.player_id);
    let e = svc.login(email.clone(), "nope".into()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);
    let e = svc
        .login(format!("ghost-{}@test.local", suffix()), "x".into())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);

    // The minted session verifies to the player; garbage does not.
    assert_eq!(
        svc.verify_session(s2.token.clone()).await.unwrap(),
        Some(sess.player_id.clone())
    );
    assert_eq!(svc.verify_session("garbage-token".into()).await.unwrap(), None);

    // me returns the flattened view with the dev identity.
    let me = svc.me(Identity::player(&sess.player_id)).await.unwrap();
    assert_eq!(me.player_id, sess.player_id);
    assert_eq!(me.display_name, "Tester");
    assert!(me
        .identities
        .iter()
        .any(|i| i.provider == "dev" && i.subject == email));

    cleanup_player(&pool, &sess.player_id).await;
}

/// Session TTL: a session whose `expires_at` has passed no longer resolves (Go's
/// `expires_at > now()` guard — forced by rewinding the row rather than waiting 30
/// days).
#[tokio::test]
async fn expired_session_does_not_verify() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("ttl-{}@test.local", suffix()), "pw".into(), String::new())
        .await
        .unwrap();
    assert_eq!(
        svc.verify_session(sess.token.clone()).await.unwrap(),
        Some(sess.player_id.clone())
    );

    sqlx::query("UPDATE accounts.sessions SET expires_at = now() - interval '1 minute' WHERE token = $1")
        .bind(&sess.token)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(svc.verify_session(sess.token.clone()).await.unwrap(), None);

    cleanup_player(&pool, &sess.player_id).await;
}

/// find_or_create_external: first sight provisions (created=true, ONE durable
/// player.registered), second sight maps to the SAME player (created=false, no
/// second event) — Go's TestStoreFindOrCreateExternal plus the durable assertion.
#[tokio::test]
async fn find_or_create_external_is_idempotent() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let sub = format!("puid-{}", suffix());

    let (p, created) = svc
        .find_or_create_external("epic", &sub, "epic:new")
        .await
        .unwrap();
    assert!(created, "first login must provision");
    assert_eq!(registered_events(&pool, &p.id).await, 1);

    let (again, created2) = svc
        .find_or_create_external("epic", &sub, "epic:new")
        .await
        .unwrap();
    assert!(!created2, "second login must not provision");
    assert_eq!(again.id, p.id, "same identity mapped to different players");
    assert_eq!(registered_events(&pool, &p.id).await, 1, "no second event");

    cleanup_player(&pool, &p.id).await;
}

/// Identity linking: an external identity attaches to an existing player; linking
/// the SAME (provider, subject) again — to anyone — is Taken.
#[tokio::test]
async fn link_identity_attaches_and_rejects_duplicates() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let sess = svc
        .register(format!("link-{}@test.local", suffix()), "pw".into(), "Linker".into())
        .await
        .unwrap();
    let sub = format!("epicacct-{}", suffix());

    svc.store.link_identity(&sess.player_id, "epic", &sub).await.unwrap();
    let ids = svc.store.identities_of(&sess.player_id).await.unwrap();
    assert!(ids.iter().any(|i| i.provider == "epic" && i.subject == sub));

    let err = svc
        .store
        .link_identity(&sess.player_id, "epic", &sub)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Taken));

    cleanup_player(&pool, &sess.player_id).await;
}

/// Drives the whole Epic OAuth LINK flow against a mock Epic (local JWKS + local
/// token endpoint): the callback exchanges the code, verifies the id_token and
/// links the Epic identity to the session's player — no real Epic (Go's
/// TestEpicOAuthLinkFlow).
#[tokio::test(flavor = "multi_thread")]
async fn epic_oauth_link_flow_end_to_end() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    const KID: &str = "k1";
    const CLIENT_ID: &str = "client-xyz";
    const ISSUER: &str = "https://eas.example";
    let epic_acct = format!("epicacct-{}", suffix());

    let (enc, jwks) = test_key(KID);
    let jwks_url = serve_jwks(jwks).await;

    // A local token endpoint answering the code exchange with a self-minted id_token.
    let id_token = sign(
        &enc,
        KID,
        serde_json::json!({"iss": format!("{ISSUER}/x"), "aud": CLIENT_ID, "sub": epic_acct, "exp": future_exp()}),
    );
    let token_body = serde_json::json!({ "id_token": id_token }).to_string();
    let token_app = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let body = token_body.clone();
            async move { ([(axum::http::header::CONTENT_TYPE, "application/json")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, token_app).await.unwrap();
    });

    let verifier = Arc::new(OidcVerifier::new(&jwks_url, ISSUER, CLIENT_ID).unwrap());
    let oauth = Arc::new(
        epic_oauth::EpicOAuth::new(
            CLIENT_ID.into(),
            "secret".into(),
            "http://localhost/cb".into(),
            "http://localhost/authorize".into(),
            token_url,
            verifier,
        )
        .unwrap(),
    );

    // A logged-in dev player to link onto.
    let sess = svc
        .register(format!("oauth-{}@test.local", suffix()), "pw".into(), "Linker".into())
        .await
        .unwrap();
    let state = oauth.new_state(sess.token.clone()); // LINK flow bound to that session

    // Drive the callback route through the mounted router — the real HTTP surface.
    let app = epic_oauth::router(oauth, svc.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{addr}/accounts/epic/callback?code=abc&state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "callback must redirect");
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/?epic=linked"
    );

    let ids = svc.store.identities_of(&sess.player_id).await.unwrap();
    assert!(
        ids.iter().any(|i| i.provider == "epic" && i.subject == epic_acct),
        "epic identity not linked to player; got {ids:?}"
    );

    cleanup_player(&pool, &sess.player_id).await;
}

/// An OAuth state is single-use and expires; an unknown state is rejected.
#[test]
fn oauth_state_is_single_use() {
    let verifier = Arc::new(OidcVerifier::new("http://localhost/jwks", "iss", "aud").unwrap());
    let oauth = epic_oauth::EpicOAuth::new(
        "cid".into(),
        "sec".into(),
        "http://localhost/cb".into(),
        "http://localhost/authorize".into(),
        "http://localhost/token".into(),
        verifier,
    )
    .unwrap();
    let s = oauth.new_state("tok".into());
    assert_eq!(oauth.take_state(&s), Some("tok".into()));
    assert_eq!(oauth.take_state(&s), None, "state must be single-use");
    assert_eq!(oauth.take_state("unknown"), None);
}

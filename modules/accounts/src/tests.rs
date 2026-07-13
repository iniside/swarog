use super::*;
use accountsapi::{Auth as _, Sessions as _};
use base64::Engine as _;

mod dev_auth_gate;
mod prune;
use crate::password::verify_password;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;
use sqlx::PgPool;
use std::sync::Mutex;
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
    lazy_service_with_verifier(Arc::new(ArgonVerifier))
}

/// The verifier-injecting twin (admin's `wired_with_verifier` shape): recording and
/// gated fakes drive the decoy-path and permit-lifetime tests without real argon2.
fn lazy_service_with_verifier(verifier: Arc<dyn PasswordVerifier>) -> Arc<Service> {
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
        dev_auth: true,
        epic: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier,
    })
}

/// A recording fake: logs every (encoded, candidate) pair, never matches.
#[derive(Default)]
struct RecordingVerifier {
    calls: Mutex<Vec<(String, String)>>,
}

impl PasswordVerifier for RecordingVerifier {
    fn verify(&self, encoded: &str, password: &str) -> bool {
        self.calls.lock().unwrap().push((encoded.to_string(), password.to_string()));
        false
    }
}

/// A verifier that reports when `verify` has started and then blocks until the test
/// releases it — lets the test freeze a login mid-Argon2 deterministically (admin's
/// fixture, duplicated per the fortress rule).
struct GatedVerifier {
    started: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PasswordVerifier for GatedVerifier {
    fn verify(&self, _encoded: &str, _password: &str) -> bool {
        self.started.send(()).expect("test alive");
        let _ = self.release.lock().unwrap().recv();
        false
    }
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
async fn over_cap_session_tokens_short_circuit_before_sql_by_bytes() {
    let store = Store {
        pool: PgPool::connect_lazy(
            "postgres://gamebackend:gamebackend@127.0.0.1:1/no-session-cap-test",
        )
        .unwrap(),
    };
    let ascii = "a".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);
    let multibyte = "é".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES / 2 + 1);
    assert!(multibyte.len() > accountsapi::MAX_SESSION_TOKEN_BYTES);

    for token in [ascii, multibyte] {
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            store.player_by_session(&token),
        )
        .await
        .expect("over-cap lookup touched the dead lazy pool")
        .unwrap();
        assert!(result.is_none());
    }
}

#[tokio::test]
async fn me_requires_identity() {
    let svc = lazy_service();
    let e = svc.me(Identity::none()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
}

/// Register rejects over-cap inputs BEFORE hashing (400, not a 64 MiB argon2 run
/// on attacker-chosen input length).
#[tokio::test(flavor = "multi_thread")]
async fn register_rejects_over_cap_inputs() {
    let svc = lazy_service();
    let e = svc
        .register(format!("{}@x.io", "a".repeat(321)), "pw".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
    let e = svc
        .register("a@x.io".into(), "p".repeat(1025), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Invalid);
}

/// Invalid login input (over-cap email) still performs exactly ONE verify — against
/// the DECOY hash with the FIXED decoy candidate (never the caller's password) —
/// and answers the same generic 401. DB-free: invalid input skips the identity
/// fetch entirely. Status-identity + call-recording, not timing.
#[tokio::test(flavor = "multi_thread")]
async fn invalid_login_input_takes_decoy_verify_path() {
    let verifier = Arc::new(RecordingVerifier::default());
    let svc = lazy_service_with_verifier(verifier.clone());
    let e = svc
        .login(format!("{}@x.io", "a".repeat(321)), "real-secret".into())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);
    let calls = verifier.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one verify per admitted login");
    assert_eq!(calls[0].0, *DUMMY_HASH, "invalid input must verify the decoy hash");
    assert_eq!(
        calls[0].1, DECOY_CANDIDATE,
        "the caller's password must never be verified against a decoy"
    );
}

/// The RAM-cap regression (admin 5844831's twin): `spawn_blocking` is NOT cancelled
/// when its JoinHandle drops, so if the argon permit lived in the login's async
/// frame a client disconnect would release it while the detached 64 MiB hash keeps
/// running. The permit must be owned by the blocking closure — released only AFTER
/// the hash completes, even when the caller future is dropped mid-verify.
#[tokio::test(flavor = "multi_thread")]
async fn argon_permit_survives_login_cancellation_until_hash_completes() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let verifier = Arc::new(GatedVerifier {
        started: started_tx,
        release: Mutex::new(release_rx),
    });
    let svc = lazy_service_with_verifier(verifier);
    assert_eq!(svc.argon_permits.available_permits(), 2);

    // Over-cap email → invalid input → decoy verify path, no DB touched.
    let task_svc = svc.clone();
    let login = tokio::spawn(async move {
        task_svc
            .login(format!("{}@x.io", "a".repeat(321)), "pw".into())
            .await
    });
    // Wait until the login is provably inside the blocking verify.
    tokio::task::spawn_blocking(move || started_rx.recv().expect("verify started"))
        .await
        .unwrap();
    assert_eq!(svc.argon_permits.available_permits(), 1);

    // Simulate the client disconnect: abort drops the login future at its `.await`
    // on the spawn_blocking JoinHandle; the blocking hash keeps running.
    login.abort();
    let err = login.await.expect_err("login task was aborted mid-verify");
    assert!(err.is_cancelled());
    assert_eq!(
        svc.argon_permits.available_permits(),
        1,
        "cancelling the request must NOT release the argon permit while the hash still runs"
    );

    // Let the hash finish; only then may the permit return.
    release_tx.send(()).expect("verifier still blocked");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while svc.argon_permits.available_permits() != 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "permit was not released after the blocking verify completed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Concurrency shape: with both argon permits busy the 3rd..32nd logins QUEUE on
/// the argon semaphore (still admitted), while the 33rd is shed at the admission
/// bound with `Unavailable` — reject, never unbounded queueing. DB-free (over-cap
/// emails take the decoy path without an identity fetch).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn third_login_queues_and_thirty_third_is_shed() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let verifier = Arc::new(GatedVerifier {
        started: started_tx,
        release: Mutex::new(release_rx),
    });
    let svc = lazy_service_with_verifier(verifier);

    let mut logins = Vec::new();
    for _ in 0..32 {
        let task_svc = svc.clone();
        logins.push(tokio::spawn(async move {
            task_svc
                .login(format!("{}@x.io", "a".repeat(321)), "pw".into())
                .await
        }));
    }
    // Two verifies running (both argon permits held)...
    for _ in 0..2 {
        let rx = started_rx.recv_timeout(Duration::from_secs(5));
        // recv_timeout blocks this test thread briefly; the runtime has 4 workers.
        rx.expect("two logins must reach the blocking verify");
    }
    // ...and all 32 admission slots taken (the other 30 queue on the argon permits).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while svc.login_slots.available_permits() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "all 32 logins must hold an admission slot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(svc.argon_permits.available_permits(), 0);

    // The 33rd concurrent login is shed immediately — Unavailable, no queueing.
    let e = svc
        .login(format!("{}@x.io", "a".repeat(321)), "pw".into())
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Unavailable,
        "the 33rd concurrent login must be shed at the admission bound"
    );

    // Release every gated verify; all 32 admitted logins (including the queued
    // 3rd..32nd) complete with the generic 401 — queued means served, not dropped.
    for _ in 0..32 {
        release_tx.send(()).expect("verifier still gated");
    }
    for login in logins {
        let e = login.await.unwrap().unwrap_err();
        assert_eq!(e.status, opsapi::Status::Unauthorized);
    }
    assert_eq!(svc.login_slots.available_permits(), 32);
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
    wired_with_verifier(pool, Arc::new(ArgonVerifier)).await
}

async fn wired_with_verifier(
    pool: &PgPool,
    verifier: Arc<dyn PasswordVerifier>,
) -> (Context, Arc<Service>) {
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
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier,
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

#[tokio::test]
async fn session_token_collision_rolls_back_registration_and_event() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let owner = svc
        .register(
            format!("owner-{}@test.local", suffix()),
            "pw".into(),
            "Collision owner".into(),
        )
        .await
        .unwrap();
    let attempted_email = format!("collision-{}@test.local", suffix());
    let attempted_display = format!("Collision rollback {}", suffix());

    let result = svc
        .register_hashed_with_token(
            &attempted_email,
            &attempted_display,
            "unused-test-hash",
            owner.token.clone(),
        )
        .await;
    let identity_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts.identities WHERE provider = 'dev' AND subject = $1",
    )
    .bind(&attempted_email)
    .fetch_one(&pool)
    .await
    .unwrap();
    let player_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts.players WHERE display_name = $1")
            .bind(&attempted_display)
            .fetch_one(&pool)
            .await
            .unwrap();
    let event_rows = asyncevents::testing::events_count(
        &pool,
        "player.registered",
        "display_name",
        &attempted_display,
    )
    .await
    .unwrap();

    cleanup_player(&pool, &owner.player_id).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "display_name", &attempted_display).await;

    assert!(result.is_err(), "duplicate session token must fail registration");
    assert_eq!(identity_rows, 0, "identity insert must roll back");
    assert_eq!(player_rows, 0, "player insert must roll back");
    assert_eq!(event_rows, 0, "player.registered append must roll back");
}

/// An UNKNOWN email (valid input, no identity row) takes the decoy verify path:
/// exactly one verifier call, against the DECOY hash with the FIXED decoy candidate
/// — so unknown-email costs the same argon2 work as wrong-password (no timing
/// oracle), asserted by call-recording + status-identity, not timing.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_email_takes_decoy_verify_path() {
    let Some(pool) = test_pool().await else { return };
    let verifier = Arc::new(RecordingVerifier::default());
    let (_ctx, svc) = wired_with_verifier(&pool, verifier.clone()).await;

    let e = svc
        .login(format!("ghost-{}@test.local", suffix()), "real-secret".into())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);

    let calls = verifier.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one verify per admitted login");
    assert_eq!(calls[0].0, *DUMMY_HASH, "unknown email must verify the decoy hash");
    assert_eq!(
        calls[0].1, DECOY_CANDIDATE,
        "the caller's password must never be verified against a decoy"
    );
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

/// Spins up a mock Epic (local JWKS + token endpoint minting an id_token whose
/// `sub` is `subject`) and the accounts callback router, returning a redirect-less
/// client, the callback base URL, and the `EpicOAuth` handle to mint states on.
async fn epic_link_harness(
    svc: Arc<Service>,
    subject: &str,
) -> (reqwest::Client, String, Arc<epic_oauth::EpicOAuth>) {
    const KID: &str = "k1";
    const CLIENT_ID: &str = "client-xyz";
    const ISSUER: &str = "https://eas.example";

    let (enc, jwks) = test_key(KID);
    let jwks_url = serve_jwks(jwks).await;

    let id_token = sign(
        &enc,
        KID,
        serde_json::json!({"iss": format!("{ISSUER}/x"), "aud": CLIENT_ID, "sub": subject, "exp": future_exp()}),
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

    let app = epic_oauth::router(oauth.clone(), svc);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/accounts/epic/callback");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    (client, base, oauth)
}

/// A LINK flow whose Epic account is already bound to a DIFFERENT player must NOT
/// read as success: the callback redirects to `/?epic=error` and the linking player
/// gains no epic identity (the false-success bug this fix closes).
#[tokio::test(flavor = "multi_thread")]
async fn epic_link_cross_player_collision_is_error() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let epic_acct = format!("epicacct-{}", suffix());

    // Player A already owns the Epic identity.
    let a = svc
        .register(format!("a-{}@test.local", suffix()), "pw".into(), "A".into())
        .await
        .unwrap();
    svc.store.link_identity(&a.player_id, "epic", &epic_acct).await.unwrap();

    // Player B, logged in, tries to link the SAME Epic account.
    let b = svc
        .register(format!("b-{}@test.local", suffix()), "pw".into(), "B".into())
        .await
        .unwrap();

    let (client, base, oauth) = epic_link_harness(svc.clone(), &epic_acct).await;
    let state = oauth.new_state(b.token.clone()); // LINK bound to B's session

    let resp = client
        .get(format!("{base}?code=abc&state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "callback must redirect");
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/?epic=error",
        "a cross-player collision must not report linked"
    );

    let ids = svc.store.identities_of(&b.player_id).await.unwrap();
    assert!(
        !ids.iter().any(|i| i.provider == "epic"),
        "B must not gain an epic identity on collision; got {ids:?}"
    );

    cleanup_player(&pool, &a.player_id).await;
    cleanup_player(&pool, &b.player_id).await;
}

/// A LINK flow re-linking the player's OWN already-linked Epic account is
/// idempotent: `/?epic=linked` and no duplicate identity row.
#[tokio::test(flavor = "multi_thread")]
async fn epic_link_same_player_is_idempotent() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let epic_acct = format!("epicacct-{}", suffix());

    let a = svc
        .register(format!("a-{}@test.local", suffix()), "pw".into(), "A".into())
        .await
        .unwrap();
    svc.store.link_identity(&a.player_id, "epic", &epic_acct).await.unwrap();

    let (client, base, oauth) = epic_link_harness(svc.clone(), &epic_acct).await;
    let state = oauth.new_state(a.token.clone()); // LINK bound to A's own session

    let resp = client
        .get(format!("{base}?code=abc&state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "callback must redirect");
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/?epic=linked",
        "re-linking one's own identity must succeed"
    );

    let ids = svc.store.identities_of(&a.player_id).await.unwrap();
    let epic_rows = ids
        .iter()
        .filter(|i| i.provider == "epic" && i.subject == epic_acct)
        .count();
    assert_eq!(epic_rows, 1, "re-link must not duplicate the identity row; got {ids:?}");

    cleanup_player(&pool, &a.player_id).await;
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

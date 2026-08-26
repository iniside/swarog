//! The guest/device provider (`guest.rs`): the previously-wrong branches an audit found
//! unexecuted after Step 7 landed — `GuestCredentials::verify`'s four outcomes,
//! `mint_ticket`, `create_guest`, and `Store::guest_identity_matches`. A child of `tests`
//! so it reuses that module's live-DB harness (`test_pool`/`wired`/`suffix`/
//! `cleanup_player`) and its once-per-binary schema serialization.

use super::*;

use crate::guest::{is_minted_subject, mint_ticket, secret_hash, MAX_GUEST_CREDENTIAL_BYTES};
use crate::providers::{Providers, GUEST};
use crate::store::Store;

// ============================================================================
// Pure, zero-I/O: `is_minted_subject` (the e9ac7ec fix's authority).
// ============================================================================

#[test]
fn is_minted_subject_accepts_every_shape_this_backend_mints() {
    for _ in 0..64 {
        let ticket = mint_ticket();
        assert!(
            is_minted_subject(&ticket.subject),
            "a freshly minted subject was rejected: {}",
            ticket.subject
        );
    }
}

/// The exact defect e9ac7ec closed: a NUL byte is valid JSON but not a valid Postgres
/// `text` bind. `is_minted_subject` must reject it on shape alone, before any SQL.
#[test]
fn is_minted_subject_rejects_a_nul_byte() {
    let poisoned = "00000000-0000-4000-8000-00000000000\0";
    assert!(!is_minted_subject(poisoned));
    assert!(!is_minted_subject("\0"));
}

#[test]
fn is_minted_subject_rejects_wrong_group_lengths_and_uppercase() {
    assert!(!is_minted_subject(""));
    assert!(!is_minted_subject("not-a-uuid-at-all"));
    // One hex digit short in the third group.
    assert!(!is_minted_subject("00000000-0000-400-8000-000000000000"));
    // Trailing garbage after a structurally valid shape.
    assert!(!is_minted_subject("00000000-0000-4000-8000-000000000000-extra"));
    // Uppercase hex — `new_subject` only ever renders lowercase.
    assert!(!is_minted_subject("00000000-0000-4000-8000-00000000000A"));
}

// ============================================================================
// A dead-pool guest registry — proves "did this reach the store" WITHOUT a live
// Postgres and WITHOUT hanging: any query against it surfaces fast as `Infra`.
// ============================================================================

/// An unroutable DSN behind a short `acquire_timeout` (the repo's `dead_service_at`
/// precedent): sqlx's acquire loop otherwise retries ConnectionRefused as "server
/// starting up" for its 30s default deadline.
const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/accounts-guest-dead";

fn dead_guest_service() -> Arc<Service> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy(DEAD_DSN)
        .unwrap();
    let mut registry = Providers::default();
    registry.insert(GUEST, crate::guest::guest_credentials(Store { pool: pool.clone() }));
    let providers = OnceLock::new();
    providers.set(Arc::new(registry)).ok().unwrap();
    Arc::new(Service {
        store: Store { pool },
        bus: Arc::new(Bus::new()),
        dev_auth: true,
        providers,
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    })
}

/// A1 (through the op): a NUL-poisoned subject must answer the SAME 401 a wrong secret
/// gets, never the 503 that means "our IdP is down" — on a DEAD store pool, so if the
/// `is_minted_subject` guard were ever removed the raw subject would reach
/// `guest_identity_matches`, the dead pool would error, and this would flip to 503.
#[tokio::test(flavor = "multi_thread")]
async fn nul_poisoned_subject_is_401_not_503_even_against_a_dead_store() {
    let svc = dead_guest_service();
    let credential = "00000000-0000-4000-8000-00000000000\0.some-secret".to_string();

    let e = svc
        .login_federated(GUEST.to_string(), credential)
        .await
        .expect_err("a malformed subject must never verify");

    assert_eq!(
        e.status,
        opsapi::Status::Unauthorized,
        "a NUL-poisoned subject must read as a rejected credential, not an outage: {e:?}"
    );
    assert_eq!(e.msg, "invalid credential");
}

/// A3: the `Infra` arm the A1 fix sits beside must stay reachable — a VALID-shaped
/// subject against a genuinely dead store must still answer 503, distinct from the 401
/// above. Proves the shape-check didn't swallow real infrastructure failures too.
#[tokio::test(flavor = "multi_thread")]
async fn genuine_store_failure_on_a_well_shaped_ticket_is_503() {
    let svc = dead_guest_service();
    let ticket = mint_ticket();

    let e = svc
        .login_federated(GUEST.to_string(), ticket.credential)
        .await
        .expect_err("a dead store must not verify anything");

    assert_eq!(
        e.status,
        opsapi::Status::Unavailable,
        "a well-shaped ticket against an unreachable store is an outage, not bad credentials: {e:?}"
    );
    assert_eq!(e.msg, "identity provider unavailable");
}

/// A4: the guest credential cap is enforced BEFORE the store is ever touched. The
/// subject half is deliberately well-shaped (so `is_minted_subject` alone can't be
/// the reason for the rejection) and the whole credential is over
/// `MAX_GUEST_CREDENTIAL_BYTES`; the store is DEAD, so if the cap check ever stopped
/// running first, the request would reach `guest_identity_matches` and this would
/// observe 503, never 400.
#[tokio::test(flavor = "multi_thread")]
async fn over_cap_credential_is_400_before_any_store_work() {
    let svc = dead_guest_service();
    let ticket = mint_ticket();
    let over_cap = format!("{}.{}", ticket.subject, "a".repeat(200));
    assert!(over_cap.len() > MAX_GUEST_CREDENTIAL_BYTES);

    let e = svc
        .login_federated(GUEST.to_string(), over_cap)
        .await
        .expect_err("an over-cap ticket must never verify");

    assert_eq!(e.status, opsapi::Status::Invalid);
    assert_eq!(e.msg, "credential too long");
}

// ============================================================================
// Live-DB: mint -> login round trip, the no-oracle 401, and digest preservation.
// ============================================================================

async fn wired_with_guest(pool: &PgPool) -> (Context, Arc<Service>) {
    let (ctx, svc) = wired(pool).await;
    let mut registry = Providers::default();
    registry.insert(GUEST, crate::guest::guest_credentials(Store { pool: pool.clone() }));
    svc.providers.set(Arc::new(registry)).ok().unwrap();
    (ctx, svc)
}

/// The Step 8(a) round trip: `create_guest` provisions a player and reveals a ticket
/// exactly once; the returning device replays it through `login_federated("guest", …)`
/// and resolves to the SAME player, with a durable `player.registered` on the
/// provisioning tx only.
#[tokio::test(flavor = "multi_thread")]
async fn mint_then_login_round_trip() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let minted = svc.create_guest().await.unwrap();
    assert!(!minted.device_secret.is_empty());
    assert_eq!(registered_events(&pool, &minted.player_id).await, 1);

    let replayed = svc
        .login_federated(GUEST.to_string(), minted.device_secret.clone())
        .await
        .unwrap();
    assert_eq!(replayed.player_id, minted.player_id);
    assert_ne!(replayed.token, minted.token, "each login mints its own bearer");
    // The second login provisioned nothing new.
    assert_eq!(registered_events(&pool, &minted.player_id).await, 1);

    cleanup_player(&pool, &minted.player_id).await;
}

/// A wrong secret against a real, existing guest subject.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_is_unauthorized() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let minted = svc.create_guest().await.unwrap();
    let (subject, _secret) = minted.device_secret.split_once('.').unwrap();
    let wrong = format!("{subject}.not-the-real-secret");

    let e = svc.login_federated(GUEST.to_string(), wrong).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);

    cleanup_player(&pool, &minted.player_id).await;
}

/// The no-oracle property, executed: an unknown subject (well-shaped, never minted)
/// and a wrong secret against a REAL subject must be byte-for-byte the same response
/// — not merely both 401. `guest_identity_matches`'s single WHERE predicate is what
/// makes this true; a two-step "does the subject exist" then "does the secret match"
/// implementation would leak a timing/response oracle even while both branches still
/// answered 401.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_subject_and_wrong_secret_are_byte_identical_401() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let minted = svc.create_guest().await.unwrap();
    let (real_subject, _secret) = minted.device_secret.split_once('.').unwrap();

    let unknown_ticket = format!("{}.some-secret", mint_ticket().subject);
    let wrong_secret_ticket = format!("{real_subject}.not-the-real-secret");

    let unknown_err = svc
        .login_federated(GUEST.to_string(), unknown_ticket)
        .await
        .unwrap_err();
    let wrong_err = svc
        .login_federated(GUEST.to_string(), wrong_secret_ticket)
        .await
        .unwrap_err();

    assert_eq!(unknown_err.status, wrong_err.status);
    assert_eq!(unknown_err.msg, wrong_err.msg);
    assert_eq!(
        format!("{unknown_err:?}"),
        format!("{wrong_err:?}"),
        "an unknown subject must be indistinguishable from a wrong secret on a real one"
    );

    cleanup_player(&pool, &minted.player_id).await;
}

/// A malformed credential (no `.`) is a 401, never a panic — `split_once` returns
/// `None` and the verifier maps it to the same `Rejected` outcome.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_credential_without_a_dot_is_unauthorized_not_a_panic() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let e = svc
        .login_federated(GUEST.to_string(), "no-dot-here".to_string())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);
}

/// A second `create_guest` call yields a DISTINCT player — no accidental collision on
/// the minted UUID subject or on the digest.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_create_guest_yields_a_distinct_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let a = svc.create_guest().await.unwrap();
    let b = svc.create_guest().await.unwrap();
    assert_ne!(a.player_id, b.player_id);
    assert_ne!(a.device_secret, b.device_secret);

    cleanup_player(&pool, &a.player_id).await;
    cleanup_player(&pool, &b.player_id).await;
}

/// The secret is not re-derivable from any read path: `me()` never carries it (the
/// wire type structurally has no such field), and replaying the STORED DIGEST itself
/// as though it were the secret must fail — proving the digest cannot be used to log
/// in, only the plaintext the client alone holds can.
#[tokio::test(flavor = "multi_thread")]
async fn stored_digest_cannot_be_replayed_as_the_secret() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let minted = svc.create_guest().await.unwrap();
    let (subject, secret) = minted.device_secret.split_once('.').unwrap();
    let digest = secret_hash(secret);

    let (stored_digest,): (Option<String>,) = sqlx::query_as(
        "SELECT secret_hash FROM accounts.identities WHERE provider = 'guest' AND subject = $1",
    )
    .bind(subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_digest.as_deref(), Some(digest.as_str()));

    let identity = svc.me(Identity::player(minted.player_id.clone())).await.unwrap();
    assert!(
        identity.identities.iter().all(|i| i.provider != "guest" || i.subject == subject),
        "me() must never carry the secret, only provider/subject"
    );

    // Replaying the stored digest as though it were the plaintext secret must be
    // rejected — the stored form is a one-way digest, not a bearer-equivalent value.
    let replay_digest_as_secret = format!("{subject}.{digest}");
    let e = svc
        .login_federated(GUEST.to_string(), replay_digest_as_secret)
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized);

    cleanup_player(&pool, &minted.player_id).await;
}

/// A2: the returning-guest branch (`player_by_identity_tx` finds a row and RETURNS
/// before any insert) must neither clear nor rewrite the stored digest. A THIRD login
/// with the original ticket only succeeds if the second login's pass through the
/// early-return branch left the digest untouched.
#[tokio::test(flavor = "multi_thread")]
async fn returning_guest_login_preserves_the_stored_digest_across_replays() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_guest(&pool).await;

    let minted = svc.create_guest().await.unwrap();
    let (subject, secret) = minted.device_secret.split_once('.').unwrap();
    let expected_digest = secret_hash(secret);

    let second = svc
        .login_federated(GUEST.to_string(), minted.device_secret.clone())
        .await
        .expect("second login with the same ticket must succeed");
    assert_eq!(second.player_id, minted.player_id);

    let (digest_after_second,): (Option<String>,) = sqlx::query_as(
        "SELECT secret_hash FROM accounts.identities WHERE provider = 'guest' AND subject = $1",
    )
    .bind(subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        digest_after_second.as_deref(),
        Some(expected_digest.as_str()),
        "the returning-identity branch must not touch the stored digest"
    );

    let third = svc
        .login_federated(GUEST.to_string(), minted.device_secret.clone())
        .await
        .expect("a third login with the untouched ticket must still succeed");
    assert_eq!(third.player_id, minted.player_id);

    cleanup_player(&pool, &minted.player_id).await;
}

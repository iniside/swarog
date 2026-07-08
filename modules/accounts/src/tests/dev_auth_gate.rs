//! Trust-model gate on the accounts edge Auth face: with `ACCOUNTS_DEV_AUTH` off the
//! service-level guard withholds `register`/`login` — the SAME two methods the HTTP op
//! contributions withhold (`ops::register_player_ops`) — so a peer reaching the internal
//! mTLS edge Auth face with a dev-CA cert cannot self-register or log in when dev auth is
//! off. `verify_session` (the Sessions face, needed by the gateway's auth-once verifier)
//! is deliberately unaffected. A child of `tests` so it reuses that module's live-DB
//! harness (`wired`/`test_pool`) and its once-per-binary schema serialization.

// The `Auth`/`Sessions` trait methods resolve via the parent `tests` module's
// `use accountsapi::{Auth as _, Sessions as _}` (re-exported through this glob).
use super::*;

/// A service with the dev-auth gate forced on/off over a LAZY pool. The gate rejects
/// register/login BEFORE any DB access, so the reject-path tests need no live DB.
fn gated_service(dev_auth: bool) -> Arc<Service> {
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
        dev_auth,
        epic: OnceLock::new(),
    })
}

/// dev auth OFF → both dev/password methods are withheld at the service level, so the
/// edge Auth face rejects them with NotFound BEFORE touching the store (non-empty inputs
/// still reject — the guard is the first thing each method does).
#[tokio::test]
async fn dev_auth_off_withholds_register_and_login() {
    let svc = gated_service(false);

    let e = svc
        .register("a@x.io".into(), "pw".into(), "N".into())
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::NotFound,
        "register must be withheld over the edge when dev auth is off"
    );

    let e = svc.login("a@x.io".into(), "pw".into()).await.unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::NotFound,
        "login must be withheld over the edge when dev auth is off"
    );
}

/// dev auth ON → the gate is open, so register/login reach their normal handling: empty
/// credentials surface as Invalid (validation), NOT NotFound (the gate). Proves the guard
/// only fires when the gate is off.
#[tokio::test]
async fn dev_auth_on_lets_methods_reach_normal_handling() {
    let svc = gated_service(true);

    let e = svc
        .register(String::new(), String::new(), String::new())
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Invalid,
        "gate open: register must reach validation (Invalid), not the gate (NotFound)"
    );
}

/// `verify_session` is unaffected by the dev-auth gate: a dev-auth-OFF service still
/// resolves a bearer minted by a normal (gate-on) service over the same pool — the
/// gateway's auth-once verifier keeps working even where register/login are withheld.
/// Live-DB; SKIPs cleanly when Postgres is unreachable.
#[tokio::test]
async fn verify_session_unaffected_by_dev_auth_gate() {
    let Some(pool) = test_pool().await else { return };
    // `wired` builds a service with dev_auth = env default (on): mint a real session.
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("gate-{}@test.local", suffix());
    let sess = svc.register(email, "pw".into(), "G".into()).await.unwrap();

    // A SEPARATE dev-auth-OFF service over the same pool.
    let off = Arc::new(Service {
        store: Store { pool: pool.clone() },
        bus: Arc::new(Bus::new()),
        dev_auth: false,
        epic: OnceLock::new(),
    });

    // Sessions still resolves the token (gate does not touch verify_session)...
    assert_eq!(
        off.verify_session(sess.token.clone()).await.unwrap(),
        Some(sess.player_id.clone()),
        "verify_session must resolve regardless of the dev-auth gate"
    );
    // ...while its Auth face is genuinely gated off.
    let e = off
        .login("anyone@test.local".into(), "pw".into())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::NotFound);

    cleanup_player(&pool, &sess.player_id).await;
}

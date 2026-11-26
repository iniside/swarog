//! The open-registration gate on the accounts Auth face: with
//! `ACCOUNTS_OPEN_REGISTRATION` off the service-level guard withholds `register` on every
//! exposure path (the HTTP op is contributed unconditionally), while password login and
//! `verify_access` stay open.

use super::*;

use accountsapi::auth_rpc::{METHOD_LOGIN, METHOD_LOGIN_FEDERATED, METHOD_ME, METHOD_REGISTER};

/// A service with registration forced open/closed over a LAZY pool; the gate rejects
/// before any DB access.
fn gated_service(open_registration: bool) -> Arc<Service> {
    Arc::new(Service {
        tokens: test_tokens(&PgPool::connect_lazy(DEFAULT_DSN).unwrap()),
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
        open_registration,
        providers: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    })
}

#[tokio::test]
async fn closed_registration_withholds_register_but_not_login() {
    let svc = gated_service(false);

    let e = svc
        .register("a@x.io".into(), "pw".into(), "N".into(), test_client())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::NotFound, "register is withheld when closed");

    // Empty input never reaches the store, so the lazy pool is never dialled: login
    // answers its own 401, not the registration gate's 404.
    let e = svc.login(String::new(), String::new(), test_client()).await.unwrap_err();
    assert_eq!(e.status, opsapi::Status::Unauthorized, "login has no gate");
}

#[tokio::test]
async fn open_registration_lets_register_reach_validation() {
    let svc = gated_service(true);

    let e = svc
        .register(String::new(), String::new(), String::new(), test_client())
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Invalid,
        "gate open: register must reach validation (Invalid), not the gate (NotFound)"
    );
}

/// All four Auth ops are contributed UNCONDITIONALLY, even with registration closed and
/// no provider configured, while the impl guards reject (register → NotFound,
/// loginFederated → Unavailable): the monolith and split route sets stay equal.
#[tokio::test]
async fn ops_contributed_unconditionally_while_guard_rejects() {
    let svc = gated_service(false);

    let ctx = Context::new();
    crate::ops::register_player_ops(&ctx, svc.clone());

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    for m in [METHOD_REGISTER, METHOD_LOGIN, METHOD_LOGIN_FEDERATED, METHOD_ME] {
        assert!(
            ops.iter().any(|o| o.method == m),
            "op {m} must be contributed even with its gate off (impl-side gating)"
        );
    }
    assert_eq!(
        ctx.contributions::<opsapi::OpBinding>(opsapi::BINDING_SLOT).len(),
        ops.len(),
        "every contributed op must carry its HTTP↔wire binding"
    );
    assert_eq!(
        ctx.contributions::<opsapi::LocalOp>(opsapi::LOCAL_SLOT).len(),
        ops.len(),
        "every contributed op must carry its in-process invoker"
    );

    let e = svc
        .register("a@x.io".into(), "pw".into(), "N".into(), test_client())
        .await
        .unwrap_err();
    assert_eq!(e.status, opsapi::Status::NotFound);
    let e = svc
        .login_federated("epic".into(), "some-token".into(), test_client())
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        opsapi::Status::Unavailable,
        "unconfigured epic answers 503 (feature unavailable), the honest status"
    );
}

/// A registration-closed service over the same pool logs in an account an open one
/// registered and resolves its bearer.
#[tokio::test]
async fn login_and_verify_access_work_without_open_registration() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("gate-{}@test.local", suffix());
    let sess = svc.register(email.clone(), "pw".into(), "G".into(), test_client()).await.unwrap();

    let closed = Arc::new(Service {
        tokens: test_tokens(&pool),
        store: Store { pool: pool.clone() },
        bus: Arc::new(Bus::new()),
        open_registration: false,
        providers: OnceLock::new(),
        argon_permits: Arc::new(Semaphore::new(2)),
        login_slots: Arc::new(Semaphore::new(32)),
        verifier: Arc::new(ArgonVerifier),
    });

    let login = closed.login(email, "pw".into(), test_client()).await;
    let verified = closed.tokens.verify_access(&sess.token).await;
    cleanup_player(&pool, &sess.player_id).await;

    assert_eq!(login.unwrap().player_id, sess.player_id, "login needs no flag");
    assert_eq!(verified.unwrap().map(|c| c.sub), Some(sess.player_id.clone()));
}

//! `provide_factories()` (D2 capability-without-routes) vs `remote_factories()` (the
//! bundled provide+routes path). Proves the D2 seam provides the `Sessions` capability
//! under its registry key while contributing ZERO `#[http]` route bindings — the
//! collision-avoidance the D2 route table (rebuilt from each svc's `describe()`) needs —
//! and that the EXISTING bundled path is unchanged (still provides `Auth` AND contributes
//! the auth routes), so no non-D2 consumer regresses.

use std::sync::Arc;

use accountsapi::{Auth, Sessions};
use lifecycle::Context;
use opsapi::{Caller, Error, RetryMode};

/// A `Caller` that must never be dialed: the factory closures only WRAP it into a
/// generated `Client` and `provide` that under the registry key (register is lazy — no
/// dial happens here), so an actual call would be a test bug, not a real path.
struct DeadCaller;

#[async_trait::async_trait]
impl Caller for DeadCaller {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        panic!("provide_factories test must not dial the peer");
    }
}

/// Applies every factory closure in `factories` to a fresh DB-less [`Context`] (the
/// closures only touch the registry + contrib slots) and returns it for assertion.
fn apply(factories: Vec<remote::RemoteFactory>) -> Context {
    let ctx = Context::new();
    let caller: Arc<dyn Caller> = Arc::new(DeadCaller);
    for f in factories {
        f(&ctx, caller.clone());
    }
    ctx
}

#[test]
fn provide_factories_provides_sessions_without_contributing_routes() {
    let ctx = apply(crate::provide_factories());

    // (1) The capability the gateway's verifier `require`s IS provided, under the same
    // key + generated Client the bundled path uses (so it dials the peer identically).
    assert!(
        ctx.registry()
            .try_require::<dyn Sessions>(&registry::key("accounts", "sessions"))
            .is_some(),
        "accounts.sessions must resolve to an Arc<dyn Sessions> on the provide-only path"
    );

    // (2) NO #[http] routes contributed — the collision-avoidance for the D2 describe pass.
    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    assert!(
        ops.is_empty(),
        "provide_factories must contribute NO Operations (D2 gets routes from describe())"
    );
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    assert!(
        bindings.is_empty(),
        "provide_factories must contribute NO OpBindings"
    );

    // (3) The Auth ops are NOT provided as a `dyn Auth` client here: the D2 front routes
    // register/login/loginEpic/me over the edge from describe, never via a typed require.
    assert!(
        ctx.registry()
            .try_require::<dyn Auth>(&registry::key("accounts", "auth"))
            .is_none(),
        "provide_factories must provide only the Sessions sync dep, not dyn Auth"
    );
}

#[test]
fn remote_factories_still_bundles_provide_and_auth_routes() {
    let ctx = apply(crate::remote_factories());

    // Both capability clients still resolve (the bundled full path is unchanged).
    assert!(
        ctx.registry()
            .try_require::<dyn Sessions>(&registry::key("accounts", "sessions"))
            .is_some(),
        "bundled path still provides accounts.sessions"
    );
    assert!(
        ctx.registry()
            .try_require::<dyn Auth>(&registry::key("accounts", "auth"))
            .is_some(),
        "bundled path still provides accounts.auth"
    );

    // And it still contributes exactly the auth #[http] route Operations — the behavior
    // the current gateway-svc (a non-D2 consumer) relies on.
    let expected: Vec<String> = accountsapi::auth_rpc::route_bindings()
        .into_iter()
        .map(|rb| rb.operation.method)
        .collect();
    assert!(!expected.is_empty(), "auth has #[http] ops to contribute");

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    let op_methods: Vec<String> = ops.into_iter().map(|o| o.method).collect();
    assert_eq!(
        op_methods, expected,
        "bundled path contributes the auth route Operations"
    );
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    assert_eq!(
        bindings.len(),
        expected.len(),
        "BINDING_SLOT matches the auth route Operations"
    );
}

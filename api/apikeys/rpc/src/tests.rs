//! `provide_factories()` (the D2 capability-without-routes seam) for apikeys. Because
//! `Keys` is wire-only, apikeys already contributed no routes; this pins that BOTH the
//! new D2 seam and the existing `remote_factories()` provide the `Keys` capability under
//! its registry key and contribute ZERO route bindings — so apikeys needs no split beyond
//! the uniform `provide_factories()` name.

use std::sync::Arc;

use apikeysapi::Keys;
use lifecycle::Context;
use opsapi::{Caller, Error, RetryMode};

/// A `Caller` that must never be dialed (register is lazy — the factory only wraps it
/// into a generated `Client` and provides that under the registry key).
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
        panic!("apikeys provide_factories test must not dial the peer");
    }
}

/// Applies every factory closure to a fresh DB-less [`Context`] and returns it.
fn apply(factories: Vec<remote::RemoteFactory>) -> Context {
    let ctx = Context::new();
    let caller: Arc<dyn Caller> = Arc::new(DeadCaller);
    for f in factories {
        f(&ctx, caller.clone());
    }
    ctx
}

/// Both apikeys factory entry points must provide the `Keys` capability under its key and
/// contribute nothing to the route-table slots (wire-only capability, no `#[http]`).
fn assert_keys_provided_and_routeless(ctx: &Context, path: &str) {
    assert!(
        ctx.registry()
            .try_require::<dyn Keys>(&registry::key("apikeys", "keys"))
            .is_some(),
        "{path}: apikeys.keys must resolve to an Arc<dyn Keys>"
    );
    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    assert!(
        ops.is_empty(),
        "{path}: Keys is wire-only — no route Operations may be contributed"
    );
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    assert!(
        bindings.is_empty(),
        "{path}: no OpBindings for a wire-only capability"
    );
}

#[test]
fn provide_factories_provides_keys_without_routes() {
    let ctx = apply(crate::provide_factories());
    assert_keys_provided_and_routeless(&ctx, "provide_factories");
}

#[test]
fn remote_factories_is_already_routeless() {
    let ctx = apply(crate::remote_factories());
    assert_keys_provided_and_routeless(&ctx, "remote_factories");
}

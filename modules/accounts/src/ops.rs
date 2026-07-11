//! Contributes the accounts player operations to the gateway slots (port of Go's
//! `modules/accounts/ops.go`). Route, HTTP↔wire binding and in-process invoker are
//! all GENERATED from `accountsapi::Auth` + its `#[http]` bindings, so the gateway's
//! LocalBackend and RemoteBackend consume the SAME wire envelopes.
//!
//! ALL four ops are contributed UNCONDITIONALLY — the monolith slot set and the
//! split route set stay structurally equal by construction. The dev/epic gating
//! lives at the IMPL, the single authority every exposure path traverses:
//! `register`/`login` answer NotFound (→ 404) when `ACCOUNTS_DEV_AUTH` is off, and
//! `loginEpic` answers Unavailable (→ 503) when the epic provider is not configured.

use std::sync::Arc;

use lifecycle::Context;

use crate::Service;

pub(crate) fn register_player_ops(ctx: &Context, svc: Arc<Service>) {
    // Hand the service to the generated glue AS the pure capability trait.
    let auth: Arc<dyn accountsapi::Auth> = svc;
    for op in accountsapi::auth_rpc::operations(auth) {
        ctx.contribute(opsapi::SLOT, op.operation);
        ctx.contribute(opsapi::BINDING_SLOT, op.binding);
        ctx.contribute(opsapi::LOCAL_SLOT, op.local);
    }
}

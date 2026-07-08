//! Contributes the accounts player operations to the gateway slots (port of Go's
//! `modules/accounts/ops.go`). Route, HTTP↔wire binding and in-process invoker are
//! all GENERATED from `accountsapi::Auth` + its `#[http]` bindings, so the gateway's
//! LocalBackend and RemoteBackend consume the SAME wire envelopes.
//!
//! register/login are contributed only under `ACCOUNTS_DEV_AUTH`, loginEpic only
//! when the epic provider is configured, me always — mirroring Go's conditional
//! route registration exactly.

use std::sync::Arc;

use accountsapi::auth_rpc::{METHOD_LOGIN, METHOD_LOGIN_EPIC, METHOD_ME, METHOD_REGISTER};
use lifecycle::Context;

use crate::Service;

pub(crate) fn register_player_ops(ctx: &Context, svc: Arc<Service>, dev_auth: bool, epic_enabled: bool) {
    // Hand the service to the generated glue AS the pure capability trait.
    let auth: Arc<dyn accountsapi::Auth> = svc;
    for op in accountsapi::auth_rpc::operations(auth) {
        let enabled = match op.operation.method.as_str() {
            m if m == METHOD_REGISTER || m == METHOD_LOGIN => dev_auth,
            m if m == METHOD_LOGIN_EPIC => epic_enabled,
            m if m == METHOD_ME => true,
            _ => true,
        };
        if !enabled {
            continue;
        }
        ctx.contribute(opsapi::SLOT, op.operation);
        ctx.contribute(opsapi::BINDING_SLOT, op.binding);
        ctx.contribute(opsapi::LOCAL_SLOT, op.local);
    }
}

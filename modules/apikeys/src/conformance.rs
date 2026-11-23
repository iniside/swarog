//! Minimal factual probes consumed by `tools/conformance`.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlplane::{InvokeCall, Provider};
use sqlx::PgPool;

use crate::admin::{ApiKeysAdmin, FIELD_EXPIRES, FIELD_NAME, FIELD_ROLE, KEY_CREATE, NEVER};
use crate::store::Store;
use crate::Service;

/// Re-exported so `tools/conformance` can state the caps by reference instead of a
/// second literal — the store's `COLUMN_CAPS` remains the sole definition site.
pub use crate::store::{MAX_NAME_BYTES, MAX_POLICY_BYTES};

/// A presented key longer than the shared byte cap is definitively rejected — the
/// invariant [`apikeysapi::MAX_KEY_BYTES`] still guarantees. Secrets are now
/// SERVER-GENERATED (there is no caller-supplied creation path to cap), so this probe
/// reflects the surviving authority: the gateway's presented-key length guard
/// (`modules/gateway/src/keys.rs`) and the store's generated-secret guard
/// (`store::generate_secret`) both hold the digest input to `<= MAX_KEY_BYTES`. Stays
/// TRUE for any over-cap length. (Step 8 repoints the conformance policy basis string to
/// the gateway lookup side accordingly.)
#[doc(hidden)]
pub fn conformance_key_rejected(len: usize) -> bool {
    len > apikeysapi::MAX_KEY_BYTES
}

/// A role's `policy` string longer than the shared byte cap is definitively rejected.
/// `roles.policy` is operator-writable (`create_role`/`set_role_policy`) and rides every
/// gateway key-lookup response plus its 5s cache, so
/// `store::validate_policy` caps it at admission — and this probe CALLS that function
/// rather than restating its comparison, so tightening the rule to `>=` cannot leave the
/// probe green (the `characters::conformance` pattern).
#[doc(hidden)]
pub fn conformance_policy_rejected(len: usize) -> bool {
    crate::store::validate_policy(&"a".repeat(len)).is_err()
}

/// A role or key NAME longer than the shared byte cap is definitively rejected. Names are
/// operator input, and `store::validate_name` caps every writer that binds one — as an
/// inserted value, an updated value, or a `WHERE` predicate. This probe calls that very
/// function, on the same `ColumnCap` the role writers pass.
#[doc(hidden)]
pub fn conformance_name_rejected(len: usize) -> bool {
    crate::store::validate_name(&crate::store::ROLE_NAME, &"a".repeat(len)).is_err()
}

/// The byte cap the provider applies to every `cp.invoke` input value.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

/// The provider's own first check on every `cp.invoke`, executed through
/// `ApiKeysAdmin::invoke` against a store that cannot connect. The create carries a valid
/// name, role and expiry, so an at-cap extra value reaches the store and answers `Internal`;
/// one byte over, the cap's `Invalid` is the only refusal it can meet.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let admin = {
        let _guard = rt.enter();
        ApiKeysAdmin::new(Arc::new(Service { store: Store { pool: dead_pool() } }))
    };
    let input = BTreeMap::from([
        (FIELD_NAME.to_string(), "conformance-key".to_string()),
        (FIELD_ROLE.to_string(), "conformance-role".to_string()),
        (FIELD_EXPIRES.to_string(), NEVER.to_string()),
        ("value".to_string(), "a".repeat(len)),
    ]);
    let call = InvokeCall {
        module: "apikeys".to_string(),
        action: KEY_CREATE.to_string(),
        entity: None,
        input,
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: None,
    };
    matches!(rt.block_on(admin.invoke(call)), Err(e) if e.status == opsapi::Status::Invalid)
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe that
/// reaches the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .expect("lazy pool from a well-formed DSN")
}

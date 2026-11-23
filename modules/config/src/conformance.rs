//! Minimal factual probes consumed by `tools/conformance`.
//!
//! Policy and expected outcomes live in the tool. The probe drives `ConfigAdmin::invoke`
//! itself, the code a portal write traverses.

use std::collections::BTreeMap;

use controlplane::{InvokeCall, Provider as _};

use crate::admin::{ConfigAdmin, FIELD_KEY, FIELD_VALUE, SAVE};

/// The byte cap `ConfigAdmin::invoke` applies to every input value before anything else.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

/// The provider's own first check on every `cp.invoke`, executed through `ConfigAdmin::invoke`
/// against a store that cannot connect. The save carries a well-formed key and revision, so an
/// at-cap value reaches the store's read and answers `Internal`; one byte over, the cap's
/// `Invalid` is the only refusal it can meet.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let admin = {
        let _guard = rt.enter();
        ConfigAdmin::new(dead_pool())
    };
    let call = InvokeCall {
        module: "config".to_string(),
        action: SAVE.to_string(),
        entity: None,
        input: BTreeMap::from([
            (FIELD_KEY.to_string(), "conformance.probe".to_string()),
            (FIELD_VALUE.to_string(), "a".repeat(len)),
        ]),
        expected_revision: Some(1),
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: None,
    };
    matches!(rt.block_on(admin.invoke(call)), Err(e) if e.status == opsapi::Status::Invalid)
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe that
/// reaches the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .expect("lazy pool from a well-formed DSN")
}

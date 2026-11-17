//! Minimal factual probes consumed by `tools/conformance`.

use std::collections::BTreeMap;

use controlplane::{EntityRef, InvokeCall, InvokeOutcome, Provider};

use crate::events_provider::{EventsProvider, KIND, MODULE, SUB_PAUSE};

#[doc(hidden)]
pub fn conformance_username_rejected(len: usize) -> bool {
    !crate::username_within_cap(&"a".repeat(len))
}

#[doc(hidden)]
pub fn conformance_password_rejected(len: usize) -> bool {
    !crate::password_within_cap(&"a".repeat(len))
}

/// The byte cap `core.events` applies to every `cp.invoke` input value.
pub const CORE_EVENTS_INPUT_VALUE_BYTES: usize = crate::events_provider::MAX_INPUT_VALUE_BYTES;

/// The provider's own first check on every `cp.invoke`, executed through
/// `EventsProvider::invoke` against a pool that cannot connect. `sub.pause` on a
/// subscription entity reads no input, so at the cap the call reaches the plane's state
/// read and answers `Internal` off the dead pool; one byte over, the cap's `Invalid` is
/// the only refusal it can meet. Deleting the guard sends the long value to the pool too,
/// and the case goes red.
#[doc(hidden)]
pub fn conformance_core_events_input_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let provider = {
        let _guard = rt.enter();
        EventsProvider::new(dead_pool())
    };
    let call = InvokeCall {
        module: MODULE.to_string(),
        action: SUB_PAUSE.to_string(),
        entity: Some(EntityRef { kind: KIND.to_string(), id: "conformance-probe".to_string() }),
        input: BTreeMap::from([("value".to_string(), "a".repeat(len))]),
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: None,
    };
    let outcome: Result<InvokeOutcome, opsapi::Error> = rt.block_on(provider.invoke(call));
    matches!(outcome, Err(e) if e.status == opsapi::Status::Invalid)
}

/// A pool pointed at a port nothing listens on, with the acquire wait bounded so a probe
/// that reaches the store fails fast instead of sitting out sqlx's 30-second default.
fn dead_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy(DEAD_DSN)
        .expect("lazy pool from a well-formed DSN")
}

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

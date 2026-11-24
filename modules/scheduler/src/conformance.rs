//! Factual probes consumed by `tools/conformance`. Each runs the portal writer itself,
//! `SchedulerAdmin::invoke`, on a provider whose process has the scheduler disabled: a call
//! the input guard admits answers `Forbidden` before any I/O, so `Invalid` can only come
//! from the guard under test.

use std::collections::BTreeMap;
use std::sync::Arc;

use bus::Bus;
use controlplane::{EntityRef, InvokeCall, Provider as _};
use sqlx::PgPool;

use crate::admin::{SchedulerAdmin, SCHEDULE_FIRE, SCHEDULE_KIND};

/// The cap `SchedulerAdmin::invoke` applies to every input value before anything else.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

/// [`CP_INVOKE_INPUT_VALUE_BYTES`], executed through `SchedulerAdmin::invoke` on a Fire now
/// carrying one extra input value of `len` bytes: at the cap it passes the guard and meets
/// the disabled refusal, over it the guard answers `Invalid` first.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let admin = {
        let _guard = rt.enter();
        let pool = PgPool::connect_lazy(DEAD_DSN).expect("lazy pool");
        SchedulerAdmin::new(pool, Arc::new(Bus::new()), false)
    };
    let call = InvokeCall {
        module: "scheduler".to_string(),
        action: SCHEDULE_FIRE.to_string(),
        entity: Some(EntityRef { kind: SCHEDULE_KIND.to_string(), id: "audit-prune".to_string() }),
        input: BTreeMap::from([("value".to_string(), "a".repeat(len))]),
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: None,
    };
    matches!(rt.block_on(admin.invoke(call)), Err(e) if e.status == opsapi::Status::Invalid)
}

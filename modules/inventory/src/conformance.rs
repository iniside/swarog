//! Factual probes consumed by `tools/conformance`. Each runs the portal writer itself,
//! `InventoryAdmin::invoke`, against a store that cannot connect: a call the guards admit
//! reaches the pool and answers `Internal`, so `Invalid` can only come from the guard
//! under test.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use controlplane::{EntityRef, InvokeCall, Provider as _};
use sqlx::PgPool;

use crate::admin::{InventoryAdmin, FIELD_CHARACTER, FIELD_ITEM, FIELD_QUANTITY, ITEM_GRANT};
use crate::{Inner, Store};

/// The cap `InventoryAdmin::invoke` applies to every input value before anything else.
pub const CP_INVOKE_INPUT_VALUE_BYTES: usize = controlplane::MAX_INPUT_VALUE_BYTES;

const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";
const PROBE_PLAYER: &str = "00000000-0000-4000-8000-000000000001";
const PROBE_CHARACTER: &str = "00000000-0000-4000-8000-000000000002";

/// [`CP_INVOKE_INPUT_VALUE_BYTES`], executed through `InventoryAdmin::invoke` on a grant whose
/// declared fields all pass: at the cap the extra value passes the input guard and the call
/// reaches the dead store, over it the guard answers first.
#[doc(hidden)]
pub fn conformance_cp_invoke_input_rejected(len: usize) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let admin = {
        let _guard = rt.enter();
        let pool = PgPool::connect_lazy(DEAD_DSN).expect("lazy pool");
        InventoryAdmin::new(Arc::new(Inner {
            store: Store { pool },
            dev_grant: false,
            ownership: OnceLock::new(),
            cfg: OnceLock::new(),
        }))
    };
    let input = BTreeMap::from([
        (FIELD_CHARACTER.to_string(), PROBE_CHARACTER.to_string()),
        (FIELD_ITEM.to_string(), "coin".to_string()),
        (FIELD_QUANTITY.to_string(), "1".to_string()),
        ("value".to_string(), "a".repeat(len)),
    ]);
    let call = InvokeCall {
        module: "inventory".to_string(),
        action: ITEM_GRANT.to_string(),
        entity: Some(EntityRef { kind: "player".to_string(), id: PROBE_PLAYER.to_string() }),
        input,
        expected_revision: None,
        reason: None,
        operator: "conformance".to_string(),
        item: None,
        request_id: Some("conformance".to_string()),
    };
    matches!(rt.block_on(admin.invoke(call)), Err(e) if e.status == opsapi::Status::Invalid)
}

//! `configevents` — the published contract of the config domain (port of Go's
//! `api/config/configevents`): the `config.changed` event appended after a setting
//! write. It is the ONLY surface other modules share with config (payload + descriptor).
//!
//! As of Step 7 the SOLE producer is config's `config.settings` write trigger, which
//! (in the writing transaction) bumps the monotonic `config.revision`, fires the
//! `config_changed` NOTIFY, and calls the plane-owned `asyncevents.append_event`. There
//! is no Rust `emit_tx` producer any more (a psql/admin write and a service write emit
//! identically — the trigger is the single path). Durable consumers subscribe with a
//! stable id (`audit.config-changed.v1`); replica-local cache freshness moved off the
//! durable plane onto the broadcast invalidation plane (the `config_changed` channel).

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Carries the namespaced setting that just changed, its new value, the mutation
/// kind, and the monotonic revision the write produced. Evolve additively
/// (constraint #6): add fields / a `ChangedV2`, never reshape.
///
/// The Step-7 payload is a deliberate one-time reshape of the fresh-world reset (the
/// acknowledged `public-api` red): `value` is now `Option<String>` — `null` on a
/// DELETE, where there is no new value — and `operation`/`revision` are new. The field
/// names are the wire/JSON contract the trigger's `jsonb_build_object` emits, so they
/// must stay snake_case and match the trigger exactly.
///
/// `Serialize`/`Deserialize` are load-bearing: the durable transport collapses the
/// payload to JSON at the append/deliver boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub namespace: String,
    pub key: String,
    /// The new value, or `None` when `operation == "delete"` (the row is gone).
    pub value: Option<String>,
    /// The mutation kind: `"insert"`, `"update"`, or `"delete"` — the trigger's `TG_OP`
    /// lowercased.
    pub operation: String,
    /// The monotonic `config.revision` value this write produced. Strictly increases
    /// across every setting mutation; a cache applies a refresh only when the revision
    /// it reads is newer than the one it holds.
    pub revision: i64,
}

/// The `config.changed` topic. The `config.settings` trigger calls
/// `asyncevents.append_event` for it in the writing transaction, so it commits
/// atomically with the setting change and every durable subscriber (audit) observes
/// exactly the writes that landed.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*configevents::CHANGED`.
pub static CHANGED: LazyLock<EventType<Changed>> =
    LazyLock::new(|| define("config.changed", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Wire samples for the contract-golden fingerprint (Step 5/5b). TWO samples:
///
/// 1. Fully POPULATED — every field set, `value = Some(...)`, so the `Option<String>`
///    field's Some-shape (`payload.value:string`) appears in the golden and a silent
///    `#[serde(rename)]` fails the blocking contract-golden stage.
/// 2. The `None` case (`value: None`, the DELETE shape) — CONVENTION (Step 5b): every
///    `Option` field in an event payload appears in at least one sample as `None`.
///    This pins `payload.value:null` in the golden AND compile-couples the optionality
///    itself: if `value` ever became a required `String`, the `value: None` literal
///    below would stop compiling — a loud break instead of a green gate over retained
///    `null` JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![
        (
            "config.changed",
            1,
            serde_json::to_value(Changed {
                namespace: "gameplay".to_string(),
                key: "max_level".to_string(),
                value: Some("100".to_string()),
                operation: "update".to_string(),
                revision: 42,
            })
            .expect("Changed serializes to json"),
        ),
        (
            "config.changed",
            1,
            serde_json::to_value(Changed {
                namespace: "gameplay".to_string(),
                key: "max_level".to_string(),
                value: None,
                operation: "delete".to_string(),
                revision: 43,
            })
            .expect("Changed serializes to json"),
        ),
    ]
}

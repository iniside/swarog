//! `schedulerevents` — the published event contract of the "scheduler" domain (port
//! of Go's `api/scheduler/schedulerevents`). Anyone reacting to a schedule elapsing
//! (e.g. `audit`: prune the ledger on `scheduler.fired{name:"audit-prune"}`) imports
//! this; nobody imports the scheduler implementation.
//!
//! Like `charactersevents`/`accountsevents` these ride the **durable** plane
//! (`bus::emit_tx` / `bus::on_tx`), atomic with the `last_fired` bump — so the payload
//! is `Serialize`/`Deserialize` (the transport collapses `T` to JSON at the
//! emit_tx/on_tx boundary). The serde field name (`name`) is the wire contract: the
//! producer (scheduler) and every durable consumer (audit) must agree on it.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires once per elapsed interval for one named schedule. It carries only the
/// schedule NAME — a closure can't cross a process boundary, so the scheduler runs no
/// job code; the reacting consumer keys off `name` (Go's `Fired`). Evolve additively
/// (constraint #6): add fields / a `FiredV2`, never reshape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fired {
    pub name: String,
}

/// The `scheduler.fired` topic. Emitted via `bus::emit_tx` inside the tx that also
/// bumps the schedule's `last_fired`, so the event is durable iff the bump is.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&schedulerevents::FIRED` (auto-deref) and read the topic string with
/// `schedulerevents::FIRED.topic()`.
pub static FIRED: LazyLock<EventType<Fired>> =
    LazyLock::new(|| define("scheduler.fired", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Fully-POPULATED wire sample for the contract-golden fingerprint (Step 5): every
/// field set so serde's actual JSON key (`name`) lands in the golden. A silent
/// `#[serde(rename)]` or a reshaped field then fails the blocking contract-golden stage
/// instead of poisoning retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![(
        "scheduler.fired",
        1,
        serde_json::to_value(Fired {
            name: "audit-prune".to_string(),
        })
        .expect("Fired serializes to json"),
    )]
}

/// Names of schedules the scheduler module SEEDS — not a namespace for names
/// consumers invent. The producer's seed DDL already ships this string
/// (coupling-through-data); the const names that existing fact where both sides can
/// reference one symbol.
pub mod schedule_names {
    pub const AUDIT_PRUNE: &str = "audit-prune";
    /// The daily cadence on which `accounts` prunes expired sessions
    /// (`accounts.sessions` where `expires_at <= now()`). The scheduler seeds this
    /// schedule (86400s); accounts reacts to `scheduler.fired{name}` matching it.
    pub const SESSIONS_PRUNE: &str = "accounts-sessions-prune";
}

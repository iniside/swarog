//! `adminevents` — the published event contract of the "admin" domain (mirrors
//! `api/match/events`). Anyone reacting to an admin-portal action (audit: record the
//! ledger row) imports this; nobody imports the admin implementation.
//!
//! Rides the **durable** plane (`bus::emit_tx` / `bus::on_tx`), atomic with the
//! domain write it accompanies — so the payload is `Serialize`/`Deserialize` (the
//! transport collapses `T` to JSON at the emit_tx/on_tx boundary). The serde field
//! names (`actor`, `action`, `target`, `detail`) are the wire contract every durable
//! consumer agrees on.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires on a GameOps admin-portal action. `action` is a documented convention, not an
/// enum, so new values are additive (constraint #6): one of
/// `login-succeeded | login-locked | logout | form-submit`. Evolve additively — add
/// fields / a new action value / an `AdminActionV2`, never reshape existing fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAction {
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

/// The `admin.action` topic. Emitted via `bus::emit_tx` from the admin auth surface
/// (login/lockout/logout — LOCAL in both topologies) and, where a form is co-hosted
/// locally, from a successful form submit.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*adminevents::ACTION` (or just `&adminevents::ACTION`, which
/// auto-derefs). Its `.topic()` is `"admin.action"` — the string audit subscribes to.
pub static ACTION: LazyLock<EventType<AdminAction>> =
    LazyLock::new(|| define("admin.action", 1, HistoryPolicy::MinRetention { days: 30 }));

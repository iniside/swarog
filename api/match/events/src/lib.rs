//! `matchevents` — the published event contract of the "match" domain (port of Go's
//! `api/match/matchevents`). Anyone reacting to a finished match (rating: adjust MMR;
//! leaderboard: bump the winner's wins; audit: record the ledger row) imports this;
//! nobody imports the match implementation.
//!
//! Unlike Go's `bus.Define` (a plain in-process bus), this rides the **durable** plane
//! (`bus::emit_tx` / `bus::on_tx`), atomic with the domain write (the `match.matches`
//! insert) — so the payload is `Serialize`/`Deserialize` (the transport collapses `T`
//! to JSON at the emit_tx/on_tx boundary). The serde field names (`match_id`, `winner`,
//! `loser`) are the wire contract every durable consumer agrees on.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// Fires when a match result is reported. Evolve additively (constraint #6): add
/// fields / a `FinishedV2`, never reshape — a structural change breaks every consumer
/// at compile time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finished {
    pub match_id: String,
    pub winner: String,
    pub loser: String,
}

/// The `match.finished` topic. Emitted via `bus::emit_tx` inside the domain tx that
/// inserts the `match.matches` row, so the event is durable iff the match is.
///
/// `bus::define` is not `const`, so the descriptor is a `LazyLock` static; callers
/// pass it as `&*matchevents::FINISHED` (or just `&matchevents::FINISHED`, which
/// auto-derefs). Its `.topic()` is `"match.finished"` — the string audit subscribes to.
pub static FINISHED: LazyLock<EventType<Finished>> =
    LazyLock::new(|| define("match.finished", 1, HistoryPolicy::MinRetention { days: 7 }));

/// Fully-POPULATED wire sample for the contract-golden fingerprint (Step 5): every
/// field set so serde's actual JSON keys (`winner`/`loser`, distinct from the HTTP
/// report body's `Winner`/`Loser`) land in the golden. A silent `#[serde(rename)]` or a
/// reshaped field then fails the blocking contract-golden stage instead of poisoning
/// retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![(
        "match.finished",
        1,
        serde_json::to_value(Finished {
            match_id: "match-1".to_string(),
            winner: "alice".to_string(),
            loser: "bob".to_string(),
        })
        .expect("Finished serializes to json"),
    )]
}

//! `friendsevents` — the published event vocabulary of the "friends" domain. Anyone who
//! reacts to a friendship changing imports this; nobody imports the friends
//! implementation.
//!
//! The topics ride the **durable** plane, emitted INSIDE the transaction that writes the
//! `friends.edges` row — so the event is durable iff the relation change is.
//!
//! **Every payload carries both players' HANDLES, not just their ids.** A durable handler
//! runs inside the delivery transaction with no registry access to `accounts.directory`,
//! and `notifications-svc` holds no accounts stub, so a consumer that received ids alone
//! could only render `"3f2a…-… sent you a friend request"`. Constraint #6 makes a payload
//! field un-addable later — a handle-less v1 would have to become a v2 contract with new
//! subscription ids — so the denormalization is the contract, not an optimization. The
//! handle is a SNAPSHOT taken at emit time; the authority for a player's current handle is
//! `accounts`.
//!
//! **`edge_id` is in every payload and is the consumer's idempotency key**, mirroring
//! `walletevents::Changed.ledger_id`: the row is the authority, the event is only the
//! delivery path. This matters because ordering is per-SUBSCRIPTION — a consumer holding
//! separate subscriptions on [`REQUESTED`] and [`REMOVED`] can observe the removal first.
//! Carry the id; never infer order across topics.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// One player asked another to be friends; the relation exists and is unanswered.
///
/// `requester_id` authored it and `addressee_id` is the only party who can accept or
/// decline. `edge_id` names the `friends.edges` row.
///
/// Evolve additively (constraint #6): a new field needs `#[serde(default)]` or must be
/// `Option` (owing a second `None`-populated golden sample) — a retained pre-change event
/// has no key for it, so a bare required field fails `decode` and pauses every
/// subscription on this topic. Anything that can't satisfy that is a `RequestedV2`, not a
/// field add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requested {
    pub edge_id: String,
    pub requester_id: String,
    pub requester_handle: String,
    pub addressee_id: String,
    pub addressee_handle: String,
}

/// The relation became mutual. Emitted once per accepted edge — including on the
/// auto-accept branch, where a request crossed the other player's own pending one and no
/// [`REQUESTED`] event precedes it for that direction.
///
/// The roles are the ORIGINAL ones, preserved from [`Requested`]: `requester_id` authored
/// the request and `addressee_id` accepted it. A consumer announcing the acceptance
/// therefore addresses the REQUESTER and renders `addressee_handle`.
///
/// Evolve additively — see [`Requested`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub edge_id: String,
    pub requester_id: String,
    pub requester_handle: String,
    pub addressee_id: String,
    pub addressee_handle: String,
}

/// The relation is gone, whatever state it was in. `actor_id` is the party who ended it
/// and `other_id` is the one who did not; neither maps to the original requester/addressee
/// roles, because either side may remove.
///
/// **One topic covers every ending.** `reason` is the discriminator (`"declined"`,
/// `"unfriended"`, and a future `"blocked"`) rather than three topics, because a defined
/// topic with no durable subscriber fails the blocking durability check — three endings
/// would each owe their own consumer. Treat an unrecognised `reason` as a plain removal;
/// a new value is an additive change, a new topic would not be.
///
/// Evolve additively — see [`Requested`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub edge_id: String,
    pub actor_id: String,
    pub actor_handle: String,
    pub other_id: String,
    pub other_handle: String,
    pub reason: String,
}

/// `MinRetention { days: 30 }` matches `AUDIT_RETENTION_DAYS`'s default: the durable log
/// is the DELIVERY path, not the archive — the authority for the social graph is
/// `friends.edges`. The history policy is IMMUTABLE after the first emit.
pub static REQUESTED: LazyLock<EventType<Requested>> =
    LazyLock::new(|| define("friend.requested", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Retention as [`REQUESTED`].
pub static ACCEPTED: LazyLock<EventType<Accepted>> =
    LazyLock::new(|| define("friend.accepted", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Retention as [`REQUESTED`].
pub static REMOVED: LazyLock<EventType<Removed>> =
    LazyLock::new(|| define("friend.removed", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Fully-POPULATED wire sample per defined `(topic, version)`: every field set, so serde's
/// actual JSON keys land in the golden and a silent `#[serde(rename)]` or a reshaped field
/// fails the blocking stage instead of poisoning retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![
        (
            "friend.requested",
            1,
            serde_json::to_value(Requested {
                edge_id: "edge-1".to_string(),
                requester_id: "player-1".to_string(),
                requester_handle: "Alice#1234".to_string(),
                addressee_id: "player-2".to_string(),
                addressee_handle: "Bob#5678".to_string(),
            })
            .expect("Requested serializes to json"),
        ),
        (
            "friend.accepted",
            1,
            serde_json::to_value(Accepted {
                edge_id: "edge-1".to_string(),
                requester_id: "player-1".to_string(),
                requester_handle: "Alice#1234".to_string(),
                addressee_id: "player-2".to_string(),
                addressee_handle: "Bob#5678".to_string(),
            })
            .expect("Accepted serializes to json"),
        ),
        (
            "friend.removed",
            1,
            serde_json::to_value(Removed {
                edge_id: "edge-1".to_string(),
                actor_id: "player-2".to_string(),
                actor_handle: "Bob#5678".to_string(),
                other_id: "player-1".to_string(),
                other_handle: "Alice#1234".to_string(),
                reason: "unfriended".to_string(),
            })
            .expect("Removed serializes to json"),
        ),
    ]
}

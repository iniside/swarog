//! `groupsevents` — the published event vocabulary of the "groups" domain. Anyone who
//! reacts to a group's membership changing imports this; nobody imports the groups
//! implementation.
//!
//! The topics ride the **durable** plane, emitted INSIDE the transaction that wrote
//! the `groups.groups`/`groups.memberships` row — so the event is durable iff the
//! membership change is.
//!
//! `create` emits both [`CREATED`] and a [`MEMBER_JOINED`] for the creator's admin row
//! in the SAME transaction: without the second, the ledger shows a group whose only
//! admin never joined, and a later `member_left` for the creator has no matching join.

use std::sync::LazyLock;

use bus::{define, EventType, HistoryPolicy};
use serde::{Deserialize, Serialize};

/// [`MemberLeft::reason`] for an ordinary voluntary departure.
pub const REASON_LEFT: &str = "left";
/// [`MemberLeft::reason`] for an admin's `decide` reject on a [`STATE_MEMBER`] row.
pub const REASON_KICKED: &str = "kicked";
/// [`MemberLeft::reason`] for a `respond`/`decide` reject on a pending row.
pub const REASON_DECLINED: &str = "declined";
/// [`MemberLeft::reason`] for a pending row the retention sweep removed after
/// `GROUPS_RETENTION_DAYS`. No party ended it, so [`MemberLeft::actor_id`] is empty.
pub const REASON_EXPIRED: &str = "expired";

/// [`RoleChanged::actor_kind`] for a change made through the operator portal. The admin
/// seam carries NO caller identity (`adminapi::AdminData`/`AdminSubmit` are
/// process-authenticated), so [`RoleChanged::actor_id`] is empty for it: the acting human
/// is named by the portal's own `admin.action` row, correlated by time.
pub const ACTOR_OPERATOR: &str = "operator";
/// [`RoleChanged::actor_kind`] for a change made by a player of the group, whose
/// [`RoleChanged::actor_id`] is that player's id.
pub const ACTOR_PLAYER: &str = "player";

/// A group was created. `join_policy` is one of `groupsapi::JOIN_OPEN`/`JOIN_REQUEST`/
/// `JOIN_INVITE`.
///
/// Evolve additively (constraint #6): a new field needs `#[serde(default)]` or must be
/// `Option` — a retained pre-change event has no key for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    pub group_id: String,
    pub name: String,
    pub creator_id: String,
    pub join_policy: String,
}

/// A player's row became `groupsapi::STATE_MEMBER`. `role` is
/// `groupsapi::ROLE_ADMIN`/`ROLE_MEMBER`.
///
/// Evolve additively — see [`Created`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberJoined {
    pub group_id: String,
    pub player_id: String,
    pub role: String,
}

/// A player's row was removed from the group, whatever state it was in.
///
/// `actor_id` is the party who ended it (the subject itself for [`REASON_LEFT`], an
/// admin for [`REASON_KICKED`], the subject for [`REASON_DECLINED`]) — recorded
/// because a kick is otherwise unattributable in a ledger that retains 30 days. It is
/// EMPTY when no party ended it ([`REASON_EXPIRED`]), never a stand-in id: a consumer
/// reading it as "who did this" must not be told the subject did.
/// `reason` is an open vocabulary: treat an unrecognised value as the plain case, so a
/// future ending is an additive change rather than a new topic.
///
/// Evolve additively — see [`Created`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLeft {
    pub group_id: String,
    pub player_id: String,
    pub actor_id: String,
    pub reason: String,
}

/// A member's `role` within a group changed while the row stayed a
/// `groupsapi::STATE_MEMBER` row. `role` is the NEW role
/// (`groupsapi::ROLE_ADMIN`/`ROLE_MEMBER`).
///
/// `actor_kind` names WHO changed it ([`ACTOR_OPERATOR`]/[`ACTOR_PLAYER`], an open
/// vocabulary — treat an unrecognised value as "some authority did this"), and
/// `actor_id` identifies them when the acting seam knows an id. It is EMPTY otherwise,
/// never a stand-in id, on the rule [`MemberLeft::actor_id`] already sets.
///
/// Evolve additively — see [`Created`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleChanged {
    pub group_id: String,
    pub player_id: String,
    pub role: String,
    pub actor_kind: String,
    pub actor_id: String,
}

/// `MinRetention { days: 30 }` matches `friendsevents`: the durable log is the
/// DELIVERY path, not the archive — the authority for group membership is
/// `groups.memberships`. The history policy is IMMUTABLE after the first emit.
pub static CREATED: LazyLock<EventType<Created>> =
    LazyLock::new(|| define("group.created", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Retention as [`CREATED`].
pub static MEMBER_JOINED: LazyLock<EventType<MemberJoined>> =
    LazyLock::new(|| define("group.member_joined", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Retention as [`CREATED`].
pub static MEMBER_LEFT: LazyLock<EventType<MemberLeft>> =
    LazyLock::new(|| define("group.member_left", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Retention as [`CREATED`].
pub static ROLE_CHANGED: LazyLock<EventType<RoleChanged>> =
    LazyLock::new(|| define("group.role_changed", 1, HistoryPolicy::MinRetention { days: 30 }));

/// Fully-POPULATED wire sample per defined `(topic, version)`: every field set, so
/// serde's actual JSON keys land in the golden and a silent `#[serde(rename)]` or a
/// reshaped field fails the blocking stage instead of poisoning retained durable JSON.
#[doc(hidden)]
pub fn golden_samples() -> Vec<(&'static str, u32, serde_json::Value)> {
    vec![
        (
            "group.created",
            1,
            serde_json::to_value(Created {
                group_id: "group-1".to_string(),
                name: "Guild of Foo".to_string(),
                creator_id: "player-1".to_string(),
                join_policy: "open".to_string(),
            })
            .expect("Created serializes to json"),
        ),
        (
            "group.member_joined",
            1,
            serde_json::to_value(MemberJoined {
                group_id: "group-1".to_string(),
                player_id: "player-1".to_string(),
                role: "admin".to_string(),
            })
            .expect("MemberJoined serializes to json"),
        ),
        (
            "group.member_left",
            1,
            serde_json::to_value(MemberLeft {
                group_id: "group-1".to_string(),
                player_id: "player-2".to_string(),
                actor_id: "player-1".to_string(),
                reason: REASON_KICKED.to_string(),
            })
            .expect("MemberLeft serializes to json"),
        ),
        (
            "group.role_changed",
            1,
            serde_json::to_value(RoleChanged {
                group_id: "group-1".to_string(),
                player_id: "player-2".to_string(),
                role: "admin".to_string(),
                actor_kind: ACTOR_OPERATOR.to_string(),
                actor_id: String::new(),
            })
            .expect("RoleChanged serializes to json"),
        ),
    ]
}

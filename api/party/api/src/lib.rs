//! `partyapi` — the party module's PURE, transport-free contract for short-lived player
//! parties: at most [`MAX_MEMBERS`] players, one leader, one party per player. The
//! edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives in the
//! sibling `partyrpc`, which expands this crate's metadata-callback macro, so THIS crate
//! never depends on `edge`.
//!
//! One capability: [`Player`], the `#[http]` player face.
//!
//! **Scalars here are `i64` and `String` only.** `tools/csharp-client-gen`'s type lattice
//! does not model `bool`, `u32` or `usize`, and every `#[http]`-reachable DTO field is
//! mapped through it, so any of those fails the blocking codegen-freshness stage. A ready
//! state is a `String` with exported consts, never a Rust enum, and absence is the empty
//! string.
//!
//! Anything the caller may not see answers [`opsapi::Status::NotFound`], never
//! `Forbidden`: a 403 would confirm the id names a real party, invite or member, which is
//! an enumeration oracle. A malformed uuid argument or an unknown ready state is
//! [`opsapi::Status::Invalid`].
//!
//! **Ordering.** `revision` is drawn from ONE global sequence. The stamping ops are
//! [`Player::create`], [`Player::accept`], [`Player::leave`], [`Player::kick`],
//! [`Player::promote`], [`Player::set_ready`], a lease-expiry removal and a
//! `player.deleted` purge removal; each stamps the party and every player whose
//! membership it changes. Lease renewal never stamps. A purge removal is NOT pushed:
//! the remaining members learn of it through [`Player::get`].
//!
//! A member's view carries the party's revision; a "no party" view carries the caller's
//! own last membership-change revision, `0` if it never had one. Client rule, for op
//! responses and [`PUSH_STATE_TOPIC`] pushes alike: apply a snapshot only when its
//! `revision >= hw`, then set `hw = revision`. `hw` is per connection and resets to 0 on
//! (re)connect. Equal revisions denote the same membership and ready state; handles may
//! differ (a failed directory lookup leaves `""`), so a client keeps non-empty handles
//! when applying an equal-revision snapshot.
//!
//! **Liveness.** Membership is a lease: every op by a member renews it, and a member whose
//! lease expires is removed as if it had called [`Player::leave`]. A client keeps its
//! membership with [`Player::heartbeat`]. Renewal changes no `revision` and pushes nothing.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Upper bound on members per party, counted under the party's lock.
pub const MAX_MEMBERS: i64 = 8;

/// A member's ready state after [`Player::set_ready`] with this value.
pub const READY: &str = "ready";
/// A member's ready state on joining, and after [`Player::set_ready`] with this value.
pub const NOT_READY: &str = "not_ready";

/// Push topic carrying a [`PartyView`] to every current member after a committed stamping
/// op (except a purge removal), and a "no party" view to a member just removed.
/// Best-effort: the authority is [`Player::get`].
pub const PUSH_STATE_TOPIC: &str = "party.state";
/// Push topic carrying an [`InviteView`] to the invitee after [`Player::invite`].
/// Best-effort: the authority is [`Player::invites`].
pub const PUSH_INVITE_TOPIC: &str = "party.invite";

/// Seconds an invite stays acceptable after it is (re)issued.
pub const INVITE_TTL_SECS: i64 = 120;

/// One member of a party.
///
/// `handle` is `"Name#1234"`, resolved after commit; it is EMPTY when the lookup failed
/// or `accounts` has no row for the id, never an error — the membership change already
/// committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberView {
    pub player_id: String,
    pub handle: String,
    /// [`READY`] or [`NOT_READY`].
    pub ready: String,
}

/// The caller's party. "No party" is `party_id == ""`, `leader_id == ""`, no members, and
/// `revision` is the caller's own last membership-change revision (`0` if none).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyView {
    pub party_id: String,
    pub revision: i64,
    pub leader_id: String,
    pub members: Vec<MemberView>,
}

/// One pending invite addressed to its invitee.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteView {
    pub invite_id: String,
    pub party_id: String,
    /// Empty when the lookup failed, as on [`MemberView::handle`].
    pub inviter_handle: String,
    /// RFC3339.
    pub expires_at: String,
}

/// The caller's unexpired invites.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteList {
    pub invites: Vec<InviteView>,
}

/// The caller's renewed lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatView {
    /// RFC3339; EMPTY when the caller is in no party.
    pub lease_until: String,
    /// The party's revision, or the caller's own last membership-change revision (`0` if
    /// none) when in no party — the same value [`Player::get`] would answer.
    pub revision: i64,
}

/// The player-facing party capability. Every op is `POST` with its arguments in the JSON
/// body.
///
/// Mutations are NOT `#[retry_safe]`: a replay after an ambiguous failure cannot be told
/// apart from a second, intentional call, so the client re-reads with [`Player::get`].
#[rpc(prefix = "party")]
#[async_trait]
pub trait Player: Send + Sync {
    /// Creates a party led by the caller. A caller already in a party is
    /// [`opsapi::Status::Conflict`].
    #[http(verb = "POST", path = "/party/create", auth = "player", success = 201)]
    async fn create(&self, identity: Identity) -> Result<PartyView, Error>;

    /// The caller's party, or the "no party" view — never an error for being in none.
    #[http(verb = "POST", path = "/party/get", auth = "player", success = 200)]
    #[retry_safe]
    async fn get(&self, identity: Identity) -> Result<PartyView, Error>;

    /// LEADER ONLY — any other caller is [`opsapi::Status::NotFound`]. Invites the player
    /// named by `target_handle` (`"Name#1234"`), or reissues the existing invite with a
    /// fresh [`INVITE_TTL_SECS`]. A handle over `accountsapi::MAX_HANDLE_BYTES` or the
    /// caller's own is [`opsapi::Status::Invalid`]; an unknown handle is
    /// [`opsapi::Status::NotFound`]; a target already in this party is
    /// [`opsapi::Status::Conflict`]; a directory outage is
    /// [`opsapi::Status::Unavailable`].
    #[http(verb = "POST", path = "/party/invite", auth = "player", success = 201)]
    async fn invite(&self, identity: Identity, target_handle: String)
        -> Result<InviteView, Error>;

    /// The caller's own unexpired invites.
    #[http(verb = "POST", path = "/party/invites", auth = "player", success = 200)]
    #[retry_safe]
    async fn invites(&self, identity: Identity) -> Result<InviteList, Error>;

    /// Joins the inviting party and consumes the invite. An invite that is not the
    /// caller's, expired or nonexistent is [`opsapi::Status::NotFound`]; a caller already
    /// in a party or a party at [`MAX_MEMBERS`] is [`opsapi::Status::Conflict`].
    #[http(verb = "POST", path = "/party/accept", auth = "player", success = 200)]
    async fn accept(&self, identity: Identity, invite_id: String) -> Result<PartyView, Error>;

    /// Drops the caller's own invite. An invite that is not the caller's or nonexistent is
    /// [`opsapi::Status::NotFound`], so a replay is a 404.
    #[http(verb = "POST", path = "/party/decline", auth = "player", success = 204)]
    async fn decline(&self, identity: Identity, invite_id: String) -> Result<(), Error>;

    /// Leaves the caller's party and answers the "no party" view. A leaving leader hands
    /// leadership to the earliest-joined remaining member; the last member leaving
    /// dissolves the party. A caller in no party gets the "no party" view, not an error.
    #[http(verb = "POST", path = "/party/leave", auth = "player", success = 200)]
    async fn leave(&self, identity: Identity) -> Result<PartyView, Error>;

    /// LEADER ONLY. Removes `player_id` from the caller's party. The caller itself is
    /// [`opsapi::Status::Invalid`] — use [`Player::leave`] — checked BEFORE the leader
    /// check, so a non-leader self-kick is also `Invalid`. Otherwise a non-leader caller or
    /// a target not in the caller's party is [`opsapi::Status::NotFound`].
    #[http(verb = "POST", path = "/party/kick", auth = "player", success = 200)]
    async fn kick(&self, identity: Identity, player_id: String) -> Result<PartyView, Error>;

    /// LEADER ONLY. Hands leadership to `player_id`. The caller itself is
    /// [`opsapi::Status::Invalid`], checked BEFORE the leader check, so a non-leader
    /// self-promote is also `Invalid`. Otherwise a non-leader caller or a target not in the
    /// caller's party is [`opsapi::Status::NotFound`].
    #[http(verb = "POST", path = "/party/promote", auth = "player", success = 200)]
    async fn promote(&self, identity: Identity, player_id: String)
        -> Result<PartyView, Error>;

    /// Sets the caller's own ready state to [`READY`] or [`NOT_READY`]; any other value is
    /// [`opsapi::Status::Invalid`]. A caller in no party is [`opsapi::Status::NotFound`].
    #[http(verb = "POST", path = "/party/set_ready", auth = "player", success = 200)]
    async fn set_ready(&self, identity: Identity, state: String) -> Result<PartyView, Error>;

    /// Renews the caller's lease and answers its new expiry, or an empty `lease_until`
    /// when the caller is in no party — never an error for being in none.
    #[http(verb = "POST", path = "/party/heartbeat", auth = "player", success = 200)]
    #[retry_safe]
    async fn heartbeat(&self, identity: Identity) -> Result<HeartbeatView, Error>;
}

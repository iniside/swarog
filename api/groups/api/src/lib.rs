//! `groupsapi` — the groups module's PURE, transport-free contract for social-group
//! membership. The edge-dependent glue (`Client`, `register_server`, `provide_remote`)
//! lives in the sibling `groupsrpc`, which expands this crate's metadata-callback
//! macros, so THIS crate never depends on `edge`.
//!
//! Two capabilities: [`Player`] is the `#[http]` player face, [`Membership`] is a
//! wire-only server-to-server predicate `chat` will consume to authorize a group
//! channel — shipped in the same rollout as the player face rather than as a later
//! retrofit (`walletapi::Wallet` beside `walletapi::Player` is the shipped precedent).
//!
//! **Scalars here are `i64` and `String` only.** `tools/csharp-client-gen`'s type
//! lattice does not model `bool`, `u32` or `usize`, and every `#[http]`-reachable DTO
//! field is mapped through it, so any of those fails the blocking codegen-freshness
//! stage. A role/state/join-policy is a `String` with exported consts, never a Rust
//! enum, and absence is the empty string.
//!
//! Anything the caller may not see answers [`opsapi::Status::NotFound`], never
//! `Forbidden`: a 403 would confirm the id names a real group, which is an
//! enumeration oracle.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Byte cap on [`GroupSummary::name`], enforced in the service before the statement,
/// with the schema's `groups_name_len_check` as the backstop.
pub const MAX_NAME_BYTES: usize = 64;

/// Byte cap on the opaque paging cursor. A longer cursor is
/// [`opsapi::Status::Invalid`] (400) before it is decoded.
pub const MAX_CURSOR_BYTES: usize = 256;

/// Upper bound on the `limit` accepted by every paged read; a larger value is clamped
/// to this, not rejected.
pub const MAX_PAGE_LIMIT: i64 = 100;

/// The page size a paged read uses when `limit == 0`.
pub const DEFAULT_PAGE_LIMIT: i64 = 50;

/// Upper bound on live rows per group (members plus pending), enforced in the service.
pub const MAX_MEMBERS: i64 = 500;

/// A membership row's state. Empty string is never a state — absence is no row.
pub const STATE_MEMBER: &str = "member";
/// A membership row awaiting the SUBJECT's own answer (an invite).
pub const STATE_INVITED: &str = "invited";
/// A membership row awaiting an ADMIN's answer (a join request).
pub const STATE_REQUESTED: &str = "requested";

/// A member's role. Only a [`STATE_MEMBER`] row carries one; `invited`/`requested`
/// rows carry the empty string.
pub const ROLE_ADMIN: &str = "admin";
/// A plain, non-admin member.
pub const ROLE_MEMBER: &str = "member";

/// [`Player::join`] admits immediately.
pub const JOIN_OPEN: &str = "open";
/// [`Player::join`] records a [`STATE_REQUESTED`] row an admin decides via
/// [`Player::decide`].
pub const JOIN_REQUEST: &str = "request";
/// [`Player::join`] is refused; only [`Player::invite`] followed by
/// [`Player::respond`] admits.
pub const JOIN_INVITE: &str = "invite";

/// One group as the caller sees it: its own metadata plus the CALLER's own row.
///
/// `my_state` is one of [`STATE_MEMBER`]/[`STATE_INVITED`]/[`STATE_REQUESTED`];
/// `my_role` is [`ROLE_ADMIN`]/[`ROLE_MEMBER`] when `my_state` is [`STATE_MEMBER`] and
/// empty otherwise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub id: String,
    pub name: String,
    pub join_policy: String,
    /// RFC3339.
    pub created_at: String,
    pub my_state: String,
    pub my_role: String,
}

/// One member/pending row as [`Player::members`]/[`Player::pending`] list it.
///
/// `handle` is `"Name#1234"`, resolved once per page via
/// `accountsapi::Directory::players_by_id` — empty when `accounts` has no row for the
/// id, never an error: a member whose account row vanished must not make the whole
/// page fail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub player_id: String,
    pub handle: String,
    pub state: String,
    pub role: String,
    /// RFC3339.
    pub joined_at: String,
}

/// One page of [`Player::list_mine`], newest first. `next_cursor` is opaque and EMPTY
/// when the page is the last one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPage {
    pub items: Vec<GroupSummary>,
    pub next_cursor: String,
}

/// One page of [`Player::members`]/[`Player::pending`], newest first. `next_cursor` is
/// opaque and EMPTY when the page is the last one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberPage {
    pub items: Vec<MemberSummary>,
    pub next_cursor: String,
}

/// The player-facing group-membership capability.
///
/// The cursor and the page size ride the REQUEST BODY on every read, which is why they
/// are `POST`: the `#[http]` grammar sources an argument from a path wildcard or the
/// JSON body, and has no query-parameter source.
#[rpc(prefix = "groups")]
#[async_trait]
pub trait Player: Send + Sync {
    /// Creates a group with the given `join_policy` (one of [`JOIN_OPEN`],
    /// [`JOIN_REQUEST`], [`JOIN_INVITE`] — anything else is
    /// [`opsapi::Status::Invalid`]) and admits the caller as its [`ROLE_ADMIN`]
    /// member in the same transaction.
    #[http(verb = "POST", path = "/groups", auth = "player", success = 201)]
    async fn create(&self, identity: Identity, name: String, join_policy: String)
        -> Result<GroupSummary, Error>;

    /// Every group the caller holds ANY row in — [`STATE_MEMBER`], [`STATE_INVITED`]
    /// and [`STATE_REQUESTED`] alike, each carrying its own state. This is also the
    /// invitation inbox: without pending rows here an invited player has no way to
    /// discover the invitation.
    #[http(verb = "POST", path = "/groups/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list_mine(&self, identity: Identity, cursor: String, limit: i64)
        -> Result<GroupPage, Error>;

    /// [`STATE_MEMBER`] rows only. Any member of the group may read it; a non-member
    /// or a nonexistent group both answer [`opsapi::Status::NotFound`].
    #[http(verb = "POST", path = "/groups/{id}/members/list", auth = "player",
           success = 200, path_args(group_id = "id"))]
    #[retry_safe]
    async fn members(&self, identity: Identity, group_id: String, cursor: String, limit: i64)
        -> Result<MemberPage, Error>;

    /// [`STATE_REQUESTED`] and [`STATE_INVITED`] rows. ADMIN ONLY — a non-admin caller
    /// answers [`opsapi::Status::NotFound`], the same as a nonexistent group. This is
    /// the op that hands an admin the `subject_id` that [`Player::decide`] needs.
    #[http(verb = "POST", path = "/groups/{id}/pending/list", auth = "player",
           success = 200, path_args(group_id = "id"))]
    #[retry_safe]
    async fn pending(&self, identity: Identity, group_id: String, cursor: String, limit: i64)
        -> Result<MemberPage, Error>;

    /// Admits the caller per the group's `join_policy`: [`JOIN_OPEN`] admits
    /// immediately as [`ROLE_MEMBER`]; [`JOIN_REQUEST`] records a [`STATE_REQUESTED`]
    /// row; [`JOIN_INVITE`] is [`opsapi::Status::Conflict`] — only an invite admits.
    /// A caller already holding ANY row is [`opsapi::Status::Conflict`], never a
    /// silent second row. NOT `#[retry_safe]`: a replay after an ambiguous failure
    /// cannot be told apart from a second, intentional call.
    #[http(verb = "POST", path = "/groups/{id}/join", auth = "player", success = 200,
           path_args(group_id = "id"))]
    async fn join(&self, identity: Identity, group_id: String) -> Result<MemberSummary, Error>;

    /// Drops the caller's own row, whatever state it is in. The only member leaving
    /// deletes the group row in the same transaction. The last [`ROLE_ADMIN`] leaving
    /// a group that still has other members is [`opsapi::Status::Conflict`] — without
    /// this the group freezes forever. NOT `#[retry_safe]`.
    #[http(verb = "POST", path = "/groups/{id}/leave", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn leave(&self, identity: Identity, group_id: String) -> Result<(), Error>;

    /// ADMIN ONLY. Creates a [`STATE_INVITED`] row for the player named by
    /// `target_handle`. A subject already holding any row is
    /// [`opsapi::Status::Conflict`]. NOT `#[retry_safe]`.
    #[http(verb = "POST", path = "/groups/{id}/invites", auth = "player", success = 201,
           path_args(group_id = "id"))]
    async fn invite(&self, identity: Identity, group_id: String, target_handle: String)
        -> Result<(), Error>;

    /// The SUBJECT's own verdict on its own [`STATE_INVITED`] row. `decision` is
    /// `"accept"` or `"reject"` (anything else is [`opsapi::Status::Invalid`]):
    /// accept promotes the row to [`STATE_MEMBER`]/[`ROLE_MEMBER`], reject deletes it.
    /// A row not in [`STATE_INVITED`] answers [`opsapi::Status::NotFound`]. NOT
    /// `#[retry_safe]`.
    #[http(verb = "POST", path = "/groups/{id}/respond", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn respond(&self, identity: Identity, group_id: String, decision: String)
        -> Result<(), Error>;

    /// An ADMIN's verdict on `subject_id`'s row, and the only way to remove another
    /// member: on a [`STATE_REQUESTED`] subject, accept promotes to
    /// [`STATE_MEMBER`]/[`ROLE_MEMBER`] and reject deletes; on a [`STATE_MEMBER`]
    /// subject, accept is [`opsapi::Status::Conflict`] and reject deletes (a kick).
    /// The subject being the caller itself is [`opsapi::Status::Conflict`] — use
    /// [`Player::leave`]. NOT `#[retry_safe]`.
    #[http(verb = "POST", path = "/groups/{id}/decide", auth = "player", success = 204,
           path_args(group_id = "id"))]
    async fn decide(&self, identity: Identity, group_id: String, subject_id: String,
                    decision: String) -> Result<(), Error>;
}

/// The wire-only, server-to-server membership predicate `chat` consumes to authorize a
/// group channel. Provided under `registry::key("groups", "membership")`.
#[rpc(prefix = "groups")]
#[async_trait]
pub trait Membership: Send + Sync {
    /// The caller's role in the group, or the empty string when it holds no
    /// [`STATE_MEMBER`] row — deliberately the SAME empty answer for "not a member"
    /// and "no such group", because a consumer authorizing a channel wants the same
    /// `NotFound` outcome for both and a second, distinguishing method would only
    /// hand it a distinction it must then discard.
    ///
    /// No `Identity`: this must never be reachable from the front door, where it
    /// would be an oracle over group rosters.
    #[retry_safe]
    async fn role_of(&self, group_id: String, player_id: String) -> Result<String, Error>;
}

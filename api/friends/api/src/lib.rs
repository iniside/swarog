//! `friendsapi` — the friends module's PURE, transport-free contract for the social
//! graph. The edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives
//! in the sibling `friendsrpc`, which expands this crate's metadata-callback macro, so
//! THIS crate never depends on `edge`.
//!
//! A friendship is a two-player object, so every op takes its caller `Identity` as the
//! leading param (injected by the gateway after bearer verification) and addresses the
//! relation by its own `edge_id` — never by the other player's id. One addressing scheme
//! across all four mutating ops means a caller never has to know which side of the pair
//! it is on.
//!
//! **Another player's edge is [`opsapi::Status::NotFound`], never `Forbidden`**: a 403
//! would confirm that the id names a real relation the caller is not part of, which is an
//! enumeration oracle. The same answer covers "no such edge" and "not yours".
//!
//! **Scalars here are `i64` and `String` only.** `tools/csharp-client-gen`'s type lattice
//! does not model `u32` or `bool` (`UNMODELLED_SCALARS`), and every `#[http]`-reachable
//! DTO field is mapped through it, so either type fails the blocking codegen-freshness
//! stage.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Upper bound on the `limit` accepted by [`Player::list`] and [`Player::pending`]; a
/// larger value is clamped to this, not rejected.
pub const MAX_PAGE_LIMIT: i64 = 100;

/// The page size those two ops use when `limit == 0`.
pub const DEFAULT_PAGE_LIMIT: i64 = 50;

/// Byte cap on the opaque paging cursor (`str::len()`, not a character count). A longer
/// cursor is [`opsapi::Status::Invalid`] (400) before it is decoded.
pub const MAX_CURSOR_BYTES: usize = 256;

/// How many requests one player may have OUTSTANDING — rows they authored that are still
/// awaiting an answer. A [`Player::request`] that would exceed it is refused, which is
/// what bounds both the table's growth and the rate at which one caller can probe for
/// which handles exist.
pub const MAX_PENDING_OUTSTANDING: i64 = 100;

/// One entry of the caller's social graph: the OTHER player, plus the relation itself.
///
/// `player_id`, `display_name` and `handle` describe the other party;
/// [`Friend::display_name`] and [`Friend::handle`] are EMPTY when the directory could not
/// resolve that id, so a name the directory has lost never drops the relation from the
/// page. `online_until` is an RFC3339 timestamp and is EMPTY when the player has no live
/// session — a timestamp rather than a `bool` because a session outlives a quit by up to
/// its access-token lifetime, and the client, not this contract, decides what to render
/// from that.
///
/// `edge_id` is the relation's id and the argument every mutating op takes. `state` is
/// `"pending"` (requested, unanswered) or `"accepted"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    pub player_id: String,
    pub display_name: String,
    pub handle: String,
    pub online_until: String,
    pub edge_id: String,
    pub state: String,
}

/// One page of [`Player::list`] or [`Player::pending`], newest first.
///
/// `next_cursor` is opaque and EMPTY when the page is the last one — a caller pages until
/// it is empty, never on a short page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Friend>,
    pub next_cursor: String,
}

/// The player-facing social-graph capability. No wire-only face exists: nothing else in
/// the repo reads or writes another player's friend list synchronously — the fan-out to
/// `audit` and `notifications` arrives over the durable topics in `friendsevents`
/// instead.
///
/// The cursor and the page size ride the REQUEST BODY on both reads, which is why they
/// are `POST`: the `#[http]` grammar sources an argument from a path wildcard or the JSON
/// body, and has no query-parameter source.
#[rpc(prefix = "friends")]
#[async_trait]
pub trait Player: Send + Sync {
    /// Requests friendship with the player whose `"Name#1234"` handle matches. Returns
    /// the resulting relation, so a caller learns from `state` whether it is now
    /// `"pending"` or was immediately `"accepted"` — which is what a request crossing the
    /// other player's own pending request produces.
    ///
    /// An unresolvable handle is [`opsapi::Status::NotFound`] (404) and the caller's own
    /// handle is [`opsapi::Status::Invalid`] (400).
    #[http(verb = "POST", path = "/friends/requests", auth = "player", success = 201)]
    async fn request(&self, identity: Identity, target_handle: String) -> Result<Friend, Error>;

    /// Accepts the pending request named by `edge_id`. Only the party who did NOT author
    /// it can accept; for anyone else — the author included — the edge is
    /// [`opsapi::Status::NotFound`] (404).
    #[http(verb = "POST", path = "/friends/requests/{id}/accept", auth = "player",
           success = 204, path_args(edge_id = "id"))]
    async fn accept(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

    /// Refuses the pending request named by `edge_id`, dropping the relation. Same
    /// party rule and same 404 as [`Player::accept`].
    #[http(verb = "POST", path = "/friends/requests/{id}/decline", auth = "player",
           success = 204, path_args(edge_id = "id"))]
    async fn decline(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

    /// Drops the relation named by `edge_id`. EITHER party may remove, in either state,
    /// so this also withdraws a request the caller itself authored. NOT `#[retry_safe]`:
    /// a replay after a successful removal is a [`opsapi::Status::NotFound`] (404),
    /// exactly like `notificationsapi::Player::delete`.
    #[http(verb = "DELETE", path = "/friends/{id}", auth = "player",
           success = 204, path_args(edge_id = "id"))]
    async fn remove(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

    /// The caller's ACCEPTED friends, newest first. An empty `cursor` starts at the
    /// newest row; `limit == 0` means [`DEFAULT_PAGE_LIMIT`] and a `limit` above
    /// [`MAX_PAGE_LIMIT`] is clamped to it. A negative `limit`, or a cursor that is
    /// malformed or longer than [`MAX_CURSOR_BYTES`], is [`opsapi::Status::Invalid`]
    /// (400) — never a silent reset to the first page.
    #[http(verb = "POST", path = "/friends/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;

    /// The caller's PENDING relations — every unanswered request they are party to,
    /// whichever side authored it. Same paging rules as [`Player::list`].
    ///
    /// [`Friend`] carries no author, so this page does NOT distinguish a request the
    /// caller received from one it sent; both render as `state == "pending"`. Only the
    /// received ones can be accepted or declined — the rest answer 404.
    #[http(verb = "POST", path = "/friends/requests/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn pending(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;
}

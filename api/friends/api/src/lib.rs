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

/// [`Friend::state`] for a relation that has been requested and not yet answered.
pub const STATE_PENDING: &str = "pending";

/// [`Friend::state`] for a mutual relation.
pub const STATE_ACCEPTED: &str = "accepted";

/// [`Friend::direction`] for a relation the OTHER player asked for.
pub const DIRECTION_INCOMING: &str = "incoming";

/// [`Friend::direction`] for a relation the CALLER asked for.
pub const DIRECTION_OUTGOING: &str = "outgoing";

/// How many requests one player may have OUTSTANDING — rows they authored that are still
/// awaiting an answer. A [`Player::request`] that would exceed it is
/// [`opsapi::Status::Conflict`] (409) — the same answer `characters::create` gives when
/// its per-player cap is reached, and not a 400, because the request itself is
/// well-formed and becomes acceptable again once the caller's queue drains. It bounds
/// both the table's growth and the rate at which one caller can probe for which handles
/// exist.
pub const MAX_PENDING_OUTSTANDING: i64 = 100;

/// One entry of the caller's social graph: the OTHER player, plus the relation itself.
///
/// `player_id`, `display_name` and `handle` describe the other party. `online_until` is an
/// RFC3339 timestamp and is EMPTY when the player has no live session — a timestamp rather
/// than a `bool` because a session outlives a quit by up to its access-token lifetime, and
/// the client, not this contract, decides what to render from that.
///
/// **An empty `display_name`/`handle` means a directory MISS, never a directory outage.**
/// A player id the directory does not return keeps its row with those two fields empty, so
/// a name lost from `accounts` cannot drop a friendship out of the page. A directory
/// FAILURE is a different outcome entirely: the read propagates it rather than serving
/// blanks — see [`Player::list`].
///
/// `edge_id` is the relation's id and the argument every mutating op takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    pub player_id: String,
    pub display_name: String,
    pub handle: String,
    pub online_until: String,
    pub edge_id: String,
    /// [`STATE_PENDING`] or [`STATE_ACCEPTED`].
    pub state: String,
    /// Who ASKED: [`DIRECTION_INCOMING`] (the other player did) or [`DIRECTION_OUTGOING`]
    /// (the caller did). Populated on every row, in both states — on an accepted one it
    /// records who originally asked, which the admin page reads too. It is what makes a
    /// [`Player::pending`] page actionable: only an incoming request can be accepted or
    /// declined. The alternative — publishing the requester's id and having each client
    /// compare it against its own — would force a client to know its own `player_id` to
    /// render a screen whose every other field is already caller-relative.
    pub direction: String,
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
    /// Requests friendship with the player whose `"Name#1234"` handle matches. Returns the
    /// resulting relation, so a caller learns from [`Friend::state`] whether it is now
    /// [`STATE_PENDING`] or was immediately [`STATE_ACCEPTED`] — which is what a request
    /// crossing the other player's own pending request produces.
    ///
    /// **Repeating a request that already succeeded is 201 with the relation's CURRENT
    /// state, never a [`opsapi::Status::Conflict`].** Whether the caller's earlier request
    /// is still pending or has since been accepted, the answer is the same success plus the
    /// relation as it stands, and nothing is emitted the second time. The method is not
    /// `#[retry_safe]`, so a client that lost the response has to repeat it by hand; a 409
    /// there would report an operation that DID succeed as an error.
    ///
    /// An unresolvable handle is [`opsapi::Status::NotFound`] (404), the caller's own
    /// handle is [`opsapi::Status::Invalid`] (400), and a caller already holding
    /// [`MAX_PENDING_OUTSTANDING`] unanswered requests is [`opsapi::Status::Conflict`]
    /// (409).
    #[http(verb = "POST", path = "/friends/requests", auth = "player", success = 201)]
    async fn request(&self, identity: Identity, target_handle: String) -> Result<Friend, Error>;

    /// Accepts the pending request named by `edge_id`. Only the party who did NOT author
    /// it can accept; for anyone else — the author included — the edge is
    /// [`opsapi::Status::NotFound`] (404).
    ///
    /// A REPLAY is that same 404: once the edge is accepted it is no longer pending, and
    /// once it is declined or removed it is gone. Only a pending edge answers 204, which
    /// is why this is not `#[retry_safe]` — a replay after an ambiguous failure cannot be
    /// told apart from an accept of an edge the caller never had.
    #[http(verb = "POST", path = "/friends/requests/{id}/accept", auth = "player",
           success = 204, path_args(edge_id = "id"))]
    async fn accept(&self, identity: Identity, edge_id: String) -> Result<(), Error>;

    /// Refuses the pending request named by `edge_id`, dropping the relation. Same party
    /// rule, same replay behaviour and same 404 as [`Player::accept`].
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
    ///
    /// Each row's name and handle come from a batched `accounts` directory lookup, and the
    /// two ways that can fall short are DIFFERENT answers. An id the directory does not
    /// return is a miss: that row is served with both fields empty (see [`Friend`]). The
    /// directory being unreachable is [`opsapi::Status::Unavailable`] (503) and NO page —
    /// a 200 carrying a screenful of nameless friends is indistinguishable from a real
    /// list of deleted accounts, so the client would render, and may cache, an anonymous
    /// friend list with nothing reporting the outage.
    #[http(verb = "POST", path = "/friends/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;

    /// The caller's PENDING relations — every unanswered request they are party to,
    /// whichever side authored it — [`Friend::direction`] is what separates the ones the
    /// caller can accept ([`DIRECTION_INCOMING`]) from the ones it is waiting on
    /// ([`DIRECTION_OUTGOING`]). Same paging rules and same directory-failure answer as
    /// [`Player::list`].
    #[http(verb = "POST", path = "/friends/requests/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn pending(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;
}

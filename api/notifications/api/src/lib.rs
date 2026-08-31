//! `notificationsapi` — the notifications module's PURE, transport-free contract for the
//! in-app inbox. The edge-dependent glue (`Client`, `register_server`, `provide_remote`)
//! lives in the sibling `notificationsrpc`, which expands this crate's metadata-callback
//! macro, so THIS crate never depends on `edge`.
//!
//! The inbox is player-owned state: every op takes its caller `Identity` as the leading
//! param (injected by the gateway after bearer verification) and never a `player_id` body
//! field, so a client can only ever reach its own rows.
//!
//! **Scalars here are `i64` and `String` only.** `tools/csharp-client-gen`'s type lattice
//! does not model `u32` or `bool` (`UNMODELLED_SCALARS`), and every `#[http]`-reachable DTO
//! field is mapped through it, so either type fails the blocking codegen-freshness stage.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Byte cap on [`Notification::title`] — a BYTE count (`str::len()`), matching the
/// `octet_length` CHECK on the column.
pub const MAX_TITLE_BYTES: usize = 200;

/// Byte cap on [`Notification::body`].
pub const MAX_BODY_BYTES: usize = 4000;

/// Byte cap on [`Notification::kind`].
pub const MAX_KIND_BYTES: usize = 64;

/// Byte cap on the opaque paging cursor accepted by [`Player::list`]. A longer cursor is
/// `Status::Invalid` (400) before it is decoded.
pub const MAX_CURSOR_BYTES: usize = 128;

/// Upper bound on [`Player::list`]'s `limit`; a larger value is clamped to this, not
/// rejected.
pub const MAX_PAGE_LIMIT: i64 = 100;

/// The page size [`Player::list`] uses when `limit == 0`.
pub const DEFAULT_PAGE_LIMIT: i64 = 25;

/// One inbox row, addressed to exactly one player.
///
/// `kind` is the classifier the client renders on (`"wallet.credit"`,
/// `"account.promoted"`, operator mail); `created_at` is RFC3339.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub created_at: String,
    /// RFC3339 read timestamp; the EMPTY STRING means unread. Not a `bool`:
    /// `csharp-client-gen`'s type lattice does not model `bool` on a player-facing
    /// surface, and the column is a nullable timestamp anyway.
    pub read_at: String,
}

/// One page of [`Player::list`], newest first.
///
/// `next_cursor` is opaque and EMPTY when the page is the last one — a caller pages until
/// it is empty, never on a short page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Notification>,
    pub next_cursor: String,
}

/// The player-facing inbox capability. No wire-only face exists: nothing else in the repo
/// reads or writes another player's inbox synchronously — the fan-in arrives over durable
/// events instead.
#[rpc(prefix = "notifications")]
#[async_trait]
pub trait Player: Send + Sync {
    /// The caller's own notifications, newest first. An empty `cursor` starts at the
    /// newest row; `limit == 0` means [`DEFAULT_PAGE_LIMIT`] and a `limit` above
    /// [`MAX_PAGE_LIMIT`] is clamped to it. A negative `limit`, or a cursor that is
    /// malformed or longer than [`MAX_CURSOR_BYTES`], is `Status::Invalid` (400) — never a
    /// silent reset to the first page.
    #[http(verb = "POST", path = "/notifications/list", auth = "player", success = 200)]
    #[retry_safe]
    async fn list(&self, identity: Identity, cursor: String, limit: i64) -> Result<Page, Error>;

    /// Stamps the row read, keeping the FIRST read timestamp, so a replay returns the same
    /// state — which is what licenses `#[retry_safe]`. A row belonging to another player is
    /// `Status::NotFound` (404), indistinguishable from an absent one.
    #[http(verb = "POST", path = "/notifications/{id}/read", auth = "player",
           success = 204, path_args(notification_id = "id"))]
    #[retry_safe]
    async fn mark_read(&self, identity: Identity, notification_id: String) -> Result<(), Error>;

    /// Removes one of the caller's rows. NOT `#[retry_safe]`: a replay after a successful
    /// delete is a `Status::NotFound` (404), exactly like `charactersapi::Player::delete`.
    #[http(verb = "DELETE", path = "/notifications/{id}", auth = "player",
           success = 204, path_args(notification_id = "id"))]
    async fn delete(&self, identity: Identity, notification_id: String) -> Result<(), Error>;
}

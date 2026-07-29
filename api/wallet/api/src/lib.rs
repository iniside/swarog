//! `walletapi` — the wallet module's PURE, transport-free capability contract. The
//! edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives in the
//! sibling `walletrpc`, which expands this crate's metadata-callback macros, so THIS crate
//! never depends on `edge`.
//!
//! **Both traits share the `wallet` prefix, so their method names must be globally
//! distinct**: a wire method is `format!("{prefix}.{lowerCamel}")` and
//! `edge::Server::handle*` PANICS on a duplicate registration — hence `my_balances` /
//! `list_currencies` on the player face rather than a second `balances`.
//!
//! **Money never moves at a player's own request.** `credit`/`debit` carry no `#[http]`
//! binding and no caller `Identity`, so they are reachable only by a TRUSTED CALLER —
//! in-process through `registry::require`, or from a peer over the internal mTLS edge in a
//! split. `Player` is reads only.
//!
//! Domain CONSUMERS import this ONLY to name a trait for `registry::require`; they never
//! import the `wallet` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One player's holding of one currency, in MINOR units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    pub amount: i64,
}

/// A catalog row, owned by wallet. `kind` is a free-form classifier (`"soft"`/`"hard"` in
/// the dev seed); `decimals` is a DISPLAY hint only — every amount on the wire is an
/// integer count of minor units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency {
    pub code: String,
    pub display_name: String,
    pub kind: String,
    pub decimals: i32,
}

/// One requested money movement. `amount` is ALWAYS POSITIVE — the direction is the method
/// (`credit` vs `debit`), never the sign of this field.
///
/// `idempotency_key` is REQUIRED and `UNIQUE` in the ledger, so a replay collapses to a
/// no-op returning the ORIGINAL movement's resulting balance — which is what licenses
/// `#[retry_safe]` on the mutating methods below.
///
/// **The replayed-movement identity is `(player_id, currency, signed delta, reason)`** —
/// every field except the key. A resubmit under the same key differing in ANY of them,
/// `reason` included, is a `Status::Conflict`, not a silent success; a genuine wire replay
/// carries a byte-identical payload and still collapses to the no-op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movement {
    /// **The key namespace is the WHOLE wallet, not one player** — `UNIQUE
    /// (idempotency_key)` carries no `player_id`, deliberately, so a key reused across two
    /// players is a loud `Status::Conflict` rather than a silently absorbed caller bug. One
    /// key per BUSINESS EVENT credited to 200 players is therefore 1 movement and 199
    /// unpaid: mint per `(player, business event)`,
    /// e.g. `"season-3-payout-batch-7:{player_id}"`.
    pub idempotency_key: String,
    pub player_id: String,
    pub currency: String,
    /// Always positive; the direction is the method. Bounded by [`MAX_MOVEMENT_AMOUNT`].
    pub amount: i64,
    /// Free-form note written to the ledger row. Part of the movement's identity (see the
    /// type docs), so it is NOT cosmetic.
    pub reason: String,
}

/// Byte cap on [`Movement::idempotency_key`] — a BYTE count (`str::len()`), not a
/// character count.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// Byte cap on a currency code as supplied by a caller ([`Movement::currency`]).
pub const MAX_CURRENCY_CODE_BYTES: usize = 32;

/// Byte cap on [`Movement::reason`] — the human-readable note written to the ledger row.
pub const MAX_REASON_BYTES: usize = 256;

/// Upper bound on a single [`Movement::amount`] (10^12 minor units). On the CALLER-facing
/// paths a movement outside `1 ..= MAX_MOVEMENT_AMOUNT` is `Status::Invalid` (400) before
/// any SQL runs. The bounds are what keep a `bigint` overflow (SQLSTATE 22003, mapped
/// nowhere) and a `-i64::MIN` negation out of the balance update, leaving the mapped
/// 23514/409 as the only way past the ceiling.
///
/// **One deliberate exception: the durable starter-grant path.** Its amount is an
/// operator-editable `config` knob rather than caller input, so an out-of-range value warns
/// and grants nothing instead of returning `Err` — which would back off and pause the
/// subscription for every subsequent player.
pub const MAX_MOVEMENT_AMOUNT: i64 = 1_000_000_000_000;

/// The wallet module's SERVER-side capability: reading any player's balances and moving
/// money. WIRE-ONLY — no leading `Identity` and no `#[http]`; it rides the internal mTLS
/// edge like `accounts.sessions`.
///
/// `credit`/`debit` are `#[retry_safe]` ONLY because a required `idempotency_key` makes a
/// replay return the ORIGINAL movement's `balance_after`. If the key becomes optional, or
/// the duplicate arm ever re-reads the LIVE balance, the attribute comes off in that diff.
#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Wallet: Send + Sync {
    /// EVERY balance row the player holds, including a currency debited back to zero. An
    /// unknown player is an empty `Vec`, not an error.
    #[retry_safe]
    async fn balances(&self, player_id: String) -> Result<Vec<Balance>, Error>;

    /// The currency catalog.
    #[retry_safe]
    async fn currencies(&self) -> Result<Vec<Currency>, Error>;

    /// Adds `movement.amount` to the player's balance and appends the ledger row in one
    /// transaction, returning the resulting balance.
    ///
    /// A duplicate `idempotency_key` whose `(player_id, currency, signed delta, reason)`
    /// matches the stored row returns THAT row's `balance_after` without moving money; a
    /// duplicate key differing in any of those fields is `Status::Conflict` (409).
    /// `amount` outside `1 ..= MAX_MOVEMENT_AMOUNT` is `Status::Invalid` (400); an
    /// unknown currency is `Status::Invalid` (400); a credit that would push the balance
    /// past its ceiling is `Status::Conflict` (409).
    #[retry_safe]
    async fn credit(&self, movement: Movement) -> Result<i64, Error>;

    /// The mirror of [`Wallet::credit`], subtracting instead — same idempotency identity,
    /// same `amount` bounds. A movement that would take the balance below zero is
    /// rejected by the DB CHECK as `Status::Conflict` (409) and consumes no idempotency
    /// key (the aborted transaction takes the ledger row with it), so the caller may
    /// retry the SAME key after a top-up.
    #[retry_safe]
    async fn debit(&self, movement: Movement) -> Result<i64, Error>;
}

/// The player-facing capability: READS only — a client can never move its own money. Each
/// method takes its caller identity as the leading `Identity` param (injected by the
/// gateway after bearer verification), NEVER a body field, so a client cannot read another
/// player's balances.
#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Player: Send + Sync {
    /// The caller's own balances.
    #[http(verb = "GET", path = "/wallet/me", auth = "player", success = 200)]
    #[retry_safe]
    async fn my_balances(&self, identity: Identity) -> Result<Vec<Balance>, Error>;

    /// The currency catalog, as the client needs it to label amounts. Identity-bearing
    /// rather than public: the catalog is game data.
    #[http(verb = "GET", path = "/wallet/currencies", auth = "player", success = 200)]
    #[retry_safe]
    async fn list_currencies(&self, identity: Identity) -> Result<Vec<Currency>, Error>;
}

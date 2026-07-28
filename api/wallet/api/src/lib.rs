//! `walletapi` — the wallet module's PURE, transport-free capability contract. It
//! declares the two capabilities wallet exposes and applies `#[rpc(prefix = "wallet")]`
//! to both, so the transport-FREE surface (per-method wire envelopes, `METHOD_*`
//! consts, and — for `#[http]` methods — `operations()`/`route_bindings()`) is
//! GENERATED into the child `wallet_rpc`/`player_rpc` modules rather than hand-written.
//! The edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives in the
//! sibling `walletrpc` crate, which expands this crate's metadata-callback macros
//! (`wallet_wallet_meta!` / `wallet_player_meta!`) — so THIS crate never depends on
//! `edge`.
//!
//! **Both traits share the `wallet` prefix, so their method names are globally
//! distinct** (design decision D1): a wire method is `format!("{prefix}.{lowerCamel}")`
//! and `edge::Server::handle*` PANICS on a duplicate registration, so a `Wallet::balances`
//! / `Player::balances` pair would be a boot failure, not a shadowing bug. Hence
//! `my_balances` / `list_currencies` on the player face.
//!
//! **Money never moves at a player's own request.** The mutating methods (`credit`,
//! `debit`) live on the WIRE-ONLY `Wallet` capability — no `#[http]` binding, no caller
//! `Identity` — so they are reachable from a peer process over the internal mTLS edge
//! and from the admin portal, never from a game client. `Player` is reads only.
//!
//! Domain CONSUMERS import this ONLY to name a trait for `registry::require` (rule 4's
//! nominal-typing cost); they never import the `wallet` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One player's holding of one currency, in MINOR units (see [`Movement::amount`]).
/// It lives here (not the impl crate) because it is a return type of both capabilities,
/// so the generated glue must be able to name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    pub amount: i64,
}

/// A currency the wallet knows about — the catalog row wallet owns in its own schema
/// (decision recorded 2026-07-28: the ledger's authority is not split across modules; a
/// module that wants the currency types asks over RPC rather than declaring its own).
///
/// `kind` is a free-form classifier (`"soft"` / `"hard"` in the dev seed), `decimals`
/// is a DISPLAY hint only: every amount on the wire is an integer count of minor units,
/// so no consumer has to agree on a fixed-point representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency {
    pub code: String,
    pub display_name: String,
    pub kind: String,
    pub decimals: i32,
}

/// One requested money movement. `amount` is ALWAYS POSITIVE — the direction is the
/// method (`credit` vs `debit`), never the sign of this field, so a caller cannot turn a
/// credit into a debit by flipping a number.
///
/// `idempotency_key` is REQUIRED (the `match::report` construction): the ledger holds a
/// `UNIQUE` index on it, so a replay of the same key collapses to a no-op returning the
/// ORIGINAL movement's resulting balance. That is precisely what licenses `#[retry_safe]`
/// on the two mutating methods below (D4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movement {
    pub idempotency_key: String,
    pub player_id: String,
    pub currency: String,
    /// Always positive; the direction is the method.
    pub amount: i64,
    pub reason: String,
}

/// Byte cap on [`Movement::idempotency_key`] — a BYTE count (`str::len()`), not a
/// character count, like `apikeysapi::MAX_KEY_BYTES`. The key is client-supplied and
/// indexed, so it is capped at the contract, not at the DB.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// Byte cap on a currency code as supplied by a caller ([`Movement::currency`]).
pub const MAX_CURRENCY_CODE_BYTES: usize = 32;

/// Byte cap on [`Movement::reason`] — the human-readable note written to the ledger row.
pub const MAX_REASON_BYTES: usize = 256;

/// The wallet module's SERVER-side capability: reading any player's balances and moving
/// money. WIRE-ONLY — no leading `Identity` (the caller is a trusted peer process or the
/// admin portal, not a player) and no `#[http]` (not a gateway route; it rides the
/// internal mTLS edge like `accounts.sessions`).
///
/// `credit`/`debit` are `#[retry_safe]` — legal ONLY because every movement carries a
/// required `idempotency_key` and a replay returns the ORIGINAL movement's
/// `balance_after`, making the retry observationally identical to the first call (D4). If
/// the key ever becomes optional, or the duplicate arm ever re-reads the LIVE balance,
/// the attribute must come off in the same diff.
#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Wallet: Send + Sync {
    /// Every non-zero balance the player holds. An unknown player is an empty `Vec`, not
    /// an error — a player with no movements simply holds nothing.
    #[retry_safe]
    async fn balances(&self, player_id: String) -> Result<Vec<Balance>, Error>;

    /// The currency catalog.
    #[retry_safe]
    async fn currencies(&self) -> Result<Vec<Currency>, Error>;

    /// Adds `movement.amount` to the player's balance and appends the ledger row, in one
    /// transaction. Returns the resulting balance. A duplicate `idempotency_key` carrying
    /// the SAME movement returns the original call's resulting balance without moving
    /// money; a duplicate key carrying a DIFFERENT movement is `Status::Conflict` (409).
    #[retry_safe]
    async fn credit(&self, movement: Movement) -> Result<i64, Error>;

    /// The mirror of [`Wallet::credit`], subtracting instead. A movement that would take
    /// the balance below zero is rejected by the DB CHECK as `Status::Conflict` (409) and
    /// consumes no idempotency key — the caller may retry after a top-up.
    #[retry_safe]
    async fn debit(&self, movement: Movement) -> Result<i64, Error>;
}

/// The wallet module's player-facing capability: the two READS a player performs on
/// their OWN wallet. Each takes its caller identity as the leading `Identity` param
/// (injected by the gateway after bearer verification), NEVER a body field — so a client
/// cannot read another player's balances. There is deliberately no player-facing
/// mutation: a client can never move its own money.
#[rpc(prefix = "wallet")]
#[async_trait]
pub trait Player: Send + Sync {
    /// The caller's own balances. 200.
    #[http(verb = "GET", path = "/wallet/me", auth = "player", success = 200)]
    #[retry_safe]
    async fn my_balances(&self, identity: Identity) -> Result<Vec<Balance>, Error>;

    /// The currency catalog, as the client needs it to label amounts. Identity-bearing
    /// (`auth = "player"`) rather than public: the catalog is game data, and the player
    /// front door is already bearer-gated. 200.
    #[http(verb = "GET", path = "/wallet/currencies", auth = "player", success = 200)]
    #[retry_safe]
    async fn list_currencies(&self, identity: Identity) -> Result<Vec<Currency>, Error>;
}

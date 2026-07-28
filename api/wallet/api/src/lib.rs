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
//! `debit`) carry no `#[http]` binding and no caller `Identity`, so they are reachable
//! only by a TRUSTED CALLER — in-process through `registry::require` when the consumer is
//! co-hosted, or from a peer process over the internal mTLS edge in a split — and never
//! from a game client. Which of the two it is depends on the deployment, not on this
//! contract: the registry swap is the only difference. `Player` is reads only.
//!
//! No consumer in this workspace calls `credit`/`debit` yet, in either topology, and that
//! is expected: they exist for the first domain that spends or awards currency. Wallet's
//! own admin surface does NOT go through them — the portal's remote write is
//! `admin.adminSubmit` into wallet's process, which reaches the same movement authority
//! in-process.
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
/// on the two mutating methods below.
///
/// **The replayed-movement identity is the tuple
/// `(player_id, currency, signed delta, reason)`** — every field except the key itself.
/// A resubmit under the same key that differs in ANY of them, `reason` included, is a
/// `Status::Conflict`, not a silent success: a `credit(K, 100, "promo")` followed by a
/// `credit(K, 100, "refund")` must not return OK while the ledger records "promo". A
/// genuine wire replay carries a byte-identical payload and still collapses to the
/// no-op; only an EDITED resubmit is rejected, and it deserves its own key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movement {
    pub idempotency_key: String,
    pub player_id: String,
    pub currency: String,
    /// Always positive; the direction is the method. Bounded by
    /// [`MAX_MOVEMENT_AMOUNT`] — see that const for why the ceiling is part of the
    /// contract rather than a defensive nicety.
    pub amount: i64,
    /// Free-form note written to the ledger row. Part of the movement's identity (see
    /// the type docs), so it is NOT a cosmetic field.
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

/// Upper bound on a single [`Movement::amount`] (10^12 minor units). On the CALLER-facing
/// paths a movement outside `1 ..= MAX_MOVEMENT_AMOUNT` is rejected as `Status::Invalid`
/// (400) BEFORE any SQL runs — the amount is the one numeric input a caller controls, so
/// it is capped at the contract exactly like the three byte caps above.
///
/// This is not tidiness, it is the reason a `bigint` overflow cannot happen. The balance
/// update is `amount + $delta` in Postgres; an overflow there is **SQLSTATE 22003**,
/// which wallet interprets nowhere. On the pool path that is a confusing 500 instead of
/// a 409. On the durable STARTER-GRANT path it is worse: 22003 aborts the delivery
/// transaction, so the plane's checkpoint UPDATE then fails with 25P02 and poisons the
/// subscription — the precise class that already bit inventory once, and the one the
/// starter grant claims to have removed by construction. With this ceiling plus the
/// balance CHECK's own upper bound of 10^15 (Step 2's schema), `balance + amount` stays
/// ~9000x below `i64::MAX`, so the only way to exceed the ceiling is the CHECK
/// violation 23514, which IS mapped — to the same 409 as insufficient funds.
///
/// The lower bound closes a second hole: the service applies a debit as `-amount`, and
/// negating `i64::MIN` panics in a debug build (`1 * i64::MAX` and `-1 * i64::MAX` are
/// both fine — `i64::MIN` is the input that overflows).
///
/// **One deliberate exception to the 400: the durable starter-grant path.** Its amount
/// comes from an operator-editable `config` knob, not from a caller, so an out-of-range
/// value is a property of the configuration rather than of the event — the handler logs a
/// warning and returns `Ok(())`, granting nothing. An `Err` inside the delivery
/// transaction would back off and eventually pause the subscription for every subsequent
/// player, i.e. reach the very poisoning described above through the guard instead of
/// through the overflow. So the same out-of-range value is a 400 on the caller-facing
/// paths and a warn-and-skip on delivery. That asymmetry is the durable handler's
/// never-poison posture, not an inconsistency.
pub const MAX_MOVEMENT_AMOUNT: i64 = 1_000_000_000_000;

/// The wallet module's SERVER-side capability: reading any player's balances and moving
/// money. WIRE-ONLY — no leading `Identity` (the caller is a trusted peer process, not a
/// player) and no `#[http]` (not a gateway route; it rides the internal mTLS edge like
/// `accounts.sessions`).
///
/// `credit`/`debit` are `#[retry_safe]` — legal ONLY because every movement carries a
/// required `idempotency_key` and a replay returns the ORIGINAL movement's
/// `balance_after`, making the retry observationally identical to the first call. If the
/// key ever becomes optional, or the duplicate arm ever re-reads the LIVE balance, the
/// attribute must come off in the same diff.
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
    /// transaction. Returns the resulting balance.
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

//! `wallet` — owns virtual currency: the currency catalog, per-player balances and the
//! append-only ledger that is the authority for every movement. It exposes a WIRE-ONLY
//! capability (read any player's balances, credit, debit — reachable by a trusted peer
//! process, never by a game client) and a player-facing read capability (`GET
//! /wallet/me`, `GET /wallet/currencies`). There is deliberately no player-facing
//! mutation: a client can never move its own money.
//!
//! The one thing to read first is [`Service::apply_on`] — the single movement authority.
//! It runs a whole movement on a CALLER-OWNED connection and never touches transaction
//! control, so the caller-facing `credit`/`debit` (their own pool transaction) and the
//! config-driven starter grant (the event plane's handed delivery transaction) share one
//! implementation instead of growing two ways to move money.
//!
//! The domain write and its durable `wallet.changed` append commit in ONE transaction on
//! the same connection — the event is durable iff the ledger row is. An impl crate: no
//! other module imports it.

pub mod conformance;

mod service;
mod store;

#[allow(unused_imports)] // re-exported so tests.rs's `use super::*;` sees the consts/validators
use service::*;
use store::*;

/// Preserved as public API (the type behind both capability registrations).
pub use service::Service;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use configapi::Config;
use lifecycle::{Context, Module};
use registry::key;
use walletapi::{Player, Wallet};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent DDL only; a schema change is a `DROP SCHEMA wallet CASCADE` + fresh boot,
/// never a data migration (the current phase's strategy).
///
/// `balances.amount` is `bigint` and CHECK-bounded at BOTH ends. The lower bound is what
/// makes "insufficient funds" a DB verdict rather than a read-then-write race — the
/// service never pre-checks a balance. The upper bound (10^15, ~9000x below `i64::MAX`
/// once a single movement is capped at `walletapi::MAX_MOVEMENT_AMOUNT`) is what keeps a
/// `bigint` overflow — SQLSTATE 22003, which nothing maps — unreachable: a credit past
/// the ceiling is the already-mapped 23514 instead. That matters most on the durable
/// path, where an unmapped DB error leaves the delivery transaction aborted and neither
/// available posture is safe (see `Store::currency_exists_tx` for the two arms) — the
/// class that already bit inventory once.
///
/// The FK to the catalog is NAMED so the 23503 → 400 mapping matches a constraint we own;
/// it is an IN-MODULE FK, which is legal — no cross-module FK exists. Those mappings are
/// COUPLED TO THESE NAMES, and `CREATE TABLE IF NOT EXISTS` never repairs a pre-existing
/// table: a `wallet.balances` carrying differently-named constraints would silently turn
/// every insufficient-funds into a 500. Renaming any constraint here is therefore a
/// `DROP SCHEMA wallet CASCADE` + fresh boot, never an `ALTER`.
///
/// **`currencies_code_len_check` is what makes the durable grant's pre-checks exhaustive
/// BY CONSTRUCTION.** `Service::apply_on` validates the movement itself, so ANY movement
/// the starter-grant handler builds that `validate_movement` would reject becomes an
/// `Err` on the delivery path — which posture A forbids. Without this CHECK the catalog
/// could legitimately hold a 33-byte code: `currency_exists_tx` would answer `true` and
/// the movement would then fail the contract's 32-byte cap. Capping the CATALOG makes a
/// rejectable row impossible to store, which closes it for the durable grant, for the
/// wire path (a 400 on a currency the catalog legitimately holds) and for the admin
/// page at once — instead of teaching one handler to re-enumerate the validator's
/// branches. It is `octet_length`, not `char_length`, because `MAX_CURRENCY_CODE_BYTES`
/// is a `str::len()` BYTE count: a 20-character multibyte code is 40 octets and must be
/// rejected here too, or the by-construction argument leaks.
///
/// **The ledger is ordered by `seq`, not `at` — and `seq` is stamped under the balance
/// row lock, not by the `bigserial` default.** `now()` is `transaction_timestamp()`,
/// evaluated at transaction start, so `at` cannot order money. But a plain `bigserial`
/// does not fix it either: it is assigned during the ledger INSERT, a full round trip
/// BEFORE the balance lock, so two concurrent credits can order as (`seq=1`, balance 200),
/// (`seq=2`, balance 100) — the running balance going DOWN on a credit, which is exactly
/// the defect this column exists to prevent. The real ordering value is drawn in
/// `Store::set_balance_after_tx`, the statement that runs while the lock is held; the
/// default here is a placeholder, and `at` stays descriptive.
///
/// KNOWN GAP (recorded, not fixed here): `wallet.ledger` is append-only and UNBOUNDED —
/// there is no retention sweep. Reads stay fast (the `(player_id, seq DESC)` index bounds
/// them), so the cost is disk and backup rather than latency, and "keep money history
/// forever" may well be the right permanent answer. If it is not, the follow-up is a
/// `scheduler.fired{wallet-prune}` subscription with a retention knob — never a silent
/// truncation of the authority for money.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS wallet;

CREATE TABLE IF NOT EXISTS wallet.currencies (
	code         text PRIMARY KEY
	             CONSTRAINT currencies_code_len_check
	             CHECK (octet_length(code) <= 32),
	display_name text        NOT NULL,
	kind         text        NOT NULL DEFAULT 'soft',
	decimals     int         NOT NULL DEFAULT 0,
	created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS wallet.balances (
	player_id  uuid NOT NULL,
	currency   text NOT NULL
	           CONSTRAINT balances_currency_fkey REFERENCES wallet.currencies(code),
	amount     bigint NOT NULL DEFAULT 0
	           CONSTRAINT balances_amount_check
	           CHECK (amount >= 0 AND amount <= 1000000000000000),
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (player_id, currency)
);

CREATE TABLE IF NOT EXISTS wallet.ledger (
	seq             bigserial   NOT NULL,
	id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	idempotency_key text        NOT NULL,
	player_id       uuid        NOT NULL,
	currency        text        NOT NULL,
	delta           bigint      NOT NULL,
	balance_after   bigint      NOT NULL,
	reason          text        NOT NULL,
	at              timestamptz NOT NULL DEFAULT clock_timestamp(),
	UNIQUE (idempotency_key)
);
CREATE INDEX IF NOT EXISTS ledger_player_seq_idx ON wallet.ledger(player_id, seq DESC);"#;

/// The dev currency catalog, seeded only under `WALLET_DEV_SEED`: `(code, display_name,
/// kind, decimals)`.
const DEV_SEED_CURRENCIES: &[(&str, &str, &str, i32)] = &[
    ("gold", "Gold", "soft", 0),
    ("gems", "Gems", "hard", 0),
];

/// Folds any lower-level error into an `Internal` operation error.
pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

/// `true` only when `WALLET_DEV_SEED` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` — a seeded catalog is a dev artifact, so this
/// follows the repo's explicit-only, fail-closed convention (`APIKEYS_DEV_SEED`), NOT a
/// module-convenience default-ON.
fn dev_seed_explicitly_on() -> bool {
    matches!(
        std::env::var("WALLET_DEV_SEED"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The wallet module. Edge exposure is topology-blind: `init` contributes the generated
/// RPC faces to `edge::EDGE_SLOT` unconditionally, and `app::run` installs them iff this
/// process serves an internal QUIC edge — the module never knows.
pub struct WalletModule {
    svc: OnceLock<Arc<Service>>,
}

impl Default for WalletModule {
    fn default() -> Self {
        WalletModule::new()
    }
}

impl WalletModule {
    pub fn new() -> WalletModule {
        WalletModule {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("wallet.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for WalletModule {
    fn name(&self) -> &str {
        "wallet"
    }

    /// `config` is a real sync dependency: the starter grant reads `wallet/starter_currency`
    /// + `wallet/starter_amount` through the injected reader. A process hosting wallet
    /// without the config capability FAILS STARTUP (`app::validate_requires`). Process
    /// infrastructure (the DB, the event plane, metrics, HTTP) is never declared.
    fn requires(&self) -> Vec<String> {
        vec!["config".into()]
    }

    /// Phase 1, no I/O: builds the store-backed service (from `ctx.db()` + `ctx.bus()` —
    /// the bus handle is required, the movement authority appends `wallet.changed`
    /// through it) and offers the ONE service under BOTH capability keys, so a
    /// dependent's `require` resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("wallet requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(
            pool,
            ctx.bus().clone(),
            dev_seed_explicitly_on(),
        ));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("wallet.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Wallet>(key("wallet", "wallet"), svc.clone());
        ctx.registry()
            .provide::<dyn Player>(key("wallet", "player"), svc);
        Ok(())
    }

    /// Creates this module's own schema (idempotent) and, only when `WALLET_DEV_SEED` is
    /// EXPLICITLY truthy, upserts the dev currency catalog (self-healing).
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("wallet requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;

        let svc = self.svc();
        if svc.dev_seed {
            tracing::warn!(
                "WALLET_DEV_SEED is ON — upserting the dev currency catalog (`gold`, `gems`). \
                 This is an explicit local-dev opt-in; keep it OFF (the fail-closed default) \
                 in production, where the catalog is operator data."
            );
            let mut conn = pool.acquire().await?;
            for (code, display_name, kind, decimals) in DEV_SEED_CURRENCIES {
                svc.store
                    .upsert_currency_tx(&mut conn, code, display_name, kind, *decimals)
                    .await?;
            }
        }
        Ok(())
    }

    /// Only wires up — no I/O (#8).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // The mandatory `config` reader, resolved HERE (phase 2) and never in `start` or
        // lazily: `requirecheck` observes `register` + `init` only, so a later resolution
        // would make the declared `requires(["config"])` unverifiable. In the split a
        // `remote::Stub` swaps a `CachedConfig` under the SAME key — topology-blind.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = svc.config.set(cfg);

        // Player operations: the generated `operations()` yields one OpSet per #[http]
        // method; contribute each half to its slot so the gateway fronts GET /wallet/me
        // and GET /wallet/currencies, authenticates once, and dispatches with the
        // verified player_id in identity.
        for op in walletapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run` applies
        // it iff this process serves an internal edge (then a peer reads balances or
        // moves money over QUIC); in the monolith it is never applied. Own glue
        // (sanctioned): the generated register_server faces live in `walletrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                walletrpc::wallet_rpc::register_server(server, svc.clone());
                walletrpc::player_rpc::register_server(server, svc);
            }),
        );

        // Routing-as-data SERVE side: this module's `#[http]` op manifest as pure DATA,
        // contributed UNCONDITIONALLY. `player_rpc` carries the two player reads;
        // `wallet_rpc` is wire-only (its `describe()` is empty) — concatenated so a
        // future `#[http]` op on either contract flows through with no edit here.
        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            opsapi::DescribeManifest::concat([
                walletrpc::player_rpc::describe(),
                walletrpc::wallet_rpc::describe(),
            ]),
        );
        Ok(())
    }
}

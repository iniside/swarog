//! `wallet` — owns virtual currency: the currency catalog, per-player balances and the
//! append-only ledger that is the authority for every movement.
//!
//! [`Service::apply_on`] is the single movement authority: it runs a whole movement on a
//! CALLER-OWNED connection and never touches transaction control, so a pool-owned
//! transaction and the event plane's handed delivery transaction share one implementation
//! instead of growing two ways to move money. The domain write and its durable
//! `wallet.changed` append commit together on that one connection — the event is durable
//! iff the ledger row is.

pub mod conformance;

mod admin;
mod projection;
mod service;
mod store;

#[allow(unused_imports)] // re-exported at crate root: `conformance` and `service` reach these via `crate::`
use service::*;
use store::*;

pub use service::Service;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use configapi::Config;
use lifecycle::{Context, Module};
use registry::key;
use walletapi::{Player, Wallet};

/// `balances_amount_check`'s lower bound makes "insufficient funds" a DB verdict rather
/// than a read-then-write race — the service never pre-checks a balance; its upper bound
/// (10^15, ~9000x below `i64::MAX` once a movement is capped at
/// `walletapi::MAX_MOVEMENT_AMOUNT`) keeps a `bigint` overflow — SQLSTATE 22003, which
/// nothing maps — unreachable, so a credit past the ceiling is the mapped 23514 instead.
///
/// The error mapping in `store.rs` is COUPLED TO THESE CONSTRAINT NAMES, and `CREATE
/// TABLE IF NOT EXISTS` never repairs a pre-existing table: a `wallet.balances` carrying
/// differently-named constraints turns every insufficient-funds into a 500. Renaming one
/// is a `DROP SCHEMA wallet CASCADE` + fresh boot, never an `ALTER`.
///
/// `currencies_code_len_check` is `octet_length`, not `char_length`, because
/// `MAX_CURRENCY_CODE_BYTES` is a `str::len()` BYTE count — a 20-character multibyte code
/// is 40 octets. Capping the CATALOG is what keeps a currency the catalog holds from
/// failing the contract's cap inside the movement authority.
///
/// The ledger orders by `seq`, not `at`: `now()` is `transaction_timestamp()`, fixed at
/// transaction start. The `bigserial` default is a placeholder — the real ordering value
/// is drawn in `Store::set_balance_after_tx`, under the balance row lock.
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

/// `(code, display_name, kind, decimals)`.
const DEV_SEED_CURRENCIES: &[(&str, &str, &str, i32)] = &[
    ("gold", "Gold", "soft", 0),
    ("gems", "Gems", "hard", 0),
];

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

/// Unset is `false`: a seeded catalog is a dev artifact, so this is explicit-only and
/// fail-closed (the `APIKEYS_DEV_SEED` convention), never a default-ON convenience.
fn dev_seed_explicitly_on() -> bool {
    matches!(
        std::env::var("WALLET_DEV_SEED"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

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

    /// `config` is wallet's one domain dependency (the starter grant's currency/amount
    /// knobs); process infrastructure — the DB, the event plane, metrics, HTTP — is never
    /// declared.
    fn requires(&self) -> Vec<String> {
        vec!["config".into()]
    }

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

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("wallet requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;

        let svc = self.svc();
        if svc.dev_seed {
            tracing::warn!(
                "WALLET_DEV_SEED is ON — seeding the dev currency catalog (`gold`, `gems`) if \
                 absent; an existing row keeps its operator-edited fields. This is an explicit \
                 local-dev opt-in; keep it OFF (the fail-closed default) in production, where \
                 the catalog is operator data."
            );
            let mut conn = pool.acquire().await?;
            for (code, display_name, kind, decimals) in DEV_SEED_CURRENCIES {
                svc.store
                    .write_currency_tx(
                        &mut conn,
                        code,
                        display_name,
                        kind,
                        *decimals,
                        OnConflict::Skip,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Resolved HERE and never in `start` or lazily: `requirecheck` observes `register`
        // + `init` only, so a later resolution makes `requires(["config"])` unverifiable.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = svc.config.set(cfg);

        // The optional starter grant, on the HANDED delivery conn so the credit, its ledger
        // row, the `wallet.changed` append and the checkpoint commit as one unit in BOTH
        // topologies. `AfterRegistration`, not `Genesis`: `player.registered` retains 7 days,
        // so Genesis would promise a retroactive grant the log cannot deliver — and the id +
        // start position are an IMMUTABLE contract (`spec_hash`), so changing either later is
        // a new subscription id, never an edit.
        let granter = svc.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: "wallet.player-registered.v1",
                start: bus::StartPosition::AfterRegistration,
            },
            &accountsevents::PLAYER_REGISTERED,
            move |mut delivery, e: accountsevents::PlayerRegistered| {
                let granter = granter.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    granter.grant_starter(conn, &e.player_id).await
                })
            },
        );

        for op in walletapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // The local "Wallet" page. The `RenderFn` is synchronous; `admin::admin_render`
        // bridges to the async store reads via `block_in_place`. The extension entries ride
        // the item as pure data — the same vec `admin_data` returns REMOTE.
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| admin::admin_render(&render_svc, params)),
            )
            .with_extensions(admin::extension_entries()),
        );

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge; in the monolith it is never applied.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                walletrpc::wallet_rpc::register_server(server, svc.clone());
                walletrpc::player_rpc::register_server(server, svc.clone());
                // The admin fan-out READ face and, ALONGSIDE it, the opt-in WRITE face —
                // both through this module's OWN glue crate's re-exports. The write face is
                // what makes the Wallet page editable from a REMOTE admin process.
                walletrpc::register_admin(server, svc.clone());
                walletrpc::register_admin_submit(server, svc);
            }),
        );

        // `wallet_rpc` is wire-only, so its `describe()` is empty; concatenated anyway so
        // a future `#[http]` op on either contract flows through with no edit here.
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

#[cfg(test)]
mod tests;

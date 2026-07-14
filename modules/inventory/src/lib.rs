//! `inventory` — owns item holdings for any owner (a player, e.g. IAP, or a
//! character). It depends on accounts-supplied identity (the gateway injects it) and
//! on `characters` for ownership checks, and REACTS to character lifecycle events:
//! granting a starter item on creation and wiping holdings on deletion — integrity
//! WITHOUT a cross-module foreign key. `characters` has no idea inventory exists.
//! Port of Go's `modules/inventory`.
//!
//! The polymorphic [`Owner`] (player | character) lives entirely inside this module;
//! `owner_id` is referenced by id with NO cross-module FK (logical isolation #10).
//! The in-module FK from `holdings.item_id` to `items.id` DOES exist. The player
//! operations (`list_mine`/`list_character`/`grant`) are exposed as `opsapi`
//! Operations: the gateway fronts the HTTP routes, authenticates ONCE, and dispatches
//! to the service with the verified caller identity threaded in — never a
//! client-supplied one. An impl crate: no other module imports it.

mod admin;
mod owner;
mod projection;
mod service;
mod store;

use admin::*;
use owner::*;
#[allow(unused_imports)] // re-exported so tests.rs's `use super::*;` sees STARTER_ITEM/STARTER_QTY/lock_key/…
use projection::*;
use service::*;
use store::*;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use charactersapi::Ownership;
use configapi::Config;
use inventoryapi::holdings_rpc;
#[allow(unused_imports)] // Holding: re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use inventoryapi::{Holding, Holdings};
use lifecycle::{Context, Module};
use opsapi::Error;
#[allow(unused_imports)] // re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use opsapi::Identity;
use registry::key;
#[allow(unused_imports)] // re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use sqlx::{PgConnection, PgPool};

/// The admin surface ids (Section groups it in the sidebar; Label is the entry).
const ADMIN_ITEM_ID: &str = "inventory";
const ADMIN_SECTION: &str = "Game Content";
const ADMIN_LABEL: &str = "Inventory";

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. `holdings.owner_id` is a plain `uuid` ref to a player/character with
/// NO cross-module FK; `holdings.item_id` DOES carry the in-module FK to `items(id)`.
/// `quantity` is `bigint` (matches the i64 the config knob + HTTP grant op carry —
/// an `int4` column overflowed to SQLSTATE 22003 inside the delivery tx and
/// poison-paused `inventory.character-created.v1`) and CHECK-bounded to
/// `0..=2_000_000`: the DB CHECK is the authority (it covers a raw `psql` writer too,
/// same doctrine as scheduler's DB-side guards), and its ceiling sits at 2x the
/// app-level `MAX_HOLDING_QTY` single-grant cap because the CHECK bounds accumulated
/// STATE (the `ON CONFLICT` sum) while the policy bounds one grant.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS inventory;

CREATE TABLE IF NOT EXISTS inventory.items (
	id   text PRIMARY KEY,
	name text NOT NULL,
	kind text NOT NULL
);
INSERT INTO inventory.items (id, name, kind) VALUES
	('coin','Coin','currency'),
	('starter_sword','Starter Sword','weapon'),
	('health_potion','Health Potion','consumable')
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS inventory.holdings (
	owner_type text NOT NULL,                 -- 'player' | 'character'
	owner_id   uuid NOT NULL,                 -- ref player/character id, no cross-module FK
	item_id    text NOT NULL REFERENCES inventory.items(id),
	quantity   bigint NOT NULL CHECK (quantity >= 0 AND quantity <= 2000000),
	PRIMARY KEY (owner_type, owner_id, item_id)
);
CREATE INDEX IF NOT EXISTS holdings_owner_idx ON inventory.holdings(owner_type, owner_id);

-- Tombstones for wiped characters: character.created and character.deleted ride
-- INDEPENDENT durable subscriptions, and the plane's ordering contract is
-- per-subscription only (asyncevents README: "ordering is per-subscription in
-- XID-allocation order") -- so a wipe can be delivered BEFORE the grant. The wipe
-- handler plants a tombstone in its delivery tx; the grant handler skips tombstoned
-- ids. Sound because character ids are UUIDs and never recur.
CREATE TABLE IF NOT EXISTS inventory.wiped_characters (
	character_id uuid PRIMARY KEY,
	wiped_at     timestamptz NOT NULL DEFAULT now()
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// Env boolean mirroring Go's `envBool` (`"1"`/`"true"`/`"on"`, case-insensitive).
fn env_bool(key: &str, def: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => {
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        }
        _ => def,
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The inventory module. Holds the constructed shared state. Edge exposure is
/// topology-blind: `init` contributes the generated RPC face to `edge::EDGE_SLOT`
/// unconditionally, and `app::run` installs it iff this process serves an internal
/// QUIC edge — the module never knows.
pub struct Inventory {
    inner: OnceLock<Arc<Inner>>,
}

impl Default for Inventory {
    fn default() -> Self {
        Inventory::new()
    }
}

impl Inventory {
    pub fn new() -> Inventory {
        Inventory { inner: OnceLock::new() }
    }

    fn inner(&self) -> Arc<Inner> {
        self.inner
            .get()
            .expect("inventory.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for Inventory {
    fn name(&self) -> &str {
        "inventory"
    }

    fn requires(&self) -> Vec<String> {
        vec!["characters".into(), "config".into()]
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed shared state (from
    /// `ctx.db()`) and offers it under `inventory.holdings`, so a dependent's
    /// `require` resolves regardless of registration order. It touches only the pool;
    /// the ownership + config deps are injected later in `init` (phase 2), matching Go
    /// where `m.svc.characters`/`m.cfg` are set in Init.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("inventory requires a DB pool"))?
            .clone();
        let inner = Arc::new(Inner {
            store: Store { pool },
            // Resolve the dev-grant gate once, here, so it is the single source of
            // truth for every exposure path (the op is contributed unconditionally;
            // only this impl-side guard decides).
            dev_grant: env_bool("INVENTORY_DEV_GRANT", false),
            ownership: OnceLock::new(),
            cfg: OnceLock::new(),
        });
        self.inner
            .set(inner.clone())
            .map_err(|_| anyhow::anyhow!("inventory.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Holdings>(key("inventory", "holdings"), inner);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("inventory requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). EXACT order mirrors Go's `Init`, minus the config
    /// inventory-owned config cache Step 8 removed:
    ///   1. resolve `characters` ownership → inject into the shared state,
    ///   2/3. the two DURABLE `on_tx` subscriptions (grant-starter/wipe, on the HANDED
    ///        conn so the effect is atomic with the checkpoint commit in the delivery tx),
    ///   4. resolve `config` (HARD — fail-loud, this is why config is in `requires`);
    ///      `grant_starter` reads it directly, no second cache/subscription needed,
    ///   5/6. contribute the player operations (ALL unconditional; grant is gated at
    ///        the impl) + the local admin item,
    ///   and the generated RPC face to the edge slot (applied by `app::run` iff this
    ///   process serves an internal edge).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let inner = self.inner();

        // 1. The ownership capability backs list_character's authz; the registry
        // resolves it to the real service (monolith) or the generated edge client
        // (split). Inject it into the state Provided in register.
        let ownership = ctx.registry().require::<dyn Ownership>(&key("characters", "ownership"));
        let _ = inner.ownership.set(ownership);

        // 2/3. React to character lifecycle. Two INDEPENDENT durable subscriptions,
        // and the plane's contract is per-subscription ordering only ("ordering is
        // per-subscription in XID-allocation order" — asyncevents README): a
        // character's `deleted` can be delivered before its `created`. Integrity is
        // therefore a consumer-side tombstone, not a cross-module FK: the wipe
        // handler plants `inventory.wiped_characters` in its delivery tx and the
        // grant handler skips tombstoned ids (UUIDs never recur, so the tombstone is
        // permanent truth); a per-character advisory xact-lock serializes concurrent
        // deliveries. Each effect runs on the HANDED conn so it commits atomically
        // with the subscription checkpoint in BOTH topologies.
        let granter = inner.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: "inventory.character-created.v1",
                start: bus::StartPosition::Genesis,
            },
            &charactersevents::CREATED,
            move |mut delivery, e: charactersevents::Created| {
                let granter = granter.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    granter.grant_starter(conn, &e.character_id).await
                })
            },
        );
        let wiper = inner.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: "inventory.character-deleted.v1",
                start: bus::StartPosition::Genesis,
            },
            &charactersevents::DELETED,
            move |mut delivery, e: charactersevents::Deleted| {
                let wiper = wiper.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    wiper.wipe_character(conn, &e.character_id).await
                })
            },
        );

        // 4. HARD dependency on config (declared in requires()): every composition
        // hosting inventory supplies the capability locally or through a remote stub,
        // so this fails loud at boot rather than silently degrading to the starter
        // consts. No inventory-owned second cache/subscription to keep fresh (Step 8):
        // `grant_starter` reads this reader directly on every grant, and the reader is
        // itself kept fresh by the app-owned broadcast invalidation plane — so editing
        // inventory/starter_item flows
        // config_changed -> the reader's own refresh -> the next grant sees it.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = inner.cfg.set(cfg);

        // 5. Player operations: contribute each generated op (route + HTTP↔wire binding
        // + in-process invoker) so the gateway fronts GET /inventory/me, GET
        // /inventory/character/{id} AND POST /inventory/me/grant, authenticates once,
        // and dispatches with the verified player_id in identity. ALL ops are
        // contributed UNCONDITIONALLY — the dev-grant gate lives in the impl
        // (`Holdings::grant` answers NotFound when `INVENTORY_DEV_GRANT` is off), so
        // the monolith slot set and the split route set are structurally equal and
        // the gate cannot be bypassed by any topology's transport.
        if inner.dev_grant {
            tracing::warn!(
                "INVENTORY_DEV_GRANT is ON — POST /inventory/me/grant (simulated IAP) is enabled; \
                 this is an explicit local-dev opt-in, keep it OFF (the fail-closed default) in production"
            );
        }
        for op in holdings_rpc::operations(inner.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // 6. The local admin page (owners list + ?owner= drill-down). The RenderFn is
        // synchronous but the store reads are async; no admin PORTAL renders this in
        // M1, so the closure bridges via block_in_place (needs the multi-thread runtime
        // the app boots on).
        let render_inner = inner.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                ADMIN_ITEM_ID,
                ADMIN_SECTION,
                ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| {
                    let render_inner = render_inner.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(admin_content(&render_inner.store, params))
                    })
                }),
            )
            // The cross-page contributions (Players menu + Characters card menu +
            // character-modal footer); the SAME vec `admin_data` sends over the wire.
            .with_extensions(admin::extension_entries()),
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // front gateway in a peer routes `inventory.*` here over QUIC); in the
        // monolith it is never applied. Own glue (sanctioned): the edge-facing
        // register_server lives in the `inventoryrpc` glue crate.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                // The admin fan-out face (`admin.adminData`), via this module's OWN
                // glue crate's re-export (no foreign rpc import).
                inventoryrpc::register_admin(server, inner.clone());
                inventoryrpc::holdings_rpc::register_server(server, inner);
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. The authz-mapping test uses a FAKE Ownership (no characters module). The DB
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Inner`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests;

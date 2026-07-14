//! `characters` — owns player characters (a player has N characters). It emits
//! lifecycle events (`character.created` / `character.deleted`) other modules react
//! to, and never knows who. Its player-facing operations (create/list/delete a
//! player's own characters) are exposed as `opsapi` Operations: the gateway fronts
//! the HTTP routes, authenticates ONCE, and dispatches to the service with the
//! verified caller identity threaded in. The service never reads a client-supplied
//! identity — the trust boundary lives at the gateway/edge seam. Port of Go's
//! `modules/characters`.
//!
//! The core pattern (copied by every later module): the domain write and its durable
//! event append commit in ONE transaction, via `bus::emit_tx` on the same `&mut *tx` — the
//! event is durable iff the character is. An impl crate: no other module imports it.

pub mod conformance;

mod admin;
mod service;
mod store;

use admin::*;
#[allow(unused_imports)] // re-exported so tests.rs's `use super::*;` sees Service/consts/name caps/…
use service::*;
use store::*;

/// Preserves `characters::Service` as public API — it was a top-level `pub struct`
/// before the split into `service.rs`.
pub use service::Service;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use charactersapi::{Ownership, Player};
use configapi::Config;
use lifecycle::{Context, Module};
use registry::key;
#[allow(unused_imports)] // re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use opsapi::Identity;
#[allow(unused_imports)] // re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use bus::Bus;
#[allow(unused_imports)] // re-exported for tests.rs's `use super::*;`, unused by lib.rs's own code
pub(crate) use sqlx::{PgConnection, PgPool};

/// The admin surface ids — shared by the contributed `Item` and the `Admin::admin_data`
/// reply so a (future) remote admin fetches the same Section/Label the local render carries.
const ADMIN_ITEM_ID: &str = "characters";
const ADMIN_SECTION: &str = "Game Content";
const ADMIN_LABEL: &str = "Characters";

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. Verbatim from Go's `schemaDDL`: `player_id` is a plain ref to
/// accounts.players with NO cross-module FK; the index keeps a player's list cheap.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS characters;
CREATE TABLE IF NOT EXISTS characters.characters (
	id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	player_id  uuid        NOT NULL,
	name       text        NOT NULL,
	class      text        NOT NULL DEFAULT 'novice',
	created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS characters_player_idx ON characters.characters(player_id);"#;

/// Folds any lower-level error into an `Internal` operation error.
pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The characters module. Holds the constructed service (shared between `register`,
/// the operations, and the admin render). Edge exposure is topology-blind: `init`
/// contributes the generated RPC faces to `edge::EDGE_SLOT` unconditionally, and
/// `app::run` installs them iff this process serves an internal QUIC edge — the
/// module never knows.
pub struct Characters {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Characters {
    fn default() -> Self {
        Characters::new()
    }
}

impl Characters {
    pub fn new() -> Characters {
        Characters {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("characters.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for Characters {
    fn name(&self) -> &str {
        "characters"
    }

    /// `config` is a hard sync dependency: `create`'s per-player cap reads
    /// `characters/max_per_player` on every call. A process hosting characters without
    /// the config capability FAILS STARTUP (`app::validate_requires`).
    fn requires(&self) -> Vec<String> {
        vec!["config".into()]
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service (from `ctx.db()` +
    /// `ctx.bus()`) and offers it under BOTH capability keys — `characters.ownership`
    /// (inventory resolves it) and `characters.player` (the gateway routes it) — so a
    /// dependent's `require` resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("characters requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service {
            store: Store { pool },
            bus: ctx.bus().clone(),
            config: OnceLock::new(),
        });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("characters.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Ownership>(registry::key("characters", "ownership"), svc.clone());
        ctx.registry()
            .provide::<dyn Player>(registry::key("characters", "player"), svc);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("characters requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Contributes (a) the three player operations into
    /// the opsapi slots so the gateway fronts POST/GET/DELETE /characters, (b) the
    /// local admin `Item`, and (c) the generated Ownership + Player RPC faces to the
    /// edge slot so a peer can reach `characters.*` over QUIC when this process
    /// serves an internal edge.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Resolve the mandatory `config` reader (phase 2 — provided in some module's
        // `register` in phase 1, so it is always present here). `create`'s cap gate
        // reads it directly; in the split a `remote::Stub` swaps a `CachedConfig` under
        // the SAME key, so this line is topology-blind.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = svc.config.set(cfg);

        // (a) Player operations: the generated `operations()` yields one OpSet per
        // #[http] method; contribute each half to its slot (LocalBackend + the future
        // RemoteBackend consume the SAME wire envelopes).
        for op in charactersapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // (b) The local admin page. RenderFn is synchronous, but the store reads are
        // async; no admin PORTAL renders this in M1, so the closure bridges via
        // block_in_place (requires the multi-thread runtime the app boots on).
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                ADMIN_ITEM_ID,
                ADMIN_SECTION,
                ADMIN_LABEL,
                Arc::new(move |_params: &adminapi::Params| {
                    let svc = render_svc.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(admin_content(&svc.store))
                    })
                }),
            ),
        );

        // (c) Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer resolves ownership / fronts the player ops over QUIC); in the monolith
        // it is never applied. Own glue (sanctioned): the generated register_server
        // faces live in `charactersrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                charactersrpc::ownership_rpc::register_server(server, svc.clone());
                // The admin fan-out face (`admin.adminData`), registered through this
                // module's OWN glue crate's re-export so no foreign rpc is imported.
                charactersrpc::register_admin(server, svc.clone());
                charactersrpc::player_rpc::register_server(server, svc);
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. Unit tests need no DB (validation runs before any DB work); integration
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Service`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests;

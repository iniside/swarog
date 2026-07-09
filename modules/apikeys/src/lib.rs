//! `apikeys` — API-key policy: the client-class credential store the gateway consults
//! on every op-dispatched request. It owns schema `apikeys` and holds one row per key
//! (`name`, plaintext `key`, `policy`, timestamps), where a policy is either the
//! literal `full` (every method) or a comma-separated wire-method allow-list. A
//! session bearer authorizes the *player*; a key authorizes the *client class* — the
//! two are orthogonal and both required where the op demands a player.
//!
//! Capability (topology-blind — the module never knows the process layout):
//!   - `apikeys.keys` ([`apikeysapi::Keys`]) — key string → [`apikeysapi::KeyRecord`],
//!     the capability the gateway's key verifier resolves (registry swap: local here,
//!     an edge client from `apikeysrpc::remote_factories()` in a split peer). Returns
//!     `Ok(None)` for an unknown OR revoked key; an `Err` is store/peer trouble.
//!
//! Dev seed (`APIKEYS_DEV_SEED`, explicitly truthy only — a well-known `full` key is a
//! trust artifact, so it follows the gateway's explicit-only convention, NOT the
//! module-convenience default-ON one): `migrate` upserts two well-known keys so the
//! local harness has a working client + trusted-server key. The upsert is self-healing
//! (`ON CONFLICT (name) DO UPDATE`), so a stray revoke on a shared dev DB can't
//! permanently poison the harness.

mod admin;
mod store;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Caps, Context, Module};
use opsapi::Error;

use crate::store::Store;

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. Keys are stored in plaintext (same trust model as
/// `accounts.sessions.token`), so `key` is a plain unique column and lookup is an
/// equality match; a revoked key keeps its row (`revoked_at` set) for the audit trail.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS apikeys;

CREATE TABLE IF NOT EXISTS apikeys.keys (
	name       text PRIMARY KEY,
	key        text        UNIQUE NOT NULL,
	policy     text        NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	revoked_at timestamptz
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// The dev seed (Decision 7). `dev-client` carries exactly the player-facing wire
// methods (the strings match the `METHOD_*` consts the `#[rpc]` macro generates —
// `<prefix>.<lowerCamel(method)>`); `dev-server` is the trusted-server `full` key.
// `match.report` is deliberately ABSENT from `dev-client` — it is the trusted-server
// op, which gives the harness a real negative case.
const DEV_CLIENT_POLICY: &str = "accounts.register,accounts.login,accounts.loginEpic,accounts.me,\
characters.create,characters.list,characters.delete,\
inventory.grant,inventory.listMine,inventory.listCharacter,\
leaderboard.topScores";

/// The well-known dev keys: `(name, key, policy)`. Seeded only when `APIKEYS_DEV_SEED`
/// is explicitly truthy.
const DEV_SEED: &[(&str, &str, &str)] = &[
    ("dev-client", "dev-key-client", DEV_CLIENT_POLICY),
    ("dev-server", "dev-key-server", "full"),
];

// ============================================================================
// Service — backs the `apikeys.keys` capability (the registry capability + the
// generated edge face).
// ============================================================================

/// What the gateway gets from `require::<dyn Keys>`. Holds only the store; a lookup is
/// a single indexed read.
pub struct Service {
    store: Store,
}

#[async_trait]
impl apikeysapi::Keys for Service {
    /// Resolves a key to its record. `Ok(None)` is a genuine unknown/revoked key (the
    /// gateway maps it to 401); an `Err` is store trouble the gateway logs and collapses
    /// to a per-request deny, never a cached one.
    async fn lookup_key(&self, key: String) -> Result<Option<apikeysapi::KeyRecord>, Error> {
        self.store.lookup(&key).await.map_err(internal)
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The apikeys module. Holds the constructed service (shared between `register` and the
/// edge face). Edge exposure is topology-blind: `init` contributes the generated Keys
/// face to `edge::EDGE_SLOT` unconditionally, and `app::run` installs it iff this
/// process serves an internal QUIC edge — the module never knows.
pub struct ApiKeys {
    svc: OnceLock<Arc<Service>>,
}

impl Default for ApiKeys {
    fn default() -> Self {
        ApiKeys::new()
    }
}

impl ApiKeys {
    pub fn new() -> ApiKeys {
        ApiKeys {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("apikeys.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for ApiKeys {
    fn name(&self) -> &str {
        "apikeys"
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service and offers it under
    /// `apikeys.keys` so the gateway's `require` resolves regardless of registration
    /// order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("apikeys requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service {
            store: Store { pool },
        });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("apikeys.register ran twice"))?;

        ctx.registry()
            .provide::<dyn apikeysapi::Keys>(registry::key("apikeys", "keys"), svc);
        Ok(())
    }

    /// Creates this module's own schema (idempotent) and, only when `APIKEYS_DEV_SEED`
    /// is EXPLICITLY truthy, upserts the well-known dev keys (self-healing).
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("apikeys requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;

        if dev_seed_explicitly_on() {
            tracing::warn!(
                "APIKEYS_DEV_SEED is ON — upserting well-known dev keys `dev-key-client` \
                 (player-facing policy) and `dev-key-server` (full). These are dev trust \
                 artifacts — NEVER enable in production."
            );
            let store = &self.svc().store;
            for (name, key, policy) in DEV_SEED {
                store.upsert_seed(name, key, policy).await?;
            }
        }
        Ok(())
    }

    /// Only wires up — no I/O (#8). Contributes the local "API Keys" admin item and the
    /// generated Keys + admin RPC faces to the edge slot UNCONDITIONALLY (topology-blind:
    /// `app::run` installs the edge faces iff this process serves an internal QUIC edge).
    /// Own glue (rule 5): the generated `register_server`/`register_admin` faces live in
    /// `apikeysrpc`.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Local admin page. The `RenderFn` is synchronous; `admin::admin_render` bridges
        // to the async store read via `block_in_place` (requires the multi-thread rt).
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| admin::admin_render(&render_svc, params)),
            ),
        );

        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                apikeysrpc::keys_rpc::register_server(server, svc.clone());
                // The admin fan-out face (`admin.adminData`), via this module's OWN glue
                // crate's re-export (no foreign rpc import).
                apikeysrpc::register_admin(server, svc.clone());
            }),
        );
        Ok(())
    }
}

/// `true` only when `APIKEYS_DEV_SEED` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` — a well-known key is a trust artifact, so this
/// follows the gateway's explicit-only convention, NOT a module-convenience default-ON.
fn dev_seed_explicitly_on() -> bool {
    matches!(
        std::env::var("APIKEYS_DEV_SEED"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

// ============================================================================
// Tests. Unit tests need no DB; integration tests target the local Postgres (the test
// DB) and SKIP cleanly when it is unreachable. In-crate so they can drive the private
// `Service`/`Store` directly. Fixtures use `test-`-prefixed key names and clean up
// their own rows — the shared local Postgres must never have the harness's dev rows
// poisoned by a test.
// ============================================================================
#[cfg(test)]
mod admin_tests;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod tests;

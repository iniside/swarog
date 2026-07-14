//! `apikeys` — API-key policy: the client-class credential store the gateway consults
//! on every op-dispatched request. It owns schema `apikeys` and two NORMALIZED relations:
//! `apikeys.roles(name, policy, revision, …)` (a named, reusable policy — `full` or a
//! comma-separated wire-method allow-list) and `apikeys.keys(name, secret_hash, prefix,
//! role → roles.name, revision, …)` (one credential referencing exactly one role). A key
//! points AT a role; editing a role's policy immediately changes the effective policy of
//! every key that references it (the lookup JOINs `keys → roles`). A session bearer
//! authorizes the *player*; a key authorizes the *client class* — the two are orthogonal.
//!
//! Secrets are SERVER-GENERATED and stored ONLY as a SHA-256 digest (`secret_hash`, an
//! indexed O(1) lookup) plus a display `prefix`. The plaintext is surfaced exactly once
//! (the show-once reveal on create) and is NEVER re-derivable from a read — a lost create
//! response means revoke + recreate under a new name.
//!
//! Capability (topology-blind — the module never knows the process layout):
//!   - `apikeys.keys` ([`apikeysapi::Keys`]) — key string → [`apikeysapi::KeyRecord`]
//!     (`name` + the resolved ROLE policy), the capability the gateway's key verifier
//!     resolves (registry swap: local here, an edge client from
//!     `apikeysrpc::remote_factories()` in a split peer). Returns `Ok(None)` for an
//!     unknown OR revoked key; an `Err` is store/peer trouble.
//!
//! Remote admin write ([`adminapi::AdminSubmit`]): the "API Keys" configurator is
//! editable in BOTH topologies — its typed form drives role/key CRUD, and `admin_submit`
//! runs the SAME dispatch server-side (in apikeys-svc) when a remote admin process POSTs
//! over the mTLS edge (`admin.adminSubmit`), so the store closure never crosses the wire.
//!
//! Dev seed (`APIKEYS_DEV_SEED`, explicitly truthy only — a well-known `full` key is a
//! trust artifact, so it follows the gateway's explicit-only convention, NOT the
//! module-convenience default-ON one): `migrate` upserts two well-known roles and two
//! keys (KNOWN plaintext secrets, so `X-Api-Key: dev-key-server` still resolves) so the
//! local harness has a working client + trusted-server key. The upserts are self-healing
//! (`ON CONFLICT (name) DO UPDATE`), so a stray edit on a shared dev DB can't poison it.

mod admin;
pub mod conformance;
mod store;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use opsapi::Error;

use crate::store::{KeySummary, RoleSummary, Store, WriteError};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent DDL (`CREATE … IF NOT EXISTS`), fresh-boot only per the wipe policy: an
/// existing dev DB carrying the OLD flat `apikeys.keys` needs `DROP SCHEMA apikeys
/// CASCADE` (no data migration — the wipe strategy).
///
/// `roles` is created BEFORE `keys` so the `keys.role → roles.name` FK resolves. The FK
/// (NO ACTION default) is the authority protecting a key's role from deletion; the
/// effective policy is resolved by JOIN, never denormalized onto the key. `secret_hash`
/// is UNIQUE + implicitly indexed, so lookup is an O(1) equality on the digest.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS apikeys;

CREATE TABLE IF NOT EXISTS apikeys.roles (
	name       text PRIMARY KEY,
	policy     text        NOT NULL,
	revision   bigint      NOT NULL DEFAULT 1,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS apikeys.keys (
	name        text PRIMARY KEY,
	secret_hash text        NOT NULL UNIQUE,
	prefix      text        NOT NULL,
	role        text        NOT NULL REFERENCES apikeys.roles(name),
	revision    bigint      NOT NULL DEFAULT 1,
	created_at  timestamptz NOT NULL DEFAULT now(),
	updated_at  timestamptz NOT NULL DEFAULT now(),
	revoked_at  timestamptz
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// The dev seed (Decision 7). The `dev-client` ROLE carries exactly the player-facing
// wire methods (the strings match the `METHOD_*` consts the `#[rpc]` macro generates —
// `<prefix>.<lowerCamel(method)>`); the `dev-server` role is `full`. `match.report` is
// deliberately ABSENT from `dev-client` — it is the trusted-server op, which gives the
// harness a real negative case.
const DEV_CLIENT_POLICY: &str = "accounts.register,accounts.login,accounts.loginEpic,accounts.me,\
characters.create,characters.list,characters.delete,\
inventory.grant,inventory.listMine,inventory.listCharacter,\
leaderboard.topScores";

/// The well-known dev ROLES: `(name, policy)`. Seeded (FK order) BEFORE the keys.
const DEV_SEED_ROLES: &[(&str, &str)] = &[
    ("dev-client", DEV_CLIENT_POLICY),
    ("dev-server", "full"),
];

/// The well-known dev KEYS: `(name, secret, role)`. The `secret` is a KNOWN plaintext so
/// `X-Api-Key: dev-key-client`/`dev-key-server` resolves to `sha256(secret)`. Seeded only
/// when `APIKEYS_DEV_SEED` is explicitly truthy, AFTER the roles (FK).
const DEV_SEED_KEYS: &[(&str, &str, &str)] = &[
    ("dev-client", "dev-key-client", "dev-client"),
    ("dev-server", "dev-key-server", "dev-server"),
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
    /// to a per-request deny, never a cached one. The `policy` is the resolved ROLE
    /// policy (the lookup JOINs `keys → roles`).
    async fn lookup_key(&self, key: String) -> Result<Option<apikeysapi::KeyRecord>, Error> {
        self.store.lookup(&key).await.map_err(internal)
    }
}

// Async CRUD wrappers over the store — the write surface the admin configurator drives
// (both the LOCAL submit closure and the REMOTE `admin_submit`). Each preserves the
// store's [`WriteError`] classification (Conflict vs Invalid vs Db) so the admin seam can
// map a domain conflict to 409 and NEVER to NotFound (finding #2).
impl Service {
    pub(crate) async fn list_roles(&self) -> Result<Vec<RoleSummary>, sqlx::Error> {
        self.store.list_roles().await
    }

    pub(crate) async fn list_keys(&self) -> Result<Vec<KeySummary>, sqlx::Error> {
        self.store.list_keys().await
    }

    pub(crate) async fn create_role(&self, name: &str, policy: &str) -> Result<(), WriteError> {
        self.store.create_role(name, policy).await
    }

    pub(crate) async fn set_role_policy(
        &self,
        name: &str,
        expected_revision: i64,
        policy: &str,
    ) -> Result<(), WriteError> {
        self.store.set_role_policy(name, expected_revision, policy).await
    }

    pub(crate) async fn delete_role(&self, name: &str, expected_revision: i64) -> Result<(), WriteError> {
        self.store.delete_role(name, expected_revision).await
    }

    /// Mints a key under `role` and returns its show-once `secret` (the plaintext exists
    /// here and nowhere else after this call) plus its display `prefix`.
    pub(crate) async fn create_key(&self, name: &str, role: &str) -> Result<(String, String), WriteError> {
        self.store.create_key(name, role).await
    }

    pub(crate) async fn set_key_role(
        &self,
        name: &str,
        expected_revision: i64,
        role: &str,
    ) -> Result<(), WriteError> {
        self.store.set_key_role(name, expected_revision, role).await
    }

    pub(crate) async fn revoke_key(&self, name: &str, expected_revision: i64) -> Result<(), WriteError> {
        self.store.revoke_key(name, expected_revision).await
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
                "APIKEYS_DEV_SEED is ON — upserting well-known dev roles + keys \
                 `dev-key-client` (player-facing policy) and `dev-key-server` (full). These \
                 are dev trust artifacts — NEVER enable in production."
            );
            let store = &self.svc().store;
            // FK order: roles BEFORE the keys that reference them.
            for (name, policy) in DEV_SEED_ROLES {
                store.upsert_seed_role(name, policy).await?;
            }
            for (name, secret, role) in DEV_SEED_KEYS {
                store.upsert_seed_key(name, secret, role).await?;
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
                // The admin fan-out READ face (`admin.adminData`) and, ALONGSIDE it, the
                // opt-in WRITE face (`admin.adminSubmit`) — both via this module's OWN glue
                // crate's re-exports (no foreign rpc import). The write face makes the "API
                // Keys" configurator editable from a REMOTE admin process: admin-svc drives
                // the store CRUD server-side over the mTLS edge (the submit closure never
                // marshals across the wire).
                apikeysrpc::register_admin(server, svc.clone());
                apikeysrpc::register_admin_submit(server, svc.clone());
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

//! `accounts` — player identity (port of Go's `modules/accounts`). It owns schema
//! `accounts` and is a trusted VERIFIER of external identities: the production model
//! is federation (the EOS Connect shape) — the backend checks an IdP's signed token,
//! never holds a password, except for the dev/password provider gated behind
//! `ACCOUNTS_DEV_AUTH` (default ON locally, loud warning; turn OFF in prod). One
//! product-scoped `player_id`, many credential providers over it
//! (`identities(provider, subject) → player_id`), opaque DB-backed `sessions`
//! (30-day TTL, 32-byte base64url tokens).
//!
//! Capabilities (all topology-blind — the module never knows the process layout):
//!   - `accounts.sessions` ([`accountsapi::Sessions`]) — bearer → player_id, the
//!     capability the gateway's auth-once verifier resolves (registry swap: local
//!     here, an edge client from `accountsrpc::remote_factories()` in a split peer).
//!   - `accounts.auth` ([`accountsapi::Auth`]) — register/login/loginEpic/me,
//!     contributed as gateway operations (conditionally, per the env gates).
//!   - Epic web OAuth — two HTTP-NATIVE browser routes (`POST /accounts/epic/start`,
//!     `GET /accounts/epic/callback`) mounted on the shared router when
//!     `EPIC_CLIENT_SECRET` is configured.
//!
//! Durable-events rule (deliberate deviation from Go's plain `Emit`):
//! `player.registered` is emitted via `bus::emit_tx` INSIDE the registration store
//! transaction — the event is durable iff the player row is.

mod admin;
mod epic;
mod epic_oauth;
mod ops;
mod password;
mod store;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::Bus;
use lifecycle::{Caps, Context, Module};
use opsapi::{Error, Identity};

use crate::epic::{short_id, OidcVerifier};
use crate::password::{hash_password, verify_password};
use crate::store::{Player, Store, StoreError};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. Verbatim from Go's `schemaDDL`: the identities/sessions FKs are
/// INTERNAL to the accounts schema (allowed; the ban is on cross-module FKs).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS accounts;

CREATE TABLE IF NOT EXISTS accounts.players (
	id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	display_name text        NOT NULL,
	created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS accounts.identities (
	provider    text NOT NULL,
	subject     text NOT NULL,
	player_id   uuid NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	secret_hash text,                         -- only dev/password uses it
	created_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (provider, subject)
);
CREATE INDEX IF NOT EXISTS identities_player_idx ON accounts.identities(player_id);

CREATE TABLE IF NOT EXISTS accounts.sessions (
	token      text PRIMARY KEY,
	player_id  uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	created_at timestamptz NOT NULL DEFAULT now(),
	expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_player_idx ON accounts.sessions(player_id);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// ============================================================================
// Service — backs Sessions + Auth (the registry capabilities + the generated edge
// faces + the gateway's in-process invokers) and the local admin render.
// ============================================================================

/// What other modules get from `require::<dyn Sessions>` / `require::<dyn Auth>`.
/// Holds the store, the bus (for the atomic `player.registered` emit) and — once
/// `init` configures the epic provider — the OIDC verifier.
pub struct Service {
    pub(crate) store: Store,
    bus: Arc<Bus>,
    /// Set in `init` iff `EPIC_CLIENT_ID` is configured; `login_epic` is only
    /// contributed as an operation in that case, but a direct edge call when it is
    /// absent still answers a typed `Unavailable`.
    epic: OnceLock<Arc<OidcVerifier>>,
}

impl Service {
    /// Mints a fresh bearer token for `p` (Go's `issueSession`). A session-store
    /// failure propagates as `Internal` (→ 500).
    async fn issue_session(&self, p: &Player) -> Result<accountsapi::Session, Error> {
        let token = self.store.new_session(&p.id).await.map_err(internal)?;
        Ok(accountsapi::Session {
            player_id: p.id.clone(),
            token,
        })
    }

    /// Maps a verified external identity to a player, creating one on first sight
    /// (implicit registration, like EOS first-login) — the bool is `true` when a new
    /// player was provisioned, in which case `player.registered` was emitted
    /// DURABLY inside the same tx as the insert. A concurrent first-login race
    /// (unique violation) resolves to the winner's player.
    pub(crate) async fn find_or_create_external(
        &self,
        provider: &str,
        subject: &str,
        display_name: &str,
    ) -> Result<(Player, bool), Error> {
        if let Some(p) = self
            .store
            .player_by_identity(provider, subject)
            .await
            .map_err(internal)?
        {
            return Ok((p, false));
        }

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        match self
            .store
            .insert_player_with_identity_tx(&mut tx, provider, subject, display_name, None)
            .await
        {
            Ok(p) => {
                self.emit_registered_tx(&mut tx, &p, provider).await?;
                tx.commit().await.map_err(internal)?;
                Ok((p, true))
            }
            Err(StoreError::Taken) => {
                // Raced with a concurrent first-login: roll back our half-insert
                // explicitly (deterministic lock release) and adopt the winner's row.
                tx.rollback().await.map_err(internal)?;
                match self
                    .store
                    .player_by_identity(provider, subject)
                    .await
                    .map_err(internal)?
                {
                    Some(p) => Ok((p, false)),
                    None => Err(Error::internal("identity insert raced but no winner found")),
                }
            }
            Err(StoreError::Db(e)) => {
                tx.rollback().await.ok();
                Err(internal(e))
            }
        }
    }

    /// Writes the `player.registered` outbox row on the caller's tx — the durable
    /// rule: the event commits iff the registration does.
    async fn emit_registered_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        p: &Player,
        provider: &str,
    ) -> Result<(), Error> {
        self.bus
            .emit_tx(
                tx,
                &accountsevents::PLAYER_REGISTERED,
                &accountsevents::PlayerRegistered {
                    player_id: p.id.clone(),
                    display_name: p.display_name.clone(),
                    provider: provider.to_string(),
                },
            )
            .await
            .map_err(internal)
    }
}

#[async_trait]
impl accountsapi::Sessions for Service {
    /// Resolves a bearer token to its player. An unknown/expired token is
    /// `Ok(None)`; a store failure propagates as `Err` (Go's B2 fix) so a consumer
    /// can answer 503 on infrastructure failure rather than 401.
    async fn verify_session(&self, token: String) -> Result<Option<String>, Error> {
        Ok(self
            .store
            .player_by_session(&token)
            .await
            .map_err(internal)?
            .map(|p| p.id))
    }
}

#[async_trait]
impl accountsapi::Auth for Service {
    /// dev/password self-registration (AuthNone): creates a player + dev identity
    /// and the `player.registered` outbox row in ONE tx, then mints a session.
    /// Missing email/password → `Invalid` (400); a duplicate email → `Conflict`
    /// (409) — the same 400/409 Go returned, typed.
    async fn register(
        &self,
        email: String,
        password: String,
        display_name: String,
    ) -> Result<accountsapi::Session, Error> {
        if email.is_empty() || password.is_empty() {
            return Err(Error::invalid("email and password are required"));
        }
        let display = if display_name.is_empty() {
            email.clone()
        } else {
            display_name
        };

        let hash = hash_password(&password).map_err(internal)?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let p = match self
            .store
            .insert_player_with_identity_tx(&mut tx, "dev", &email, &display, Some(&hash))
            .await
        {
            Ok(p) => p,
            Err(StoreError::Taken) => {
                tx.rollback().await.map_err(internal)?;
                return Err(Error::conflict("email already registered"));
            }
            Err(StoreError::Db(e)) => {
                tx.rollback().await.ok();
                tracing::error!(err = %e, "register failed");
                return Err(internal(e));
            }
        };
        self.emit_registered_tx(&mut tx, &p, "dev").await?;
        tx.commit().await.map_err(internal)?;

        self.issue_session(&p).await
    }

    /// dev/password login (AuthNone). Bad credentials — an unknown email or a wrong
    /// password, deliberately indistinguishable — are `Unauthorized` (401).
    async fn login(&self, email: String, password: String) -> Result<accountsapi::Session, Error> {
        let Some((p, hash)) = self.store.password_identity(&email).await.map_err(|e| {
            tracing::error!(err = %e, "login failed");
            internal(e)
        })?
        else {
            return Err(Error::unauthorized("invalid credentials"));
        };
        if !verify_password(&hash, &password) {
            return Err(Error::unauthorized("invalid credentials"));
        }
        self.issue_session(&p).await
    }

    /// Epic (EOS Connect / OIDC) login (AuthNone): verifies the id_token and logs
    /// the player in, provisioning on first sight (which emits `player.registered`
    /// durably). Missing id_token → `Invalid` (400); a rejected token →
    /// `Unauthorized` (401).
    async fn login_epic(&self, id_token: String) -> Result<accountsapi::Session, Error> {
        if id_token.is_empty() {
            return Err(Error::invalid("id_token is required"));
        }
        let Some(epic) = self.epic.get() else {
            return Err(Error::unavailable("epic provider not configured"));
        };
        let subject = match epic.verify(&id_token).await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(%err, "epic token rejected");
                return Err(Error::unauthorized("invalid id_token"));
            }
        };
        let (p, _created) = self
            .find_or_create_external("epic", &subject, &format!("epic:{}", short_id(&subject)))
            .await?;
        self.issue_session(&p).await
    }

    /// The caller's own player + identities (player_id from `identity`, injected by
    /// the gateway after bearer verification — the AuthPlayer trust boundary; the
    /// service never reads a client-supplied identity). A missing identity is
    /// `Invalid` (→ 400); the gateway rejects an unauthenticated request with 401
    /// before `me` is ever called.
    async fn me(&self, identity: Identity) -> Result<accountsapi::MeView, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        let p = self
            .store
            .get_player(pid)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found("player not found"))?;
        let identities = self.store.identities_of(pid).await.map_err(internal)?;
        Ok(accountsapi::MeView {
            player_id: p.id,
            display_name: p.display_name,
            identities,
        })
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The accounts module. Holds the constructed service (shared between `register`,
/// the operations, the OAuth routes and the admin render). Edge exposure is
/// topology-blind: `init` contributes the generated Sessions + Auth faces to
/// `edge::EDGE_SLOT` unconditionally, and `app::run` installs them iff this process
/// serves an internal QUIC edge — the module never knows.
pub struct Accounts {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts::new()
    }
}

impl Accounts {
    pub fn new() -> Accounts {
        Accounts {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("accounts.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for Accounts {
    fn name(&self) -> &str {
        "accounts"
    }

    fn requires(&self) -> Vec<String> {
        // Registration publishes player.registered on the DURABLE plane (emit_tx),
        // so any process hosting accounts needs the messaging transport.
        vec!["messaging".to_string()]
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service and offers it
    /// under BOTH capability keys — `accounts.sessions` (the gateway's verifier
    /// resolves it) and `accounts.auth` — so a dependent's `require` resolves
    /// regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("accounts requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service {
            store: Store { pool },
            bus: ctx.bus().clone(),
            epic: OnceLock::new(),
        });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("accounts.register ran twice"))?;

        ctx.registry().provide::<dyn accountsapi::Sessions>(
            registry::key("accounts", "sessions"),
            svc.clone(),
        );
        ctx.registry()
            .provide::<dyn accountsapi::Auth>(registry::key("accounts", "auth"), svc);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("accounts requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Reads the env gates, configures the epic
    /// provider (JWKS fetch is LAZY, so construction is pure), mounts the OAuth
    /// browser routes, contributes the (gated) player operations, the local admin
    /// item, and the generated Sessions + Auth RPC faces to the edge slot.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // dev/password provider — local testing convenience, gated off for prod. The
        // register/login OPERATIONS are contributed below only when this gate is ON.
        let dev_auth = env_bool("ACCOUNTS_DEV_AUTH", true);
        if dev_auth {
            tracing::warn!(
                "ACCOUNTS_DEV_AUTH is ON — /accounts/register and /accounts/login are enabled; \
                 turn OFF in production"
            );
        }

        // epic provider — the real federated path via Epic Account Services (OIDC).
        // Enabled only when configured. Defaults point at EAS endpoints (web OAuth);
        // sub is the Epic Account ID.
        let mut epic_enabled = false;
        let client_id = std::env::var("EPIC_CLIENT_ID").unwrap_or_default();
        if !client_id.is_empty() {
            let jwks_url = env_or(
                "EPIC_JWKS_URL",
                "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json",
            );
            let issuer = env_or("EPIC_ISSUER_PREFIX", "https://api.epicgames.dev/epic/oauth/v1");
            match OidcVerifier::new(&jwks_url, &issuer, &client_id) {
                Err(err) => {
                    tracing::error!(%err, "epic provider disabled: verifier construction failed");
                }
                Ok(v) => {
                    let v = Arc::new(v);
                    svc.epic
                        .set(v.clone())
                        .map_err(|_| anyhow::anyhow!("accounts.init ran twice"))?;
                    epic_enabled = true;
                    tracing::info!(jwks = %jwks_url, aud = %client_id, "epic provider enabled");

                    // Web OAuth (authorize-code) needs the confidential client secret.
                    // These two routes are HTTP-NATIVE (a browser redirect flow with an
                    // external contract) — they are NOT operations; they mount on the
                    // shared router (Go's ctx.Mux ≙ ctx.mount).
                    let secret = std::env::var("EPIC_CLIENT_SECRET").unwrap_or_default();
                    if !secret.is_empty() {
                        let redirect = env_or(
                            "EPIC_REDIRECT_URI",
                            "http://localhost:8080/accounts/epic/callback",
                        );
                        let oauth = epic_oauth::EpicOAuth::new(
                            client_id.clone(),
                            secret,
                            redirect.clone(),
                            env_or("EPIC_AUTHORIZE_URL", "https://www.epicgames.com/id/authorize"),
                            env_or("EPIC_TOKEN_URL", "https://api.epicgames.dev/epic/oauth/v1/token"),
                            v,
                        )?;
                        ctx.mount(epic_oauth::router(Arc::new(oauth), svc.clone()));
                        tracing::info!(redirect = %redirect, "epic OAuth enabled");
                    }
                }
            }
        }

        // Player operations: the generated Auth OpSets, gated exactly as Go —
        // register/login under devAuth, loginEpic when the epic provider is up, me
        // always.
        ops::register_player_ops(ctx, svc.clone(), dev_auth, epic_enabled);

        // The local admin page (RenderFn is synchronous; the store reads are async —
        // bridge via block_in_place like characters, requires the multi-thread rt).
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |_params: &adminapi::Params| {
                    let svc = render_svc.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(admin::admin_content(&svc.store))
                    })
                }),
            ),
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer gateway verifies sessions / fronts the auth ops over QUIC); in the
        // monolith it is never applied. Own glue (rule 5): the generated
        // register_server faces live in `accountsrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                accountsrpc::sessions_rpc::register_server(server, svc.clone());
                // The admin fan-out face (`admin.adminData`), via this module's OWN
                // glue crate's re-export (no foreign rpc import).
                accountsrpc::register_admin(server, svc.clone());
                accountsrpc::auth_rpc::register_server(server, svc);
            }),
        );
        Ok(())
    }
}

/// Truthiness mirrors the repo's `envBool` (`"1"`/`"true"`/`"on"`, case-insensitive);
/// unset/empty returns `default`.
fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => {
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        }
        _ => default,
    }
}

fn env_or(key: &str, def: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => def.to_string(),
    }
}

// ============================================================================
// Tests. Unit tests (argon2, tokens, validation) need no DB; the OIDC/OAuth tests
// mint their own JWTs against a LOCAL JWKS fixture (no live Epic); integration
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Service`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests;

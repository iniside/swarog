//! `accounts` — player identity (port of Go's `modules/accounts`). It owns schema
//! `accounts` and is a trusted VERIFIER of external identities: the production model
//! is federation (the EOS Connect shape) — the backend checks an IdP's signed token,
//! never holds a password, except for the dev/password provider gated behind
//! `ACCOUNTS_DEV_AUTH` (explicit opt-in — default OFF/fail-closed, loud warn when set;
//! the run/split-proof scripts set `ACCOUNTS_DEV_AUTH=1` for local dev). One
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
use bus::{AnyTx, Bus, Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use opsapi::{Error, Identity};
use sqlx::PgConnection;

use tokio::sync::Semaphore;

use crate::epic::{short_id, OidcVerifier, VerifyError};
use crate::password::{hash_password, ArgonVerifier, PasswordVerifier, DUMMY_HASH};
use crate::store::{Player, Store, StoreError};

/// Input caps enforced before any argon2 work: RFC 5321's total-address maximum
/// for the email, 1 KiB for the password (argon2 cost scales with input length —
/// an unauthenticated caller must not choose it).
const MAX_EMAIL_BYTES: usize = 320;
const MAX_PASSWORD_BYTES: usize = 1024;

/// The fixed decoy candidate verified against [`DUMMY_HASH`] when the email is
/// unknown or the input invalid — never the caller's real password against a decoy.
const DECOY_CANDIDATE: &str = "accounts-invalid-credentials";

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
CREATE INDEX IF NOT EXISTS sessions_player_idx ON accounts.sessions(player_id);
CREATE INDEX IF NOT EXISTS sessions_expires_idx ON accounts.sessions(expires_at);"#;

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
    /// Whether the dev/password provider is enabled (`ACCOUNTS_DEV_AUTH`, resolved in
    /// `register`). Gates `register`/`login` at the SERVICE level — the SINGLE
    /// authority every exposure path traverses (the HTTP ops are contributed
    /// unconditionally in `ops::register_player_ops`, so the gateway route, the
    /// player-QUIC plane and the internal mTLS edge face all funnel into this one
    /// guard): with the gate off both methods answer NotFound, so a peer with a
    /// dev-CA cert cannot self-register/login when dev auth is off. `me` +
    /// `verify_session` are unaffected (needed by gateway/admin fan-out regardless).
    dev_auth: bool,
    /// Set in `init` iff `EPIC_CLIENT_ID` is configured. The `loginEpic` op is
    /// contributed unconditionally; when the provider is absent `login_epic` answers
    /// a typed `Unavailable` (→ 503) on every path, edge calls included.
    epic: OnceLock<Arc<OidcVerifier>>,
    /// RAM cap on concurrent argon2 hashes (64 MiB each): at most 2 run at once,
    /// on `spawn_blocking` threads — never on an async worker (admin's pattern).
    argon_permits: Arc<Semaphore>,
    /// Admission bound on concurrent login requests: beyond 32 in flight new logins
    /// are shed with `Unavailable` (503) instead of queueing without bound.
    login_slots: Arc<Semaphore>,
    /// The injectable verify seam — [`ArgonVerifier`] in production, fakes in tests.
    verifier: Arc<dyn PasswordVerifier>,
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

    /// Appends the `player.registered` durable event (`emit_tx`) on the caller's
    /// tx — the durable rule: the event commits iff the registration does.
    async fn emit_registered_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        p: &Player,
        provider: &str,
    ) -> Result<(), Error> {
        self.bus
            .emit_tx(
                AnyTx::new(&mut **tx),
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
    /// and appends the `player.registered` durable event in ONE tx, then mints a session.
    /// Missing email/password → `Invalid` (400); a duplicate email → `Conflict`
    /// (409) — the same 400/409 Go returned, typed.
    async fn register(
        &self,
        email: String,
        password: String,
        display_name: String,
    ) -> Result<accountsapi::Session, Error> {
        if !self.dev_auth {
            return Err(Error::not_found("registration is not enabled"));
        }
        if email.is_empty() || password.is_empty() {
            return Err(Error::invalid("email and password are required"));
        }
        if email.len() > MAX_EMAIL_BYTES || password.len() > MAX_PASSWORD_BYTES {
            return Err(Error::invalid("email or password too long"));
        }
        let display = if display_name.is_empty() {
            email.clone()
        } else {
            display_name
        };

        let Ok(argon) = self.argon_permits.clone().acquire_owned().await else {
            return Err(Error::internal("argon2 semaphore closed"));
        };
        // The 64 MiB hash runs on a blocking thread, never the async worker; the
        // permit MOVES INTO the closure — spawn_blocking is not cancelled when its
        // JoinHandle drops, so a permit held in this async frame would be released
        // on client disconnect while the detached hash keeps running (RAM-cap
        // defeat; admin 5844831 precedent).
        let pw = password;
        let hash = tokio::task::spawn_blocking(move || {
            let _permit = argon;
            hash_password(&pw)
        })
        .await
        .map_err(|e| Error::internal(format!("password hash task failed: {e}")))?
        .map_err(internal)?;

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

    /// dev/password login (AuthNone). Bad credentials — an unknown email, a wrong
    /// password, or over-cap/empty input, deliberately indistinguishable — are
    /// `Unauthorized` (401). Every admitted request performs exactly ONE argon2
    /// verify (the real hash or the [`DUMMY_HASH`] decoy) on a `spawn_blocking`
    /// thread behind the argon permit, so unknown-email and wrong-password are
    /// timing-indistinguishable and the 64 MiB hashes never run on an async worker.
    ///
    /// No per-IP limiter here on purpose: `Auth::login` is a pre-auth opsapi
    /// method — the gateway injects `Identity` only POST-auth, so no client IP ever
    /// reaches this service. Per-IP throttling, if wanted, lives at the gateway
    /// (which already rate-limits by source IP).
    async fn login(&self, email: String, password: String) -> Result<accountsapi::Session, Error> {
        // The dev-auth guard stays the VERY FIRST check (before any admission or
        // identity fetch): with the gate off the contributed op answers NotFound —
        // the single trust gate every exposure path traverses (Step 1's invariant).
        if !self.dev_auth {
            return Err(Error::not_found("password login is not enabled"));
        }
        // Admission bound: shed (never queue) beyond 32 concurrent logins. The slot
        // stays in this async frame — releasing it on cancel is correct, the request
        // is gone.
        let Ok(_slot) = self.login_slots.clone().try_acquire_owned() else {
            return Err(Error::unavailable("too many concurrent login attempts"));
        };
        let valid_input = !email.is_empty()
            && email.len() <= MAX_EMAIL_BYTES
            && !password.is_empty()
            && password.len() <= MAX_PASSWORD_BYTES;
        let identity = if valid_input {
            self.store.password_identity(&email).await.map_err(|e| {
                tracing::error!(err = %e, "login failed");
                internal(e)
            })?
        } else {
            None
        };
        let known_user = identity.is_some();
        // Real-or-decoy: an unknown email (or invalid input) verifies a FIXED decoy
        // candidate against the decoy hash — same argon2 cost, never a match, and
        // the caller's password is never run against a hash we didn't store for it.
        let (hash, candidate) = match &identity {
            Some((_, hash)) => (hash.clone(), password),
            None => (DUMMY_HASH.clone(), DECOY_CANDIDATE.to_string()),
        };
        let Ok(argon) = self.argon_permits.clone().acquire_owned().await else {
            return Err(Error::internal("argon2 semaphore closed"));
        };
        let verifier = self.verifier.clone();
        // The argon permit MUST live inside the blocking closure: spawn_blocking is
        // not cancelled when its JoinHandle drops, so a permit held in this async
        // frame would be released on client disconnect while the detached 64 MiB
        // hash keeps running — defeating the RAM cap (admin 5844831 precedent).
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = argon;
            verifier.verify(&hash, &candidate)
        })
        .await
        .map_err(|e| Error::internal(format!("password verifier task failed: {e}")))?;
        if !(verified && known_user && valid_input) {
            return Err(Error::unauthorized("invalid credentials"));
        }
        let (p, _) = identity.expect("known_user implies identity");
        self.issue_session(&p).await
    }

    /// Epic (EOS Connect / OIDC) login (AuthNone): verifies the id_token and logs
    /// the player in, provisioning on first sight (which emits `player.registered`
    /// durably). Missing id_token → `Invalid` (400); a rejected token →
    /// `Unauthorized` (401); a JWKS/IdP infrastructure failure → `Unavailable`
    /// (503) — the `verify_session` 503-not-401 precedent: an IdP outage must not
    /// read as bad credentials.
    async fn login_epic(&self, id_token: String) -> Result<accountsapi::Session, Error> {
        if id_token.is_empty() {
            return Err(Error::invalid("id_token is required"));
        }
        let Some(epic) = self.epic.get() else {
            return Err(Error::unavailable("epic provider not configured"));
        };
        let subject = match epic.verify(&id_token).await {
            Ok(s) => s,
            Err(VerifyError::Rejected(err)) => {
                tracing::warn!(%err, "epic token rejected");
                return Err(Error::unauthorized("invalid id_token"));
            }
            Err(VerifyError::Infra(err)) => {
                tracing::warn!(%err, "epic JWKS unavailable");
                return Err(Error::unavailable("identity provider unavailable"));
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
// Durable prune reaction — sessions grow unboundedly (INSERT-only, TTL filtered on
// read), so accounts reacts to the seeded daily `accounts-sessions-prune` schedule and
// deletes expired rows on the DELIVERY tx (exactly-once with the checkpoint advance).
// Copied from audit's prune: subscribe raw by the CONTRACT descriptor's topic const, no
// `schedulerevents::Fired` payload-type import — the handler parses only `name`.
// ============================================================================

/// The prune reaction's own durable checkpoint — a globally unique subscription id,
/// independent of any other accounts subscription.
const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "accounts.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

/// The `scheduler.fired` `name` accounts prunes on — a shared-vocabulary string (like a
/// topic): the scheduler seeds this schedule name (86400s), accounts reacts to it.
const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::SESSIONS_PRUNE;

/// Just the `name` field of a `scheduler.fired` payload — parsed out of the raw JSON
/// rather than importing `schedulerevents::Fired` into the handler (zero-coupling: it
/// subscribes by the descriptor's topic const but never deserializes the producer type).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

/// Prunes expired sessions as a REACTION to `scheduler.fired{name:"accounts-sessions-prune"}`,
/// on the delivery tx (downcast from `Delivery`). A non-prune schedule name is a
/// committed no-op (the tick is marked processed, nothing to do); a redelivered tick is
/// idempotent.
struct PruneHandler {
    svc: Arc<Service>,
}

impl TxHandler for PruneHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(BusError::from)?;
            if fired.name != PRUNE_SCHEDULE_NAME {
                return Ok(()); // some other schedule — marked processed, nothing to do
            }
            self.svc
                .store
                .prune_expired_sessions(conn)
                .await
                .map_err(BusError::transport)?;
            Ok(())
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
/// serves an internal QUIC edge — the module never knows. The Auth face's
/// dev-auth-gated methods (`register`/`login`) self-reject at the service level when
/// `ACCOUNTS_DEV_AUTH` is off — the impl guard is the SINGLE gate for every exposure
/// path (HTTP op, player QUIC, mTLS edge), so the trust model cannot diverge.
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
            // Resolve the dev-auth gate once, here, so it is the single source of
            // truth for BOTH the (gated) HTTP op contributions and the service-level
            // guard on the edge Auth face (register/login).
            dev_auth: env_bool("ACCOUNTS_DEV_AUTH", false),
            epic: OnceLock::new(),
            // Pure construction (no I/O): the argon RAM cap, the login admission
            // bound and the real verifier — admin's shapes.
            argon_permits: Arc::new(Semaphore::new(2)),
            login_slots: Arc::new(Semaphore::new(32)),
            verifier: Arc::new(ArgonVerifier),
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
    /// browser routes, contributes the player operations (all unconditional — the
    /// dev/epic gating lives at the impl), the local admin
    /// item, and the generated Sessions + Auth RPC faces to the edge slot.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // dev/password provider — local testing convenience, gated off for prod. The
        // register/login OPERATIONS are contributed unconditionally below; the
        // `Service::dev_auth` guard rejects both methods at the impl when the gate is
        // off — ONE trust model, every exposure path.
        if svc.dev_auth {
            tracing::warn!(
                "ACCOUNTS_DEV_AUTH is ON — /accounts/register and /accounts/login (dev/password \
                 auth) are enabled; this is an explicit local-dev opt-in, keep it OFF (the \
                 fail-closed default) in production"
            );
        }

        // epic provider — the real federated path via Epic Account Services (OIDC).
        // Enabled only when configured. Defaults point at EAS endpoints (web OAuth);
        // sub is the Epic Account ID.
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

        // Player operations: the generated Auth OpSets, ALL contributed
        // unconditionally — the gating lives at the impl (register/login → NotFound
        // when dev auth is off, loginEpic → Unavailable when epic is unconfigured),
        // so the monolith and split front-door route sets are structurally equal.
        ops::register_player_ops(ctx, svc.clone());

        // Durable session prune as a REACTION to scheduler.fired on the durable plane.
        // Raw subscribe by the CONTRACT descriptor's topic const (no payload-type
        // import): the handler parses `name` and prunes only for "accounts-sessions-prune",
        // inside the handed delivery tx (audit's prune precedent — a contract-crate dep,
        // NOT a scheduler capability, so no requires() entry).
        let prune: Arc<dyn TxHandler> = Arc::new(PruneHandler { svc: svc.clone() });
        ctx.bus()
            .on_tx_raw(PRUNE_SUB, schedulerevents::FIRED.topic(), prune);

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
        //
        // The whole Auth trait face is registered (the generated `register_server`
        // installs all methods at once), but the TRUST GATES live in the impl so the
        // edge face matches the HTTP ops exactly: `register`/`login` self-reject with
        // NotFound when `dev_auth` is off, and `login_epic` answers `Unavailable`
        // until `EPIC_CLIENT_ID` is configured. `me` (+ Sessions `verify_session` and
        // the admin face) stay unconditional — the gateway/admin fan-out need them.
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
mod epic_tests;
#[cfg(test)]
mod tests;

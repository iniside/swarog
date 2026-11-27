//! `accounts` — player identity (port of Go's `modules/accounts`). It owns schema
//! `accounts` and is a trusted VERIFIER of external identities: the production model
//! is federation (the EOS Connect shape) — the backend checks an IdP's signed token,
//! never holds a password, except for the `password` provider: login is always on,
//! self-registration is gated behind `ACCOUNTS_OPEN_REGISTRATION` (explicit opt-in,
//! default OFF, loud warn when set). Its subject is the email after [`normalize_email`]. One
//! product-scoped `player_id`, many credential providers over it
//! (`identities(provider, subject) → player_id`), ES256 JWT access tokens
//! ([`accountsapi::ACCESS_TTL_MINUTES`]-minute, signed in `jwt`) renewed by rotating
//! opaque refresh tokens whose family lives at most 30 days.
//!
//! Capabilities (all topology-blind — the module never knows the process layout):
//!   - `accounts.token_state` ([`accountsapi::TokenState`]) — the public keys and
//!     revocations the gateway's auth-once verifier checks access tokens against
//!     (registry swap: local here, an edge client from `accountsrpc::remote_factories()`
//!     in a split peer).
//!   - `accounts.auth` ([`accountsapi::Auth`]) — register/login/loginFederated/
//!     createGuest/refresh/me/link/beginDelete/deleteAccount, ALL contributed as gateway
//!     operations UNCONDITIONALLY — the dev/provider gating lives at the impl (see
//!     `ops.rs`).
//!   - `accounts.directory` ([`accountsapi::Directory`]) — wire-only player lookup by
//!     id or `Name#1234` handle, for a consumer that renders or validates a player
//!     other than its own caller.
//!   - Epic web OAuth — two HTTP-NATIVE browser routes (`POST /accounts/epic/start`,
//!     `GET /accounts/epic/callback`) mounted on the shared router when
//!     `EPIC_CLIENT_SECRET` is configured.
//!   - Google web OAuth — browser sign-in only: the hosted form's
//!     `POST /accounts/oidc/google` and `GET /accounts/oidc/google/callback`, live when
//!     `GOOGLE_CLIENT_SECRET` is configured.
//!
//! Durable-events rule (deliberate deviation from Go's plain `Emit`):
//! `player.registered` is emitted via `bus::emit_tx` INSIDE the registration store
//! transaction — the event is durable iff the player row is.

mod admin;
mod clients;
mod codes;
pub mod conformance;
mod discovery;
mod epic_oauth;
mod guest;
mod jwt;
mod oauth_flow;
mod oidc;
mod ops;
mod password;
mod providers;
mod secret;
mod signing_keys;
mod store;
mod web;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::{AnyTx, Bus, Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use lifecycle::{Context, Ddl, Module};
use accountsapi::grant_types::REFRESH_TOKEN;
use opsapi::{Error, Identity};
use sqlx::PgConnection;

use tokio::sync::Semaphore;

use crate::password::{hash_password, ArgonVerifier, PasswordVerifier, DUMMY_HASH};
use crate::providers::{
    CredentialVerifier, ProviderConfig, Providers, Resolution, VerifiedSubject, VerifyError, GUEST,
};
use crate::store::{LinkOutcome, Player, Store, StoreError};
use crate::web::session::NewBrowserSession;

/// Input caps enforced before any expensive verifier, Argon2, RPC, or database work.
/// Email follows RFC 5321's total-address maximum; password is capped at 1 KiB
/// because Argon2 cost scales with input length. Email, password and the provider name
/// are private accounts-domain policy; the display-name, handle and session-token caps
/// live in `accountsapi`, shared with the gateway and with directory consumers for
/// cross-layer fast rejection.
const MAX_EMAIL_BYTES: usize = 320;
const MAX_PASSWORD_BYTES: usize = 1024;
pub const MAX_PROVIDER_NAME_BYTES: usize = 64;
/// The delete ticket is server-minted (43 base64url characters) and only ever replayed
/// back to us, so nothing outside this module sizes it; the headroom is the session
/// token's, for the same reason — bounding attacker-controlled input before the lookup.
pub const MAX_DELETE_TICKET_BYTES: usize = 128;

/// The one spelling of a password identity's email: trimmed and ASCII-lowercased, so
/// `A@x.io` and `a@x.io` are one account. Every password path calls it before the cap.
pub(crate) fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// Why a password account was not created.
pub(crate) enum PasswordRefusal {
    /// The normalised email already names a `password` identity.
    EmailTaken,
    /// Every handle of the display name is taken.
    NoFreeHandle,
}

impl PasswordRefusal {
    /// The self-registration op's answer: both are 409.
    fn conflict(&self) -> Error {
        match self {
            PasswordRefusal::EmailTaken => Error::conflict("email already registered"),
            PasswordRefusal::NoFreeHandle => Error::conflict("display name has no free handle"),
        }
    }
}

/// What [`Service::reset_password`] did.
pub(crate) enum PasswordReset {
    NoPlayer,
    /// The player signs in with no password; nothing changed.
    NoPassword,
    /// The new password, shown once, and the emails it signs in with.
    Reset { emails: Vec<String>, password: String },
}

/// The SHARED cap checks — the register/login handlers and factual conformance probes
/// call these same pure fns, so the probe
/// proves what the handlers actually enforce, never a const compared to itself.
/// Byte counts (`str::len()`), not characters. Emptiness is checked separately by
/// each handler (a different rejection, not a cap).
pub(crate) fn email_within_cap(email: &str) -> bool {
    email.len() <= MAX_EMAIL_BYTES
}

pub(crate) fn password_within_cap(password: &str) -> bool {
    password.len() <= MAX_PASSWORD_BYTES
}

pub(crate) fn display_name_within_cap(display_name: &str) -> bool {
    display_name.len() <= accountsapi::MAX_DISPLAY_NAME_BYTES
}

pub(crate) fn handle_within_cap(handle: &str) -> bool {
    handle.len() <= accountsapi::MAX_HANDLE_BYTES
}

pub(crate) fn provider_name_within_cap(provider: &str) -> bool {
    provider.len() <= MAX_PROVIDER_NAME_BYTES
}

/// Whether `credential` fits the cap the RESOLVED provider states for its own
/// credential shape — the single cap authority the handler and the conformance probe
/// both traverse, so no second constant can drift from what a request meets.
pub(crate) fn credential_within_cap(verifier: &dyn CredentialVerifier, credential: &str) -> bool {
    credential.len() <= verifier.max_credential_bytes()
}

pub(crate) fn client_id_within_cap(client_id: &str) -> bool {
    client_id.len() <= accountsapi::MAX_CLIENT_ID_BYTES
}

pub(crate) fn opaque_token_within_cap(token: &str) -> bool {
    token.len() <= accountsapi::MAX_OPAQUE_TOKEN_BYTES
}

pub(crate) fn delete_ticket_within_cap(ticket: &str) -> bool {
    ticket.len() <= MAX_DELETE_TICKET_BYTES
}

/// The fixed decoy candidate verified against [`DUMMY_HASH`] when the email is
/// unknown or the input invalid — never the caller's real password against a decoy.
const DECOY_CANDIDATE: &str = "accounts-invalid-credentials";

/// The schema `migrate` fills the signing keys in and the JWKS reads them from: one name
/// for both, so a rename cannot split them.
const SIGNING_KEYS_SCHEMA: &str = "accounts";

/// This module's OWN schema and tables, nothing else — full logical isolation (#10).
/// Idempotent; `migrate` adds the indexes. The identities/sessions FKs are INTERNAL to
/// the accounts schema (allowed; the ban is on cross-module FKs).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS accounts;

CREATE TABLE IF NOT EXISTS accounts.players (
	id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	display_name  text        NOT NULL,
	discriminator text        NOT NULL,
	created_at    timestamptz NOT NULL DEFAULT now()
);
-- The handle authority: display names are NOT unique, so `Name#1234` is what addresses a
-- player, and THIS table decides which pairs are free. Minting INSERTs a claim here and
-- retries what the unique key rejects; deleting a player NULLs `player_id` and keeps the
-- row, so a freed handle is refused by the same key that refuses a live one and can never
-- be re-minted (impersonation-by-recycling). NO foreign key to `players`: the cascade
-- would take the reservation with the row it exists to outlive.
-- Every live player has a row here — an `accounts` schema without one per player is
-- wiped and rebooted (wipe-over-migrations), never backfilled.
CREATE TABLE IF NOT EXISTS accounts.handles (
	display_name_lower text NOT NULL,
	discriminator      text NOT NULL,
	player_id          uuid,
	claimed_at         timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (display_name_lower, discriminator)
);

-- The confirmation ticket `begin_delete` mints. One live ticket per player (the primary
-- key), so a repeated call returns the same one; it dies with the player through the
-- cascade, unlike the reservation and the receipt.
CREATE TABLE IF NOT EXISTS accounts.delete_tickets (
	player_id  uuid PRIMARY KEY REFERENCES accounts.players(id) ON DELETE CASCADE,
	ticket     text        NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	expires_at timestamptz NOT NULL
);

-- The permanent record of one completed deletion: what an internal-edge replay of
-- `delete_account` is answered from after the player row is gone. NO foreign key to
-- `players`, for the same reason as `handles`.
CREATE TABLE IF NOT EXISTS accounts.deletion_receipts (
	player_id  uuid PRIMARY KEY,
	handle     text        NOT NULL,
	ticket     text        NOT NULL,
	deleted_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS accounts.identities (
	provider    text NOT NULL,
	subject     text NOT NULL,
	player_id   uuid NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	secret_hash text,                         -- password: argon2id PHC; guest: SHA-256 digest
	created_at  timestamptz NOT NULL DEFAULT now(),
	last_used_at timestamptz,                 -- the newest session family minted through it
	PRIMARY KEY (provider, subject)
);

-- The hosted login pages' signed-in browsers: the cookie's opaque token is stored only as
-- its SHA-256 digest; it is not an access token, so a browser cookie never authenticates
-- an operation.
CREATE TABLE IF NOT EXISTS accounts.browser_sessions (
	token_digest bytea PRIMARY KEY,
	player_id    uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	csrf_token   text        NOT NULL,
	created_at   timestamptz NOT NULL DEFAULT now(),
	expires_at   timestamptz NOT NULL
);

-- A program tokens are issued to, created and deleted in the admin portal or seeded as
-- the dev client.
CREATE TABLE IF NOT EXISTS accounts.oidc_clients (
	client_id    text PRIMARY KEY,
	display_name text        NOT NULL CHECK (display_name <> ''),
	grant_types   text[]      NOT NULL CHECK (cardinality(grant_types) > 0),
	audience      text        NOT NULL,
	redirect_uris text[]      NOT NULL DEFAULT '{}',
	root_domains  text[]      NOT NULL DEFAULT '{}',
	scopes        text[]      NOT NULL DEFAULT '{}',
	-- NULL for a public client.
	secret_digest text,
	created_at    timestamptz NOT NULL DEFAULT now()
);

-- One login's token family: every token minted from that login, for one client. A revoked
-- family's row is deleted, and its refresh tokens with it.
CREATE TABLE IF NOT EXISTS accounts.token_families (
	id                uuid PRIMARY KEY,
	player_id         uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	client_id         text        NOT NULL REFERENCES accounts.oidc_clients(client_id) ON DELETE CASCADE,
	created_at        timestamptz NOT NULL DEFAULT now(),
	-- The newest access token's expiry, moved forward by every mint in the family.
	access_expires_at timestamptz NOT NULL
);

-- Rotating refresh tokens. A consumed row is KEPT (until it expires) with the successor
-- it minted: it is the only record that can tell a replay from an unknown token.
CREATE TABLE IF NOT EXISTS accounts.refresh_tokens (
	token       text PRIMARY KEY,
	player_id   uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	family_id   uuid        NOT NULL REFERENCES accounts.token_families(id) ON DELETE CASCADE,
	issued_at   timestamptz NOT NULL DEFAULT now(),
	expires_at  timestamptz NOT NULL,
	used_at     timestamptz,
	replaced_by text
);

-- Access-token revocations, read by every verifier. No foreign keys: a deleted player's or
-- client's row must outlive the cascade until every token it refuses has expired.
CREATE TABLE IF NOT EXISTS accounts.token_revocations (
	id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	family_id  uuid,
	player_id  uuid,
	client_id  text,
	revoked_at timestamptz NOT NULL,
	expires_at timestamptz NOT NULL,
	CHECK (num_nonnulls(family_id, player_id, client_id) = 1)
);

-- Authorization codes of /accounts/oidc/authorize, keyed by the code's SHA-256 digest. A
-- redeemed row stays until the replay window passes, so a replay can still find it.
-- session_digest is the browser session the code was issued under. Deleting the session
-- (logout, prune, login rotation, reset) sets it NULL rather than deleting the row: a code
-- redeems only while it is set, and a redeemed row must stay for replay detection. It is
-- declared after player_id so the insert's foreign-key checks lock the player row before the
-- session row, the order a reset locks them (the checks fire in constraint-creation order).
CREATE TABLE IF NOT EXISTS accounts.oidc_codes (
	code_digest    bytea PRIMARY KEY,
	client_id      text        NOT NULL REFERENCES accounts.oidc_clients(client_id) ON DELETE CASCADE,
	player_id      uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	session_digest bytea       REFERENCES accounts.browser_sessions(token_digest) ON DELETE SET NULL,
	redirect_uri   text        NOT NULL,
	code_challenge text        NOT NULL,
	scope          text        NOT NULL,
	nonce          text,
	auth_time      timestamptz NOT NULL,
	created_at     timestamptz NOT NULL DEFAULT now(),
	redeemed_at    timestamptz,
	family_id      uuid,
	CHECK ((redeemed_at IS NULL) = (family_id IS NULL))
);
"#;

/// The web-OAuth in-flight redemption store, shared by every provider's flow. Shared, NOT
/// process memory, so a provider's callback can LB-route to ANY replica: new_state INSERTs
/// and take_state is a DELETE ... RETURNING scoped to the redeeming provider
/// (cross-replica exactly-once single-redemption, 10-min TTL as a created_at predicate).
/// intent decides the callback: login mints a player session for client_id, link attaches the identity
/// to session_token's player, browser mints a browser session, ends replaces_session (the
/// digest of the browser's previous session cookie) and returns to return_to. Only Epic
/// has login and link. No player_id and deliberately NO cascade: a LINK row still in
/// flight when its player is deleted names only that player's session token, and it is
/// swept by the same 10-minute TTL prune as any abandoned one. client_id has no FK either:
/// the mint refuses a client deleted while its login is in flight.
static OAUTH_STATES_DDL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"
CREATE TABLE IF NOT EXISTS accounts.oauth_states (
	state            text PRIMARY KEY,
	provider         text        NOT NULL CHECK (provider IN ('{EPIC}', '{GOOGLE}')),
	intent           text        NOT NULL CHECK (intent IN ('login', 'link', 'browser')),
	session_token    text,
	return_to        text,
	replaces_session bytea,
	browser_binding  text        NOT NULL,
	client_id        text,
	created_at       timestamptz NOT NULL DEFAULT now(),
	CONSTRAINT oauth_states_epic_only CHECK (provider = '{EPIC}' OR intent = 'browser'),
	CHECK ((intent = 'link') = (session_token IS NOT NULL)),
	CHECK ((intent = 'browser') = (return_to IS NOT NULL)),
	CHECK (intent = 'browser' OR replaces_session IS NULL),
	CHECK ((intent = 'login') = (client_id IS NOT NULL))
);"#,
        EPIC = providers::EPIC,
        GOOGLE = providers::GOOGLE,
    )
});

/// Refuses a `{schema}.oauth_states` that predates a column every start writes:
/// `CREATE TABLE IF NOT EXISTS` keeps the old table, and every start would then fail its
/// INSERT.
async fn refuse_stale_oauth_states(pool: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    for column in ["intent", "provider", "client_id"] {
        let present: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = 'oauth_states' AND column_name = $2",
        )
        .bind(schema)
        .bind(column)
        .fetch_one(pool)
        .await?;
        if present == 0 {
            anyhow::bail!(
                "accounts: {schema}.oauth_states has no {column} column. Remedy: DROP TABLE \
                 {schema}.oauth_states; and reboot (it holds only in-flight sign-ins)."
            );
        }
    }
    Ok(())
}

/// Refuses a `{schema}.refresh_tokens` without an ON DELETE CASCADE foreign key to
/// `{schema}.token_families`: `CREATE TABLE IF NOT EXISTS` keeps an older table, whose
/// tokens would outlive a deleted family.
async fn refuse_unfamilied_tokens(pool: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    let families = format!("{schema}.token_families");
    for table in ["refresh_tokens"] {
        let table = format!("{schema}.{table}");
        let linked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_constraint \
                             WHERE conrelid = $1::regclass AND contype = 'f' \
                               AND confrelid = $2::regclass AND confdeltype = 'c')",
        )
        .bind(&table)
        .bind(&families)
        .fetch_one(pool)
        .await?;
        if !linked {
            anyhow::bail!(
                "accounts: {table} has no ON DELETE CASCADE foreign key to {families}. \
                 Remedy: DROP SCHEMA {schema} CASCADE; and reboot."
            );
        }
    }
    Ok(())
}

/// Refuses a `{schema}.oidc_clients` without a column the client code reads:
/// `CREATE TABLE IF NOT EXISTS` keeps an older table.
async fn refuse_stale_clients(pool: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    for column in ["redirect_uris", "root_domains", "scopes", "secret_digest"] {
        let present: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = 'oidc_clients' AND column_name = $2",
        )
        .bind(schema)
        .bind(column)
        .fetch_one(pool)
        .await?;
        if present == 0 {
            anyhow::bail!(
                "accounts: {schema}.oidc_clients has no {column} column. \
                 Remedy: DROP SCHEMA {schema} CASCADE; and reboot."
            );
        }
    }
    Ok(())
}

/// Refuses an existing `{schema}.oidc_codes` without the `session_digest` column every code
/// insert writes: `CREATE TABLE IF NOT EXISTS` keeps an older table. Migrate runs it before
/// its DDL, whose `oidc_codes_session_idx` step would otherwise fail first on the missing
/// column; an absent table passes, the DDL creates it.
async fn refuse_stale_codes(pool: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    let stale: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
                         WHERE table_schema = $1 AND table_name = 'oidc_codes') \
            AND NOT EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = $1 AND table_name = 'oidc_codes' \
                               AND column_name = 'session_digest')",
    )
    .bind(schema)
    .fetch_one(pool)
    .await?;
    if stale {
        anyhow::bail!(
            "accounts: {schema}.oidc_codes has no session_digest column. \
             Remedy: DROP SCHEMA {schema} CASCADE; and reboot."
        );
    }
    Ok(())
}

/// The one answer to a client a mint or refresh may not issue for: an id over
/// [`accountsapi::MAX_CLIENT_ID_BYTES`], no such client, or a client not listing the grant.
fn invalid_client() -> Error {
    Error::unauthorized("invalid client")
}

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// A mint the identity stamp refused: with a verified hash, that hash was replaced (an
/// operator reset) after it was checked, answered like a wrong password; without one,
/// the identity is gone, which no login path leaves behind.
fn stamp_refused(secret_hash: Option<&str>) -> Error {
    if secret_hash.is_some() {
        Error::unauthorized("invalid credentials")
    } else {
        Error::internal("no identity to mint a session through")
    }
}

/// The identity a login is minted through, which [`Service::issue_session_tx`] stamps as
/// last used; `secret_hash` is the hash a password sign-in verified.
struct MintedVia<'a> {
    provider: &'a str,
    subject: &'a str,
    secret_hash: Option<&'a str>,
}

/// Who presents a refresh token: an op-path caller, which names no client, or a client
/// `/token` authenticated.
#[derive(Clone, Copy)]
pub(crate) enum RefreshPresenter<'a> {
    OpPath,
    Client(&'a str),
}

/// A password [`Service::verify_password`] accepted: the player, the normalised email
/// of its `password` identity and the stored hash the password matched. Minting through
/// it checks, in the minting transaction, that the identity still stores that hash, so
/// a sign-in verified just before an operator reset cannot mint after it.
pub(crate) struct VerifiedPassword {
    pub(crate) player: Player,
    email: String,
    hash: String,
}

/// A store failure on the delete path: an id the `::uuid` cast could not parse gets the
/// same `NotFound` an unknown ticket gets — never a 500, and never a 403, which would
/// confirm the ticket exists.
fn delete_store_error(e: sqlx::Error) -> Error {
    if store::is_invalid_uuid(&e) {
        Error::not_found(TICKET_NOT_FOUND)
    } else {
        internal(e)
    }
}

/// The ONE answer every miss on the delete path gives: an unknown ticket, an expired
/// one, one belonging to another player, and an unparseable caller id.
const TICKET_NOT_FOUND: &str = "delete ticket not found";

// ============================================================================
// Service — backs TokenState + Auth (the registry capabilities + the generated edge
// faces + the gateway's in-process invokers).
// ============================================================================

/// What other modules get from `require::<dyn TokenState>` / `require::<dyn Auth>`.
/// Holds the store, the bus (for the atomic `player.registered` emit) and — once
/// `init` has parsed the provider configuration — the credential verifiers.
pub struct Service {
    pub(crate) store: Store,
    bus: Arc<Bus>,
    /// Whether password self-registration is open (`ACCOUNTS_OPEN_REGISTRATION`, resolved
    /// in `register`). Gates only `Auth::register`, at the SERVICE level: the HTTP op
    /// is contributed unconditionally, so the gateway route, the player-QUIC plane and
    /// the internal mTLS edge face all meet this one guard and answer NotFound when it
    /// is off. Password login has no gate.
    open_registration: bool,
    /// The credential verifiers this process configured, filled in `init` from the
    /// validated [`ProviderConfig`]. The credential ops are contributed
    /// unconditionally; a known provider with no verifier here answers a typed
    /// `Unavailable` (→ 503) on every path, edge calls included.
    providers: OnceLock<Arc<Providers>>,
    /// RAM cap on concurrent argon2 hashes (64 MiB each): at most 2 run at once,
    /// on `spawn_blocking` threads — never on an async worker (admin's pattern).
    argon_permits: Arc<Semaphore>,
    /// Admission bound on concurrent login requests: beyond 32 in flight new logins
    /// are shed with `Unavailable` (503) instead of queueing without bound.
    login_slots: Arc<Semaphore>,
    /// The injectable verify seam — [`ArgonVerifier`] in production, fakes in tests.
    verifier: Arc<dyn PasswordVerifier>,
    pub(crate) tokens: jwt::Tokens,
}

impl Service {
    /// Looks `name` up in this process's configured verifiers, falling back to the
    /// empty registry when `init` never filled the cell — so an unwired service
    /// answers exactly like one configured with no providers.
    fn resolve_provider(&self, name: &str) -> Resolution<'_> {
        match self.providers.get() {
            Some(providers) => providers.resolve(name),
            None => Providers::empty().resolve(name),
        }
    }

    /// Mints one login for `client_id`: a new token FAMILY and the first access token
    /// inside it, on the caller's transaction, and stamps the identity `via` names as last
    /// used. The family's first refresh token is minted only when the client lists
    /// `refresh_token`; otherwise the answer's `refresh_token` is empty, the family holds no
    /// refresh row, and it grants only until its access token expires. The client must
    /// exist and list the grant [`clients::grant_for_provider`]
    /// names, else nothing is minted and the answer is [`invalid_client`]; every caller has
    /// already capped `client_id` at its op's entry ([`client_id_within_cap`]). With
    /// `via.secret_hash` (a password sign-in) the identity must still store the hash that
    /// was verified, else nothing is minted and the answer is the wrong-password 401. All
    /// three commit with whatever else that transaction carries (the player row, the
    /// `player.registered` append); a refresh rotates inside a family and never comes here,
    /// so it never stamps.
    ///
    /// Lock order: whatever the caller already holds (a registration or first external
    /// login has inserted the player, identity and handle rows; an external login holds the
    /// identity's advisory lock), then the client row (FOR KEY SHARE, so a client delete
    /// waits for this commit), then the player row (FOR KEY SHARE, taken by the family
    /// insert's FK), BEFORE the identity row (the stamp's UPDATE), the same order
    /// `delete_player_tx` locks them in (player, then its identities by cascade). Taking
    /// the client after the caller's rows makes no cycle: a client delete takes no player,
    /// identity or handle lock. The stamp must stay after the first statement that
    /// references `players`, or a login racing the same player's self-delete can deadlock.
    async fn issue_session_tx(
        &self,
        conn: &mut PgConnection,
        p: &Player,
        via: MintedVia<'_>,
        client_id: &str,
    ) -> Result<accountsapi::Session, Error> {
        let MintedVia { provider, subject, secret_hash } = via;
        let Some(grants) = self
            .store
            .client_allows_tx(conn, client_id, clients::grant_for_provider(provider))
            .await
            .map_err(internal)?
        else {
            return Err(invalid_client());
        };
        let (session, _) = self.mint_session_tx(conn, &p.id, client_id, grants, None).await?;
        let stamped = self
            .store
            .stamp_identity_used_tx(conn, provider, subject, &p.id, secret_hash)
            .await
            .map_err(internal)?;
        if !stamped {
            return Err(stamp_refused(secret_hash));
        }
        Ok(session)
    }

    /// The mint every login and code exchange calls after its grant check: a new token
    /// family for `player_id` and `client_id`, under `family_id` when given, its refresh
    /// root when `grants` lists `refresh_token`, and its first access token, on the caller's
    /// transaction, answered with what that token was minted with. The caller's grant check
    /// holds the client row FOR KEY SHARE; the family insert takes the player row FOR KEY
    /// SHARE through its FK.
    async fn mint_session_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        client_id: &str,
        grants: clients::MintGrants,
        family_id: Option<&str>,
    ) -> Result<(accountsapi::Session, store::MintedAccess), Error> {
        let family_id = self
            .store
            .insert_family_tx(conn, player_id, client_id, family_id)
            .await
            .map_err(internal)?;
        let refresh_token = if grants.refresh_token {
            let refresh_token = store::new_token();
            self.store
                .insert_refresh_root_tx(conn, &refresh_token, player_id, &family_id)
                .await
                .map_err(internal)?;
            refresh_token
        } else {
            String::new()
        };
        let (token, minted) = self.sign_access(conn, player_id, &family_id).await?;
        let session = accountsapi::Session {
            player_id: player_id.to_string(),
            token,
            refresh_token,
            access_expires_in_secs: store::access_expires_in_secs(),
        };
        Ok((session, minted))
    }

    /// Mints and signs the next access token of family `family_id` for `player_id`
    /// ([`store::Store::mint_access_tx`]), reading the client's audience and root domains,
    /// the player's handle and a stale signing key on `conn`, the caller's transaction.
    async fn sign_access(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        family_id: &str,
    ) -> Result<(String, store::MintedAccess), Error> {
        let minted = self.store.mint_access_tx(conn, family_id).await.map_err(internal)?;
        let claims = self.tokens.claims(player_id, family_id, &minted);
        let token = self.tokens.sign(conn, &claims).await.map_err(|e| internal(format!("{e:#}")))?;
        Ok((token, minted))
    }

    /// Mints a fresh bearer token through the password identity `verified` matched, in its
    /// own thin transaction, only while that identity still stores the verified hash.
    /// Password login uses this path; registration/external login use their outer tx.
    async fn issue_password_session(
        &self,
        verified: &VerifiedPassword,
        client_id: &str,
    ) -> Result<accountsapi::Session, Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let session = self
            .issue_session_tx(
                &mut tx,
                &verified.player,
                MintedVia {
                    provider: providers::PASSWORD,
                    subject: &verified.email,
                    secret_hash: Some(&verified.hash),
                },
                client_id,
            )
            .await?;
        tx.commit().await.map_err(internal)?;
        Ok(session)
    }

    /// Mints the hosted pages' browser session through the password identity `verified`
    /// matched, stamping it used, only while that identity still stores the verified hash
    /// (the bearer mint's check, [`Store::stamp_identity_used_tx`]). A refusal, or a player
    /// deleted meanwhile, is the wrong-password 401. The same commit ends the session whose
    /// digest is `replaces`, so a refusal leaves it alive. Lock order as
    /// [`Service::issue_session_tx`]: the insert's foreign key takes the player row
    /// before the stamp takes the identity, and the replaced session's row comes last.
    pub(crate) async fn issue_password_browser_session(
        &self,
        verified: &VerifiedPassword,
        replaces: Option<&[u8]>,
    ) -> Result<NewBrowserSession, Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let Some(minted) = self
            .store
            .create_browser_session_tx(&mut tx, &verified.player.id)
            .await
            .map_err(internal)?
        else {
            return Err(Error::unauthorized("invalid credentials"));
        };
        let stamped = self
            .store
            .stamp_identity_used_tx(
                &mut tx,
                providers::PASSWORD,
                &verified.email,
                &verified.player.id,
                Some(&verified.hash),
            )
            .await
            .map_err(internal)?;
        if !stamped {
            return Err(stamp_refused(Some(&verified.hash)));
        }
        if let Some(old) = replaces {
            self.store
                .delete_browser_session_digest_tx(&mut tx, old)
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(minted)
    }

    /// Checks `password` against the password identity of `email`, which the caller has
    /// already passed through [`normalize_email`]; the password login op and the web form
    /// both call it. Bad credentials — an unknown email, a wrong password, or over-cap/empty
    /// input, deliberately indistinguishable — are `Unauthorized` (401). Every admitted
    /// request performs exactly ONE argon2 verify (the real hash or the [`DUMMY_HASH`]
    /// decoy) on a `spawn_blocking` thread behind the argon permit, so unknown-email and
    /// wrong-password are timing-indistinguishable and the 64 MiB hashes never run on an
    /// async worker.
    pub(crate) async fn verify_password(
        &self,
        email: &str,
        password: String,
    ) -> Result<VerifiedPassword, Error> {
        // Admission bound: shed (never queue) beyond 32 concurrent logins. The slot
        // stays in this async frame — releasing it on cancel is correct, the request
        // is gone.
        let Ok(_slot) = self.login_slots.clone().try_acquire_owned() else {
            return Err(Error::unavailable("too many concurrent login attempts"));
        };
        let valid_input = !email.is_empty()
            && email_within_cap(email)
            && !password.is_empty()
            && password_within_cap(&password);
        let identity = if valid_input {
            self.store.password_identity(email).await.map_err(|e| {
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
        let (player, hash) = identity.expect("known_user implies identity");
        Ok(VerifiedPassword {
            player,
            email: email.to_string(),
            hash,
        })
    }

    /// Hashes `password` with argon2id on a blocking thread behind the argon permit, so
    /// the 64 MiB hash never runs on an async worker. The permit MOVES INTO the closure:
    /// spawn_blocking is not cancelled when its JoinHandle drops, so a permit held in the
    /// async frame would be released on client disconnect while the detached hash keeps
    /// running (RAM-cap defeat; admin 5844831 precedent).
    async fn hash_on_permit(&self, password: String) -> Result<String, Error> {
        let Ok(argon) = self.argon_permits.clone().acquire_owned().await else {
            return Err(Error::internal("argon2 semaphore closed"));
        };
        tokio::task::spawn_blocking(move || {
            let _permit = argon;
            hash_password(&password)
        })
        .await
        .map_err(|e| Error::internal(format!("password hash task failed: {e}")))?
        .map_err(internal)
    }

    /// Inserts a player with its `password` identity `email` (already normalised) and
    /// appends `player.registered` on the caller's tx: the core self-registration and the
    /// operator's Create account share. A refusal leaves the tx for the caller to roll back.
    async fn insert_password_player_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        email: &str,
        display: &str,
        hash: &str,
    ) -> Result<Result<Player, PasswordRefusal>, Error> {
        let p = match self
            .store
            .insert_player_with_identity_tx(tx, providers::PASSWORD, email, display, Some(hash))
            .await
        {
            Ok(p) => p,
            Err(StoreError::Taken) => return Ok(Err(PasswordRefusal::EmailTaken)),
            Err(StoreError::HandleExhausted) => return Ok(Err(PasswordRefusal::NoFreeHandle)),
            Err(StoreError::Db(e)) => {
                tracing::error!(err = %e, "register failed");
                return Err(internal(e));
            }
        };
        self.emit_registered_tx(tx, &p, providers::PASSWORD).await?;
        Ok(Ok(p))
    }

    /// The operator's Create account: a player with a `password` identity `email` (already
    /// normalised) and a fresh random password, inserted with its `player.registered` in
    /// one tx and no session. Answers the player and the password, which nothing stores
    /// readably.
    pub(crate) async fn create_password_account(
        &self,
        email: &str,
        display: &str,
    ) -> Result<Result<(Player, String), PasswordRefusal>, Error> {
        let password = password::new_password();
        let hash = self.hash_on_permit(password.clone()).await?;
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        match self.insert_password_player_tx(&mut tx, email, display, &hash).await {
            Ok(Ok(p)) => {
                tx.commit().await.map_err(internal)?;
                Ok(Ok((p, password)))
            }
            Ok(Err(refusal)) => {
                tx.rollback().await.map_err(internal)?;
                Ok(Err(refusal))
            }
            Err(err) => {
                tx.rollback().await.ok();
                Err(err)
            }
        }
    }

    /// The operator's Reset password: a fresh random password on every `password` identity
    /// of the player and, in the same tx, the end of all its logins
    /// ([`Store::kill_player_families_tx`]: token families, refresh tokens, browser
    /// sessions), so a signed-in holder of the old password is signed out with it.
    pub(crate) async fn reset_password(&self, player_id: &str) -> Result<PasswordReset, Error> {
        let password = password::new_password();
        let hash = self.hash_on_permit(password.clone()).await?;
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        // Takes the player row first, the lock order of every login and refresh.
        if self.store.kill_player_families_tx(&mut tx, player_id).await.map_err(internal)?.is_none() {
            tx.rollback().await.map_err(internal)?;
            return Ok(PasswordReset::NoPlayer);
        }
        let emails = self.store.set_password_hash_tx(&mut tx, player_id, &hash).await.map_err(internal)?;
        if emails.is_empty() {
            tx.rollback().await.map_err(internal)?;
            return Ok(PasswordReset::NoPassword);
        }
        tx.commit().await.map_err(internal)?;
        Ok(PasswordReset::Reset { emails, password })
    }

    async fn register_hashed(
        &self,
        email: &str,
        display: &str,
        hash: &str,
        client_id: &str,
    ) -> Result<accountsapi::Session, Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let p = match self.insert_password_player_tx(&mut tx, email, display, hash).await {
            Ok(Ok(p)) => p,
            Ok(Err(refusal)) => {
                tx.rollback().await.map_err(internal)?;
                return Err(refusal.conflict());
            }
            Err(err) => {
                tx.rollback().await.ok();
                return Err(err);
            }
        };
        let session = match self
            .issue_session_tx(
                &mut tx,
                &p,
                MintedVia { provider: providers::PASSWORD, subject: email, secret_hash: None },
                client_id,
            )
            .await
        {
            Ok(session) => session,
            Err(err) => {
                tx.rollback().await.ok();
                return Err(err);
            }
        };
        tx.commit().await.map_err(internal)?;
        Ok(session)
    }

    /// Transactional first-login: lock the external identity, re-read under that
    /// lock, then create at most one player/event while every caller gets its own
    /// session in the same transaction. IdP I/O has already completed before entry.
    pub(crate) async fn external_login(
        &self,
        provider: &str,
        subject: &str,
        display_name: &str,
        secret_hash: Option<&str>,
        client_id: &str,
    ) -> Result<(accountsapi::Session, bool), Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let (p, created) =
            match self.external_player_tx(&mut tx, provider, subject, display_name, secret_hash).await {
                Ok(found) => found,
                Err(err) => {
                    tx.rollback().await.ok();
                    return Err(err);
                }
            };
        let session = match self
            .issue_session_tx(
                &mut tx,
                &p,
                MintedVia { provider, subject, secret_hash: None },
                client_id,
            )
            .await
        {
            Ok(session) => session,
            Err(err) => {
                tx.rollback().await.ok();
                return Err(err);
            }
        };
        tx.commit().await.map_err(internal)?;
        Ok((session, created))
    }

    /// `external_login` for a browser sign-in: the same find-or-create, but no player
    /// session or refresh token. Answers the player and whether it was created.
    pub(crate) async fn external_browser_login(
        &self,
        provider: &str,
        subject: &str,
        display_name: &str,
    ) -> Result<(Player, bool), Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let found = match self.external_player_tx(&mut tx, provider, subject, display_name, None).await {
            Ok(found) => found,
            Err(err) => {
                tx.rollback().await.ok();
                return Err(err);
            }
        };
        tx.commit().await.map_err(internal)?;
        Ok(found)
    }

    /// Locks the external identity, re-reads under that lock and creates at most one
    /// player (with its durable `player.registered`) inside `tx`. The caller rolls back
    /// on `Err`.
    async fn external_player_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        provider: &str,
        subject: &str,
        display_name: &str,
        secret_hash: Option<&str>,
    ) -> Result<(Player, bool), Error> {
        // First statement after BEGIN: every external identity writer shares it.
        self.store
            .lock_identity_tx(tx, provider, subject)
            .await
            .map_err(internal)?;
        if let Some(p) = self
            .store
            .player_by_identity_tx(tx, provider, subject)
            .await
            .map_err(internal)?
        {
            return Ok((p, false));
        }
        let p = match self
            .store
            .insert_player_with_identity_tx(tx, provider, subject, display_name, secret_hash)
            .await
        {
            Ok(p) => p,
            Err(StoreError::Taken) => {
                return Err(Error::internal("identity was taken while its writer lock was held"));
            }
            Err(StoreError::HandleExhausted) => {
                return Err(Error::conflict("display name has no free handle"));
            }
            Err(StoreError::Db(e)) => return Err(internal(e)),
        };
        self.emit_registered_tx(tx, &p, provider).await?;
        Ok((p, true))
    }

    /// Resolves `provider` and verifies `credential` with it — the single guard
    /// ORDER every credential path traverses: the name cap before the name is ever a
    /// registry key, then the RESOLVED provider's own credential cap before any
    /// verifier, JWKS or store work, then the rejected-vs-unavailable split that keeps
    /// an IdP outage from reading as bad credentials.
    async fn verify_credential(
        &self,
        provider: &str,
        credential: &str,
    ) -> Result<VerifiedSubject, Error> {
        if !provider_name_within_cap(provider) {
            return Err(Error::invalid("provider too long"));
        }
        let verifier = match self.resolve_provider(provider) {
            Resolution::Configured(v) => v.clone(),
            Resolution::KnownButUnconfigured => {
                return Err(Error::unavailable("provider not configured"))
            }
            Resolution::Unknown => return Err(Error::invalid("unknown provider")),
        };
        if credential.is_empty() {
            return Err(Error::invalid("credential is required"));
        }
        if !credential_within_cap(verifier.as_ref(), credential) {
            return Err(Error::invalid("credential too long"));
        }
        match verifier.verify(credential).await {
            Ok(v) => Ok(v),
            Err(VerifyError::Rejected(err)) => {
                tracing::warn!(%provider, %err, "federated credential rejected");
                Err(Error::unauthorized("invalid credential"))
            }
            Err(VerifyError::Infra(err)) => {
                tracing::warn!(%provider, %err, "identity provider unavailable");
                Err(Error::unavailable("identity provider unavailable"))
            }
        }
    }

    /// Attaches an already-verified identity to an EXISTING player: the single
    /// tx-owning, lock-taking, emit-owning authority for linking (the `link` op and
    /// the Epic web-OAuth callback both come here; neither opens a transaction).
    ///
    /// Lock ORDER is part of the contract: the PLAYER lock first, the identity lock
    /// second. The identity lock alone serializes writers of one `(provider,
    /// subject)`, which two links of different providers to the same player never
    /// share — without the player lock both would read "no non-guest identity" under
    /// READ COMMITTED and both would emit `player.promoted`. Any future site taking
    /// both takes them in this order (`external_login` takes only the identity lock,
    /// since it creates a player rather than mutating one).
    ///
    /// The promotion decision is made HERE, from state read under those locks before
    /// the insert, and its event rides the same transaction as the identity row.
    pub(crate) async fn link_identity(
        &self,
        player_id: &str,
        provider: &str,
        subject: &str,
    ) -> Result<LinkOutcome, Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_player_tx(&mut tx, player_id)
            .await
            .map_err(internal)?;
        self.store
            .lock_identity_tx(&mut tx, provider, subject)
            .await
            .map_err(internal)?;
        let had_real = match self.store.has_real_identity_tx(&mut tx, player_id).await {
            Ok(had_real) => had_real,
            Err(e) => {
                tx.rollback().await.ok();
                return Err(internal(e));
            }
        };
        let outcome = match self
            .store
            .link_identity_tx(&mut tx, player_id, provider, subject)
            .await
        {
            Ok(outcome) => outcome,
            Err(StoreError::Taken) => {
                tx.rollback().await.ok();
                return Err(Error::conflict("identity already linked to another player"));
            }
            // Linking writes no player row, so the handle mint is not on this path.
            Err(StoreError::HandleExhausted) => {
                tx.rollback().await.ok();
                return Err(Error::internal("link cannot exhaust a handle"));
            }
            Err(StoreError::Db(e)) => {
                tx.rollback().await.ok();
                return Err(internal(e));
            }
        };
        if outcome == LinkOutcome::Linked && !had_real && provider != GUEST {
            if let Err(err) = self.emit_promoted_tx(&mut tx, player_id, provider).await {
                tx.rollback().await.ok();
                return Err(err);
            }
        }
        tx.commit().await.map_err(internal)?;
        Ok(outcome)
    }

    /// Appends the `player.promoted` durable event on the caller's tx.
    async fn emit_promoted_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        player_id: &str,
        to_provider: &str,
    ) -> Result<(), Error> {
        self.bus
            .emit_tx(
                AnyTx::new(&mut **tx),
                &accountsevents::PLAYER_PROMOTED,
                &accountsevents::PlayerPromoted {
                    player_id: player_id.to_string(),
                    from_provider: GUEST.to_string(),
                    to_provider: to_provider.to_string(),
                },
            )
            .await
            .map_err(internal)
    }

    /// The `{player_id, display_name, handle, identities}` view of one player — shared
    /// by `me` and `link` so both answer with the same read. The handle comes from the
    /// summary projection the directory serves, never a second rendering.
    async fn me_view(&self, player_id: &str) -> Result<accountsapi::MeView, Error> {
        let p = self
            .store
            .player_summary(player_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found("player not found"))?;
        let identities = self.store.identities_of(player_id).await.map_err(internal)?;
        Ok(accountsapi::MeView {
            player_id: p.player_id,
            display_name: p.display_name,
            handle: p.handle,
            identities,
        })
    }

    /// Writes the successor refresh row and moves the family's access expiry for a
    /// successful rotation, on the tx that consumed the presented token.
    async fn complete_rotation_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        family_id: &str,
        consumed: &str,
        successor: String,
    ) -> Result<accountsapi::Session, Error> {
        self.store
            .insert_refresh_successor_tx(conn, &successor, consumed)
            .await
            .map_err(internal)?;
        self.issue_access_in_family_tx(conn, player_id, family_id, successor)
            .await
    }

    /// Refuses `presenter` a token of family `family_id`: on the op path any token of a
    /// confidential client's family, at `/token` one of another client's family, and, when
    /// `rotating` (a rotation or a grace replay), one whose client no longer lists
    /// `refresh_token`. A reuse outside the grace window passes `rotating = false`, so the
    /// family's own presenter still kills it after its client dropped the grant. The caller
    /// rolls back on the refusal, so nothing is consumed or killed.
    async fn refresh_allowed_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
        presenter: RefreshPresenter<'_>,
        rotating: bool,
    ) -> Result<(), Error> {
        let client = self
            .store
            .family_client_tx(conn, family_id, REFRESH_TOKEN)
            .await
            .map_err(internal)?;
        let allowed = client.is_some_and(|c| {
            (c.lists_grant || !rotating)
                && match presenter {
                    RefreshPresenter::OpPath => !c.confidential,
                    RefreshPresenter::Client(client_id) => c.client_id == client_id,
                }
        });
        if allowed {
            Ok(())
        } else {
            Err(invalid_client())
        }
    }

    /// Rotates `refresh_token` for `presenter`: reuse inside the grace window replays the
    /// recorded successor, reuse outside it kills the family, except that a presenter
    /// [`Service::refresh_allowed_tx`] refuses the family (the op path a confidential
    /// client's, a [`RefreshPresenter::Client`] another client's) is refused with nothing
    /// consumed or killed.
    pub(crate) async fn refresh_as(
        &self,
        refresh_token: String,
        presenter: RefreshPresenter<'_>,
    ) -> Result<accountsapi::Session, Error> {
        if !opaque_token_within_cap(&refresh_token) {
            return Err(Error::unauthorized("invalid refresh token"));
        }
        let successor = store::new_token();
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let rotated = match self
            .store
            .rotate_refresh_tx(&mut tx, &refresh_token, &successor)
            .await
        {
            Ok(rotated) => rotated,
            Err(e) => {
                tx.rollback().await.ok();
                return Err(internal(e));
            }
        };
        // Read on the SAME transaction as the failed UPDATE (and skipped entirely when
        // it succeeded — the row is already ours).
        let row = if rotated.is_some() {
            None
        } else {
            match self.store.refresh_row_tx(&mut tx, &refresh_token).await {
                Ok(row) => row,
                Err(e) => {
                    tx.rollback().await.ok();
                    return Err(internal(e));
                }
            }
        };
        let outcome = match store::classify_presentation(rotated, row) {
            store::RefreshVerdict::Rotate {
                player_id,
                family_id,
            } => match self.refresh_allowed_tx(&mut tx, &family_id, presenter, true).await {
                Ok(()) => {
                    self.complete_rotation_tx(&mut tx, &player_id, &family_id, &refresh_token, successor)
                        .await
                }
                Err(err) => {
                    tx.rollback().await.ok();
                    return Err(err);
                }
            },
            store::RefreshVerdict::GraceReplay {
                player_id,
                family_id,
                successor,
            } => match self.refresh_allowed_tx(&mut tx, &family_id, presenter, true).await {
                Ok(()) => {
                    self.issue_access_in_family_tx(&mut tx, &player_id, &family_id, successor)
                        .await
                }
                Err(err) => {
                    tx.rollback().await.ok();
                    return Err(err);
                }
            },
            store::RefreshVerdict::Revoke { family_id } => {
                if let Err(err) = self.refresh_allowed_tx(&mut tx, &family_id, presenter, false).await {
                    tx.rollback().await.ok();
                    return Err(err);
                }
                tracing::warn!(
                    %family_id,
                    "consumed refresh token replayed outside the grace window — revoking the token family"
                );
                match self.store.kill_family_tx(&mut tx, &family_id).await {
                    Ok(_) => Err(Error::unauthorized("invalid refresh token")),
                    Err(e) => Err(internal(e)),
                }
            }
            store::RefreshVerdict::Deny => Err(Error::unauthorized("invalid refresh token")),
        };
        // A revoked family must persist, so the tx commits on the 401 path too; only a
        // 5xx (a store failure mid-verdict) discards the work.
        match &outcome {
            Err(err) if err.status == opsapi::Status::Internal => {
                tx.rollback().await.ok();
            }
            _ => tx.commit().await.map_err(internal)?,
        }
        outcome
    }

    /// Mints one access token inside an EXISTING family, moving the family's
    /// `access_expires_at` to its expiry — the half a grace replay repeats without
    /// rotating anything.
    async fn issue_access_in_family_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        family_id: &str,
        refresh_token: String,
    ) -> Result<accountsapi::Session, Error> {
        let (token, _) = self.sign_access(conn, player_id, family_id).await?;
        Ok(accountsapi::Session {
            player_id: player_id.to_string(),
            token,
            refresh_token,
            access_expires_in_secs: store::access_expires_in_secs(),
        })
    }

    /// Erases one player in ONE transaction: consume the ticket, reserve the handle
    /// permanently, write the receipt, delete the player row — whose cascades take its
    /// identities, token families, refresh tokens and remaining ticket — and append
    /// `player.deleted`.
    ///
    /// The player revocation shares this transaction with the append, and a consumer
    /// becomes eligible for an event only once the appending transaction is globally
    /// visible, so every purge handler runs after accounts' own verification refuses the
    /// player's access tokens. A gateway refuses them only from its next token-state pull.
    ///
    /// The player lock is the first statement after BEGIN (the lock order this module
    /// documents on `link_identity`), which is also what makes the replay deterministic:
    /// a second delivery waits for the original to commit, then finds no ticket and reads
    /// the receipt instead of racing the delete.
    async fn delete_player_atomic(
        &self,
        player_id: &str,
        ticket: &str,
    ) -> Result<accountsapi::DeleteReceipt, Error> {
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        if let Err(e) = self.store.lock_player_tx(&mut tx, player_id).await {
            tx.rollback().await.ok();
            return Err(internal(e));
        }
        let consumed = match self
            .store
            .consume_delete_ticket_tx(&mut tx, player_id, ticket)
            .await
        {
            Ok(consumed) => consumed,
            Err(e) => {
                tx.rollback().await.ok();
                return Err(delete_store_error(e));
            }
        };
        let Some(player_id) = consumed else {
            let replay = match self
                .store
                .deletion_receipt_tx(&mut tx, player_id, ticket)
                .await
            {
                Ok(replay) => replay,
                Err(e) => {
                    tx.rollback().await.ok();
                    return Err(delete_store_error(e));
                }
            };
            tx.rollback().await.ok();
            return replay.ok_or_else(|| Error::not_found(TICKET_NOT_FOUND));
        };

        let receipt = match self.delete_player_steps(&mut tx, &player_id, ticket).await {
            Ok(receipt) => receipt,
            Err(err) => {
                tx.rollback().await.ok();
                return Err(err);
            }
        };
        tx.commit().await.map_err(internal)?;
        Ok(receipt)
    }

    /// The writes of a deletion, in the order they depend on each other: the reservation
    /// and the handle read BEFORE the row they read is gone, then the receipt, then the
    /// delete, then the append.
    async fn delete_player_steps(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        player_id: &str,
        ticket: &str,
    ) -> Result<accountsapi::DeleteReceipt, Error> {
        let handle = self
            .store
            .handle_of_tx(tx, player_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(TICKET_NOT_FOUND))?;
        self.store
            .reserve_handle_tx(tx, player_id)
            .await
            .map_err(internal)?;
        let receipt = self
            .store
            .insert_deletion_receipt_tx(tx, player_id, &handle, ticket)
            .await
            .map_err(internal)?;
        if !self
            .store
            .delete_player_tx(tx, player_id)
            .await
            .map_err(internal)?
        {
            return Err(Error::not_found(TICKET_NOT_FOUND));
        }
        self.bus
            .emit_tx(
                AnyTx::new(&mut **tx),
                &accountsevents::PLAYER_DELETED,
                &accountsevents::PlayerDeleted {
                    player_id: player_id.to_string(),
                    handle,
                },
            )
            .await
            .map_err(internal)?;
        Ok(receipt)
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
impl accountsapi::Directory for Service {
    /// The summaries for `ids`, omitting every id that names no player. Over
    /// [`accountsapi::MAX_LOOKUP_IDS`] ids is `Invalid` — the batch bound is refused,
    /// never silently truncated to a result the caller would read as complete.
    async fn players_by_id(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<accountsapi::PlayerSummary>, Error> {
        if ids.len() > accountsapi::MAX_LOOKUP_IDS {
            return Err(Error::invalid("too many ids"));
        }
        self.store.players_by_id(&ids).await.map_err(internal)
    }

    /// The player a `"Name#1234"` handle names, or `Ok(None)` for a miss.
    async fn find_by_handle(
        &self,
        handle: String,
    ) -> Result<Option<accountsapi::PlayerSummary>, Error> {
        if !handle_within_cap(&handle) {
            return Err(Error::invalid("handle too long"));
        }
        self.store.player_by_handle(&handle).await.map_err(internal)
    }
}

#[async_trait]
impl accountsapi::TokenState for Service {
    async fn token_state(&self) -> Result<accountsapi::TokenStateSnapshot, Error> {
        self.tokens.snapshot().await.map_err(|e| internal(format!("{e:#}")))
    }
}

#[async_trait]
impl accountsapi::Auth for Service {
    /// Password self-registration (AuthNone): creates a player + password identity
    /// and appends the `player.registered` durable event in ONE tx, then mints a session.
    /// `NotFound` (404) unless registration is open; missing email/password → `Invalid`
    /// (400); an email already registered in any letter case → `Conflict` (409).
    async fn register(
        &self,
        email: String,
        password: String,
        display_name: String,
        client_id: String,
    ) -> Result<accountsapi::Session, Error> {
        if !self.open_registration {
            return Err(Error::not_found("registration is not enabled"));
        }
        let email = normalize_email(&email);
        if email.is_empty() || password.is_empty() {
            return Err(Error::invalid("email and password are required"));
        }
        if !email_within_cap(&email) || !password_within_cap(&password) {
            return Err(Error::invalid("email or password too long"));
        }
        if !client_id_within_cap(&client_id) {
            return Err(invalid_client());
        }
        let display = if display_name.is_empty() {
            email.clone()
        } else {
            display_name
        };
        if !display_name_within_cap(&display) {
            return Err(Error::invalid("display name too long"));
        }

        let hash = self.hash_on_permit(password).await?;
        self.register_hashed(&email, &display, &hash, &client_id)
            .await
    }

    /// Password login (AuthNone): [`Service::verify_password`] on the normalised email,
    /// then a fresh session through that identity.
    ///
    /// No per-IP limiter here on purpose: `Auth::login` is a pre-auth opsapi
    /// method — the gateway injects `Identity` only POST-auth, so no client IP ever
    /// reaches this service. Per-IP throttling, if wanted, lives at the gateway
    /// (which already rate-limits by source IP).
    async fn login(
        &self,
        email: String,
        password: String,
        client_id: String,
    ) -> Result<accountsapi::Session, Error> {
        if !client_id_within_cap(&client_id) {
            return Err(invalid_client());
        }
        let email = normalize_email(&email);
        let verified = self.verify_password(&email, password).await?;
        self.issue_password_session(&verified, &client_id).await
    }

    /// Federated login (AuthNone): resolves `provider`, verifies `credential` with
    /// it and logs the player in, provisioning on first sight (which emits
    /// `player.registered` durably). An unknown provider name and an unconfigured
    /// one are DIFFERENT answers — `Invalid` (400) is caller-supplied garbage,
    /// `Unavailable` (503) is a deployment fact about this process. A rejected
    /// credential is `Unauthorized` (401); an IdP infrastructure failure is 503, the
    /// `TokenState` / `verify_access` 503-not-401 precedent: an outage must not read as
    /// bad credentials.
    async fn login_federated(
        &self,
        provider: String,
        credential: String,
        client_id: String,
    ) -> Result<accountsapi::Session, Error> {
        if !client_id_within_cap(&client_id) {
            return Err(invalid_client());
        }
        let verified = self.verify_credential(&provider, &credential).await?;
        let (session, _created) = self
            .external_login(&provider, &verified.subject, &verified.display_name, None, &client_id)
            .await?;
        Ok(session)
    }

    /// Mints a guest player (AuthNone): provisions player + `guest` identity, appends
    /// `player.registered` durably and issues a session in ONE transaction — the same
    /// provisioning path a first-sight federated login takes, differing only in that
    /// this identity carries a secret digest. The ticket is returned here and nowhere
    /// else: `guest::mint_ticket` is the only place the plaintext exists, and the
    /// store keeps only its SHA-256 digest.
    async fn create_guest(&self, client_id: String) -> Result<accountsapi::GuestSession, Error> {
        if !client_id_within_cap(&client_id) {
            return Err(invalid_client());
        }
        let ticket = guest::mint_ticket();
        let display_name = format!("{GUEST}:{}", oidc::short_id(&ticket.subject));
        let (session, _created) = self
            .external_login(
                GUEST,
                &ticket.subject,
                &display_name,
                Some(&ticket.secret_hash),
                &client_id,
            )
            .await?;
        Ok(accountsapi::GuestSession {
            player_id: session.player_id,
            token: session.token,
            refresh_token: session.refresh_token,
            access_expires_in_secs: session.access_expires_in_secs,
            device_secret: ticket.credential,
        })
    }

    /// Rotates a refresh token (AuthNone): consumes the presented one, mints its
    /// successor and a fresh access token, and answers with both. The rotation, the
    /// successor row and the family's access expiry share ONE transaction — a crash mid-way leaves
    /// the presented token unconsumed rather than consumed with no successor.
    ///
    /// Unknown, expired and replayed tokens are the SAME `Unauthorized` — the answer
    /// never tells a holder which of the three it is holding. A replay outside the grace
    /// window additionally revokes the family, and that revocation COMMITS before the
    /// 401 is returned.
    async fn refresh(&self, refresh_token: String) -> Result<accountsapi::Session, Error> {
        self.refresh_as(refresh_token, RefreshPresenter::OpPath).await
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
        self.me_view(pid).await
    }

    /// Attaches a second credential to the CALLING player: verifies `credential`
    /// through the same provider registry and guard order as
    /// [`accountsapi::Auth::login_federated`], then links the resulting subject and
    /// answers with the caller's refreshed view. Re-linking an identity the caller
    /// already owns is an idempotent success; an identity owned by another player is
    /// `Conflict` (409) — accounts are never merged here.
    async fn link(
        &self,
        identity: Identity,
        provider: String,
        credential: String,
    ) -> Result<accountsapi::MeView, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        let verified = self.verify_credential(&provider, &credential).await?;
        self.link_identity(pid, &provider, &verified.subject).await?;
        self.me_view(pid).await
    }

    /// Mints — or re-reads — the caller's ONE live delete ticket. The upsert is the whole
    /// safety property: a repeat answers with the same ticket and the same expiry, so a
    /// client that calls twice cannot invalidate the confirmation its user is looking at.
    /// A caller identity that names no player is `NotFound`.
    async fn begin_delete(&self, identity: Identity) -> Result<accountsapi::DeleteTicket, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        let minted = self
            .store
            .upsert_delete_ticket(pid, &store::new_token())
            .await
            .map_err(internal)?;
        let Some((ticket, expires_at)) = minted else {
            return Err(Error::not_found("player not found"));
        };
        Ok(accountsapi::DeleteTicket {
            ticket,
            expires_at,
        })
    }

    /// Erases the calling player against the ticket `begin_delete` minted. The cap is the
    /// only shape check: a ticket that is not this player's live one — unknown, expired,
    /// or another player's — is the same `NotFound`, since a `Forbidden` would confirm it
    /// exists. Everything the deletion does happens in one transaction
    /// ([`Service::delete_player_atomic`]).
    async fn delete_account(
        &self,
        identity: Identity,
        ticket: String,
    ) -> Result<accountsapi::DeleteReceipt, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        if !delete_ticket_within_cap(&ticket) {
            return Err(Error::invalid("ticket too long"));
        }
        self.delete_player_atomic(pid, &ticket).await
    }
}

// ============================================================================
// Durable prune reaction — refresh tokens, token families, delete tickets, browser sessions
// and revocations all carry a TTL filtered on read, so accounts reacts to the seeded daily
// `accounts-sessions-prune` schedule and deletes the expired rows of all five on the
// DELIVERY tx (exactly-once with the checkpoint advance).
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
                .prune_expired_token_rows(conn)
                .await
                .map_err(BusError::transport)?;
            self.svc
                .store
                .prune_expired_delete_tickets(conn)
                .await
                .map_err(BusError::transport)?;
            self.svc
                .store
                .prune_expired_browser_sessions(conn)
                .await
                .map_err(BusError::transport)?;
            self.svc
                .store
                .prune_expired_revocations(conn)
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
/// the operations and the OAuth routes). Edge exposure is
/// topology-blind: `init` contributes the generated TokenState + Auth faces to
/// `edge::EDGE_SLOT` unconditionally, and `app::run` installs them iff this process
/// serves an internal QUIC edge — the module never knows. The Auth face's gated
/// `register` self-rejects at the service level when `ACCOUNTS_OPEN_REGISTRATION` is
/// off — the impl guard is the SINGLE gate for every exposure path (HTTP op, player
/// QUIC, mTLS edge), so the trust model cannot diverge.
pub struct Accounts {
    svc: OnceLock<Arc<Service>>,
    web_limiter: OnceLock<Arc<httpmw::IpLimiter>>,
    web_limiter_reaper: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    signing_kek: Arc<OnceLock<signing_keys::SigningKek>>,
    issuer: OnceLock<String>,
    dev_seed: OnceLock<bool>,
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
            web_limiter: OnceLock::new(),
            web_limiter_reaper: std::sync::Mutex::new(None),
            signing_kek: Arc::new(OnceLock::new()),
            issuer: OnceLock::new(),
            dev_seed: OnceLock::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_signing_kek(kek: signing_keys::SigningKek) -> Accounts {
        let accounts = Accounts::new();
        accounts.signing_kek.get_or_init(|| kek);
        accounts
    }

    /// The dev seed switch `migrate` passes on: what `init` read from `ACCOUNTS_DEV_SEED`, off
    /// when `init` never ran.
    fn dev_seed_on(&self) -> bool {
        self.dev_seed.get().copied().unwrap_or(false)
    }

    /// Creates this module's own schema and guarantees an active signing key that opens
    /// under the KEK. Idempotent. The KEK is the one register parsed, else `kek_env` (the value
    /// of `ACCOUNTS_SIGNING_KEK`, which must then be set), and it is resolved BEFORE the pool is
    /// touched so a missing key fails loudly even where no pool is wired.
    pub(crate) async fn migrate_with(
        &self,
        ctx: &Context,
        kek_env: Option<std::ffi::OsString>,
    ) -> anyhow::Result<()> {
        let kek = match self.signing_kek.get() {
            Some(kek) => kek.clone(),
            None => {
                let kek = signing_keys::required_kek(kek_env)?;
                self.signing_kek.get_or_init(|| kek).clone()
            }
        };
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("accounts requires a DB pool"))?;
        refuse_stale_codes(pool, "accounts").await?;
        Ddl::new(pool)
            .sql(SCHEMA_DDL)
            .sql(OAUTH_STATES_DDL.as_str())
            // A second, DB-level constraint on `players` itself (see the claim-coverage
            // check below): `accounts.handles` decides freeness at mint time, this index
            // backs `player_by_handle`'s lookup and still refuses a duplicate pair.
            .index(
                "accounts.accounts_handle_idx",
                "CREATE UNIQUE INDEX accounts_handle_idx \
                 ON accounts.players (lower(display_name), discriminator)",
            )
            .index(
                "accounts.identities_player_idx",
                "CREATE INDEX identities_player_idx ON accounts.identities(player_id)",
            )
            .index(
                "accounts.token_families_player_idx",
                "CREATE INDEX token_families_player_idx ON accounts.token_families(player_id)",
            )
            .index(
                "accounts.token_families_client_idx",
                "CREATE INDEX token_families_client_idx ON accounts.token_families(client_id)",
            )
            .index(
                "accounts.token_families_access_expires_idx",
                "CREATE INDEX token_families_access_expires_idx \
                 ON accounts.token_families(access_expires_at)",
            )
            .index(
                "accounts.refresh_family_idx",
                "CREATE INDEX refresh_family_idx ON accounts.refresh_tokens(family_id)",
            )
            .index(
                "accounts.refresh_player_idx",
                "CREATE INDEX refresh_player_idx ON accounts.refresh_tokens(player_id)",
            )
            .index(
                "accounts.refresh_expires_idx",
                "CREATE INDEX refresh_expires_idx ON accounts.refresh_tokens(expires_at)",
            )
            .index(
                "accounts.browser_sessions_player_idx",
                "CREATE INDEX browser_sessions_player_idx ON accounts.browser_sessions(player_id)",
            )
            .index(
                "accounts.browser_sessions_expires_idx",
                "CREATE INDEX browser_sessions_expires_idx ON accounts.browser_sessions(expires_at)",
            )
            .index(
                "accounts.oauth_states_created_idx",
                "CREATE INDEX oauth_states_created_idx ON accounts.oauth_states(created_at)",
            )
            .index(
                "accounts.oidc_codes_player_idx",
                "CREATE INDEX oidc_codes_player_idx ON accounts.oidc_codes(player_id)",
            )
            .index(
                "accounts.oidc_codes_client_idx",
                "CREATE INDEX oidc_codes_client_idx ON accounts.oidc_codes(client_id)",
            )
            .index(
                "accounts.oidc_codes_session_idx",
                "CREATE INDEX oidc_codes_session_idx ON accounts.oidc_codes(session_digest)",
            )
            .index(
                "accounts.oidc_codes_created_idx",
                "CREATE INDEX oidc_codes_created_idx ON accounts.oidc_codes(created_at)",
            )
            .index(
                "accounts.token_revocations_expires_idx",
                "CREATE INDEX token_revocations_expires_idx ON accounts.token_revocations(expires_at)",
            )
            .run()
            .await?;

        // `CREATE TABLE IF NOT EXISTS` is silent on a schema that predates
        // `accounts.handles`, leaving `accounts_handle_idx` as a SECOND uniqueness
        // authority the mint loop cannot see: a draw the claim table accepts then trips
        // 23505 on the players INSERT and aborts the caller's whole transaction.
        let unclaimed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM accounts.players p \
             WHERE NOT EXISTS (SELECT 1 FROM accounts.handles h \
                                WHERE h.display_name_lower = lower(p.display_name) \
                                  AND h.discriminator = p.discriminator)",
        )
        .fetch_one(pool)
        .await?;
        if unclaimed != 0 {
            anyhow::bail!(
                "accounts: {unclaimed} player(s) have no accounts.handles claim, so \
                 accounts_handle_idx can refuse a handle the claim table just granted and \
                 abort a register/login transaction. Remedy: DROP SCHEMA accounts CASCADE; \
                 and reboot. NOT a backfill: wipe is this phase's migration strategy, and a \
                 backfill would mint reservations for handles nothing ever claimed."
            );
        }
        refuse_stale_oauth_states(pool, "accounts").await?;
        refuse_unfamilied_tokens(pool, "accounts").await?;
        refuse_stale_clients(pool, "accounts").await?;
        signing_keys::migrate_signing_keys(pool, SIGNING_KEYS_SCHEMA, &kek).await?;
        clients::migrate_dev_seed(self.dev_seed_on(), pool).await?;
        Ok(())
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("accounts.register must run before init/start")
            .clone()
    }

    /// `init` past the env read: everything it wires, from the parsed inputs.
    pub(crate) fn init_with(
        &self,
        ctx: &Context,
        config: ProviderConfig,
        cookie_secure: bool,
        trusted: Vec<ipnet::IpNet>,
        issuer: &str,
    ) -> anyhow::Result<()> {
        let svc = self.svc();

        if svc.open_registration {
            tracing::warn!(
                "ACCOUNTS_OPEN_REGISTRATION is ON — anyone can self-register a password \
                 account at /accounts/register; keep it OFF (the default) in production"
            );
        }

        if !cookie_secure {
            tracing::warn!(
                "accounts cookies are NOT Secure (ACCOUNTS_COOKIE_SECURE=0) — dev/proof opt-out, never production"
            );
        }
        let web_limiter = login_ip_limiter();
        self.web_limiter
            .set(web_limiter.clone())
            .map_err(|_| anyhow::anyhow!("accounts.init ran twice"))?;
        let login_limiter = web::LoginLimiter::new(web_limiter, trusted);
        let mut epic_flow = None;
        if let Some(epic) = &config.epic {
            tracing::info!(jwks = %epic.jwks_url, aud = %epic.client_id, "epic provider enabled");

            // Web OAuth (authorize-code), present iff the confidential client secret
            // enabled it. These two routes are HTTP-NATIVE (a browser redirect flow
            // with an external contract) — they are NOT operations; they join the
            // hosted-browser router mounted below.
            if let Some(oauth) = &epic.oauth {
                let redirect = oauth.redirect_uri.clone();
                let flow = epic_oauth::EpicOAuth::new(
                    epic.client_id.clone(),
                    oauth.clone(),
                    epic.verifier.clone(),
                    svc.store.pool.clone(),
                    cookie_secure,
                )?;
                epic_flow = Some(Arc::new(flow));
                tracing::info!(redirect = %redirect, "epic OAuth enabled");
            }
        }
        let mut google_flow = None;
        if let Some(google) = &config.google {
            tracing::info!(
                jwks = %google.jwks_url,
                aud = %google.client_ids.join(","),
                "google provider enabled"
            );
            // Browser sign-in only, present iff GOOGLE_CLIENT_SECRET enabled it. Its start
            // is the hosted form's; its callback joins the hosted-browser router, not
            // rate-limited, as Epic's.
            if let Some(flow) = google.browser_flow(&svc.store.pool, cookie_secure)? {
                tracing::info!(
                    redirect = %flow.client().redirect_uri,
                    client = %flow.client().client_id,
                    "google OAuth enabled"
                );
                google_flow = Some(Arc::new(flow));
            }
        }
        let flows = web::Flows { epic: epic_flow, google: google_flow };
        ctx.mount(web::hosted_browser_router(svc.clone(), cookie_secure, login_limiter, flows)?);
        ctx.mount(discovery::router(issuer, svc.tokens.keys()));
        svc.providers
            .set(Arc::new(config.providers(&svc.store.pool)))
            .map_err(|_| anyhow::anyhow!("accounts.init ran twice"))?;

        // Player operations: the generated Auth OpSets, ALL contributed
        // unconditionally — the gating lives at the impl (register/login → NotFound
        // when dev auth is off, loginFederated → Unavailable for a known but
        // unconfigured provider),
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

        let provider: Arc<dyn controlplane::Provider> =
            Arc::new(admin::AccountsAdmin::new(svc.clone()));
        ctx.contribute(controlplane::PROVIDER_SLOT, provider);

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer gateway pulls the token state / fronts the auth ops over QUIC); in the
        // monolith it is never applied. Own glue (rule 5): the generated
        // register_server faces live in `accountsrpc`.
        //
        // The whole Auth trait face is registered (the generated `register_server`
        // installs all methods at once), but the TRUST GATES live in the impl so the
        // edge face matches the HTTP ops exactly: `register` self-rejects with
        // NotFound when `open_registration` is off, and `login_federated` answers
        // `Unavailable` for a provider this deployment did not configure. `me` and
        // `token_state` stay unconditional — the gateway needs them.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                accountsrpc::token_state_rpc::register_server(server, svc.clone());
                accountsrpc::directory_rpc::register_server(server, svc.clone());
                accountsrpc::auth_rpc::register_server(server, svc);
            }),
        );

        // Routing-as-data SERVE side (D1.5b): this module's `#[http]` op manifest as
        // pure DATA, contributed UNCONDITIONALLY (topology-blind). `app::run` concats
        // every module's manifest and serves the union under the ONE reserved
        // `__describe` op iff this process serves an edge. `auth_rpc` carries the
        // register/login/loginFederated/me HTTP ops; `token_state_rpc` and `directory_rpc`
        // are wire-only (empty `describe()`) — concatenated so a future `#[http]` op on
        // any of them flows through.
        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            opsapi::DescribeManifest::concat([
                accountsrpc::auth_rpc::describe(),
                accountsrpc::token_state_rpc::describe(),
                accountsrpc::directory_rpc::describe(),
            ]),
        );
        Ok(())
    }
}

#[async_trait]
impl Module for Accounts {
    fn name(&self) -> &str {
        "accounts"
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service and offers it
    /// under all three capability keys — `accounts.token_state` (the gateway's verifier
    /// resolves it), `accounts.auth` and `accounts.directory` — so a dependent's
    /// `require` resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let issuer = signing_keys::oidc_issuer(std::env::var_os(signing_keys::ISSUER_ENV))?;
        let issuer = self.issuer.get_or_init(|| issuer).clone();
        if let Some(kek) = signing_keys::kek_from(std::env::var_os(signing_keys::KEK_ENV))? {
            self.signing_kek.get_or_init(|| kek);
        }
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("accounts requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service {
            store: Store { pool: pool.clone() },
            bus: ctx.bus().clone(),
            open_registration: env_bool("ACCOUNTS_OPEN_REGISTRATION", false),
            providers: OnceLock::new(),
            // Pure construction (no I/O): the argon RAM cap, the login admission
            // bound and the real verifier — admin's shapes.
            argon_permits: Arc::new(Semaphore::new(2)),
            login_slots: Arc::new(Semaphore::new(32)),
            verifier: Arc::new(ArgonVerifier),
            tokens: jwt::Tokens::new(pool, SIGNING_KEYS_SCHEMA, issuer, self.signing_kek.clone()),
        });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("accounts.register ran twice"))?;

        ctx.registry().provide::<dyn accountsapi::TokenState>(
            registry::key("accounts", "token_state"),
            svc.clone(),
        );
        ctx.registry()
            .provide::<dyn accountsapi::Auth>(registry::key("accounts", "auth"), svc.clone());
        ctx.registry().provide::<dyn accountsapi::Directory>(
            registry::key("accounts", "directory"),
            svc,
        );
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        self.migrate_with(ctx, std::env::var_os(signing_keys::KEK_ENV)).await
    }

    /// Only wires up — no I/O (#8). Reads the env gates, parses and validates the
    /// credential providers (verifier construction is pure — the JWKS fetch is
    /// LAZY — so a malformed value can only be caught here), mounts the OAuth
    /// browser routes and the OIDC discovery and JWKS, contributes the player
    /// operations (all unconditional — the dev/epic gating lives at the impl), and the
    /// generated TokenState, Directory and Auth RPC faces to the edge slot.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        // Federated credential providers, parsed and VALIDATED by the one typed
        // authority. A present-but-malformed provider is an `Err` here and fails
        // startup — the verifiers construct without I/O (#8), so nothing downstream
        // can reject a bad endpoint; before this it silently enabled a provider that
        // answered 503 on every token forever.
        let config = ProviderConfig::from_env()?;
        let cookie_secure = web::cookie_secure_from_env();
        let trusted = httpmw::parse_cidrs(&std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default())
            .map_err(|e| anyhow::anyhow!("accounts: parse TRUSTED_PROXY_CIDRS: {e}"))?;
        let issuer = self
            .issuer
            .get()
            .ok_or_else(|| anyhow::anyhow!("accounts.register must run before init"))?;
        if *self.dev_seed.get_or_init(|| clients::dev_seed_from(std::env::var_os(clients::DEV_SEED_ENV))) {
            tracing::warn!(
                "ACCOUNTS_DEV_SEED is ON — inserting the well-known client `{}` if absent; \
                 never enable it in production, where clients come from the admin portal",
                accountsapi::DEV_GAME_CLIENT_ID
            );
        }
        self.init_with(ctx, config, cookie_secure, trusted, issuer)
    }

    /// First I/O/CPU (#8): forces the argon2 `DUMMY_HASH` LazyLock here, on a
    /// `spawn_blocking` thread, so the first unknown-user login never pays the
    /// 64 MiB argon2id init cost on an async Tokio worker.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let mut conn = self.svc().store.pool.acquire().await?;
        self.svc().tokens.check_token_size(&mut conn).await?;
        drop(conn);
        tokio::task::spawn_blocking(|| {
            std::sync::LazyLock::force(&DUMMY_HASH);
        })
        .await?;
        let limiter = self
            .web_limiter
            .get()
            .ok_or_else(|| anyhow::anyhow!("accounts.init must run before start"))?;
        let mut reaper = self
            .web_limiter_reaper
            .lock()
            .map_err(|_| anyhow::anyhow!("accounts limiter reaper lock poisoned"))?;
        if reaper.is_none() {
            *reaper = Some(limiter.spawn_eviction_task());
        }
        Ok(())
    }

    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        let reaper = self
            .web_limiter_reaper
            .lock()
            .map_err(|_| anyhow::anyhow!("accounts limiter reaper lock poisoned"))?
            .take();
        let Some(reaper) = reaper else {
            return Ok(());
        };
        reaper.abort();
        match reaper.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(anyhow::anyhow!("accounts limiter reaper failed: {error}")),
        }
    }
}

/// The one per-client-IP sign-in bucket the hosted form and the Epic start share, built from
/// the rate and burst `accountsapi` publishes for a harness timing it.
pub(crate) fn login_ip_limiter() -> Arc<httpmw::IpLimiter> {
    httpmw::IpLimiter::new("accounts_login", accountsapi::LOGIN_LIMIT_PER_SEC, accountsapi::LOGIN_LIMIT_BURST)
}

/// Test-only: exposes this module's argon2 cost parameters so `cmd/server`'s
/// cross-module parity test can assert accounts' and admin's security-cost twins
/// never drift silently.
pub fn argon2_params_for_parity_test() -> (u32, u32, u32, usize) {
    password::argon2_params()
}

/// Truthiness mirrors the repo's `envBool` (`"1"`/`"true"`/`"on"`, case-insensitive);
/// unset/empty returns `default`.
fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => truthy(&v),
        _ => default,
    }
}

fn truthy(v: &str) -> bool {
    v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
}

// ============================================================================
// Tests. Unit tests (argon2, tokens, validation) need no DB; the OIDC/OAuth tests
// mint their own JWTs against a LOCAL JWKS fixture (no live Epic); integration
// tests target the local Postgres (the test DB) through `testdb`, which FAILS the run
// when it is unreachable. In-crate so they can drive the private `Service`/`Store`.
// ============================================================================
#[cfg(test)]
mod oidc_tests;
#[cfg(test)]
mod providers_tests;
#[cfg(test)]
mod tests;

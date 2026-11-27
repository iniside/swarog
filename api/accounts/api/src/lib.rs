//! `accountsapi` — the accounts module's PURE, transport-free capability contract
//! (port of Go's `api/accounts/accountsapi`). It declares the capabilities accounts
//! exposes and applies `#[rpc(prefix = "accounts")]` to each wire capability so
//! the transport-FREE surface (per-method wire envelopes, `METHOD_*` consts, and —
//! for `#[http]` methods — `operations()`/`route_bindings()`) is GENERATED into
//! child `*_rpc` modules. The edge-dependent glue (`Client`, `register_server`,
//! `provide_remote`) lives in the sibling `accountsrpc` crate, which expands this
//! crate's metadata-callback macros (`accounts_token_state_meta!` /
//! `accounts_auth_meta!` / `accounts_directory_meta!`) — so THIS crate never depends
//! on `edge`.
//!
//! The gateway's verifier imports this crate ONLY to name `dyn TokenState` for
//! `registry::require` (rule 4); it never imports the `accounts` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Maximum accepted opaque token size in bytes: the refresh token [`Auth::refresh`] rotates
/// and the hosted pages' browser cookie (43-byte base64url). The wider cap leaves format
/// headroom while bounding lookup work from attacker-controlled input.
pub const MAX_OPAQUE_TOKEN_BYTES: usize = 128;

/// Maximum accepted `display_name` size in bytes — the cap accounts' registration
/// guard enforces, published here so a consumer sizing a handle input cannot drift
/// from what the register/link handlers actually accept.
pub const MAX_DISPLAY_NAME_BYTES: usize = 128;

/// Maximum accepted handle size in bytes: a display name, `'#'`, and the four-digit
/// discriminator.
pub const MAX_HANDLE_BYTES: usize = MAX_DISPLAY_NAME_BYTES + 5;

/// Maximum number of ids one [`Directory::players_by_id`] call may carry — the bound
/// on the work a single batched lookup can ask of the store.
pub const MAX_LOOKUP_IDS: usize = 256;

/// The per-client-IP sign-in admission rate, in tokens a second: ONE bucket per client
/// IP, shared by the hosted sign-in form and `POST /accounts/epic/start`. Accounts builds
/// its limiter from this and [`LOGIN_LIMIT_BURST`]; a harness timing that bucket reads
/// the same values instead of restating them.
pub const LOGIN_LIMIT_PER_SEC: f64 = 5.0;

/// The burst of the per-client-IP sign-in bucket [`LOGIN_LIMIT_PER_SEC`] refills: the
/// sign-in attempts (hosted form and Epic start together) one client IP may make back
/// to back before accounts refuses it.
pub const LOGIN_LIMIT_BURST: u32 = 20;

/// The monolith's own front door, the origin of accounts' default OIDC issuer and of every
/// provider's default redirect URI.
#[macro_export]
macro_rules! default_front_origin {
    () => {
        "http://localhost:8080"
    };
}

/// The OIDC issuer accounts publishes when `ACCOUNTS_OIDC_ISSUER` is unset: the monolith's
/// own front door. A launcher whose accounts sits behind another port sets the variable.
pub const DEFAULT_OIDC_ISSUER: &str = concat!(crate::default_front_origin!(), "/accounts/oidc");

/// The audience of a token issued for the Swarog game API: the audience of the dev client
/// and of every client the admin portal creates.
pub const SWAROG_AUDIENCE: &str = "swarog";

/// The client the explicit-only `ACCOUNTS_DEV_SEED` inserts when absent on a dev or proof
/// boot, listing the password, guest and refresh-token grants and the [`SWAROG_AUDIENCE`].
pub const DEV_GAME_CLIENT_ID: &str = "dev-game";

/// Maximum accepted OIDC client id size in bytes, checked before any lookup. A generated id
/// is `c_` and 22 base64url characters.
pub const MAX_CLIENT_ID_BYTES: usize = 64;

/// Maximum accepted access token (JWT) size in bytes. accounts fails to boot when a token
/// with its configured issuer and the longest claims would be larger, and the portal refuses
/// a client whose longest token would be.
pub const MAX_ACCESS_TOKEN_BYTES: usize = 4096;

/// The JOSE `typ` of an access token (RFC 9068): the accounts verifier and the gateway's
/// both refuse a token whose header says anything else, so another JWT signed by the same
/// key is never taken for an access token.
pub const ACCESS_TOKEN_TYP: &str = "at+jwt";

/// An access token's lifetime: `exp - iat`.
pub const ACCESS_TTL_MINUTES: u64 = 15;

/// How long past `exp` a verifier still accepts an access token, for clock skew between
/// accounts and the verifying process.
pub const ACCESS_EXP_LEEWAY: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a revocation outlives the last token it can refuse: it expires
/// [`ACCESS_TTL_MINUTES`] + [`ACCESS_EXP_LEEWAY`] + this after it was written.
pub const REVOCATION_SKEW_MARGIN: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the gateway's verifier pulls `token_state` for a fresh snapshot, so a revocation
/// committed at accounts refuses its tokens at the gateway from the first pull that starts after
/// the commit.
pub const TOKEN_STATE_PULL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// The grant types an OIDC client lists: the ways a token may be minted for it.
pub mod grant_types {
    /// Email and password register and login.
    pub const PASSWORD: &str = "password";
    /// Guest creation and a returning guest's login.
    pub const GUEST: &str = "guest";
    /// The OAuth authorization code with PKCE, redeemed at the OIDC token endpoint.
    pub const AUTHORIZATION_CODE: &str = "authorization_code";
    /// Refresh-token rotation.
    pub const REFRESH_TOKEN: &str = "refresh_token";
    /// Every grant type, in the order a client stores and the portal offers them.
    pub const ALL: [&str; 4] = [PASSWORD, GUEST, AUTHORIZATION_CODE, REFRESH_TOKEN];
}

/// The OIDC scopes a client may list and an authorization request may ask for.
pub mod scopes {
    /// The OpenID Connect scope.
    pub const OPENID: &str = "openid";
    /// Accepted; changes nothing a client is issued.
    pub const PROFILE: &str = "profile";
    /// Accepted; changes nothing a client is issued.
    pub const OFFLINE_ACCESS: &str = "offline_access";
    /// Every scope, in the order a client stores and the portal offers them.
    pub const ALL: [&str; 3] = [OPENID, PROFILE, OFFLINE_ACCESS];
}

/// Epic's whole environment surface: every variable accounts' provider parse reads for
/// epic. Presence of ANY of these means the operator is configuring epic, including the
/// browser-flow keys, so accounts rejects a malformed browser-flow endpoint even when
/// nothing else about epic is set. A harness that forwards provider configuration keeps
/// its own copy of these keys and pins that copy to this list in a test.
pub const EPIC_ENV_KEYS: &[&str] = &[
    "EPIC_CLIENT_ID",
    "EPIC_JWKS_URL",
    "EPIC_ISSUER_PREFIX",
    "EPIC_CLIENT_SECRET",
    "EPIC_REDIRECT_URI",
    "EPIC_AUTHORIZE_URL",
    "EPIC_TOKEN_URL",
];

/// Google's whole environment surface, read the way [`EPIC_ENV_KEYS`] is: presence of
/// ANY of these means the operator is configuring google, including the browser-flow keys.
pub const GOOGLE_ENV_KEYS: &[&str] = &[
    "GOOGLE_CLIENT_IDS",
    "GOOGLE_JWKS_URL",
    "GOOGLE_CLIENT_SECRET",
    "GOOGLE_OAUTH_CLIENT_ID",
    "GOOGLE_REDIRECT_URI",
    "GOOGLE_AUTHORIZE_URL",
    "GOOGLE_TOKEN_URL",
];

/// The result of a successful register/login/refresh: the caller's product-scoped
/// `player_id`, the short-lived opaque bearer token minted for it, and the refresh
/// token that renews it. The serde field names are the public HTTP response shape
/// (`{player_id, token, refresh_token, access_expires_in_secs}`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub player_id: String,
    pub token: String,
    /// The single-use refresh token: presenting it to [`Auth::refresh`] consumes it
    /// and yields its successor. It outlives `token` and belongs to a family whose
    /// expiry a rotation never extends. Empty when the client the login named does not
    /// hold the `refresh_token` grant ([`grant_types::REFRESH_TOKEN`]): that login is
    /// over once `token` expires.
    pub refresh_token: String,
    /// The lifetime of `token` — far shorter than the refresh token's, which is the
    /// point of the split.
    pub access_expires_in_secs: i64,
}

/// What [`Auth::create_guest`] returns: a provisioned player, its session, and the
/// device ticket — the ONLY time the guest secret is ever readable. Nothing can
/// re-derive it afterwards: only its digest is stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestSession {
    pub player_id: String,
    pub token: String,
    /// The device's refresh token, rotated through [`Auth::refresh`] exactly like a
    /// [`Session`]'s, and empty under the same rule: when the client does not hold the
    /// `refresh_token` grant. It matters most here: a guest is the longest-lived client
    /// class and the only one that cannot re-authenticate from an external identity
    /// provider; without it the device logs back in with `device_secret`.
    pub refresh_token: String,
    /// The lifetime of `token`, not of the refresh family behind it.
    pub access_expires_in_secs: i64,
    /// The `"<subject>.<secret>"` ticket the device stores and replays through
    /// `login_federated("guest", …)`. Revealed exactly once, here.
    pub device_secret: String,
}

/// The confirmation ticket [`Auth::begin_delete`] mints and [`Auth::delete_account`]
/// consumes. `expires_at` is RFC3339 (`"2026-01-02T15:04:05Z"`), so a client can show
/// the user how long the confirmation stays valid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteTicket {
    pub ticket: String,
    pub expires_at: String,
}

/// The record of a completed deletion: the player that no longer exists, the handle now
/// permanently reserved against it, and when it happened (RFC3339). It outlives the
/// player row — it is what an internal-edge replay is answered from, and what an
/// operator reads afterwards.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteReceipt {
    pub player_id: String,
    pub handle: String,
    pub deleted_at: String,
}

/// One credential mapping `(provider, subject) → player`. Go named this `Identity`;
/// renamed here so it can never be confused with the macro's leading
/// `opsapi::Identity` caller-identity convention. Serde field names are Go's JSON
/// tags, unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRef {
    pub provider: String,
    pub subject: String,
}

/// The single return of [`Auth::me`]: the caller's own player plus the identities
/// list. `handle` is the ONLY front-door read of the caller's own `"Name#1234"` —
/// [`Directory`] is wire-only, so without this field a player could be resolved by a
/// handle it had no way to learn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeView {
    pub player_id: String,
    pub display_name: String,
    pub handle: String,
    pub identities: Vec<IdentityRef>,
}

/// One player as the rest of the backend sees it: the product-scoped `player_id`, the
/// chosen `display_name`, the globally unique `handle` (`"Name#1234"` — a display name
/// alone is NOT unique) and, as an RFC3339 instant, when the player's longest-lived
/// live session expires. `online_until` is EMPTY when no session is live; it is a
/// session fact, not socket presence — nothing here observes a connected client.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerSummary {
    pub player_id: String,
    pub display_name: String,
    pub handle: String,
    pub online_until: String,
}

/// Resolves players for a consumer that holds ids or a handle — the capability a
/// social feature renders its lists and validates an invite target through. WIRE-ONLY
/// on purpose: no `#[http]` on either method, so a player can never walk the directory
/// through the front door; a consuming module decides what its own caller may ask for.
/// Both methods are pure reads, hence `#[retry_safe]`.
#[rpc(prefix = "accounts")]
#[async_trait]
pub trait Directory: Send + Sync {
    /// The summaries for `ids`, in ONE batched lookup. An id that names no player —
    /// including one that is not a canonical uuid — is OMITTED from the result: the
    /// answer is a short vector, never an error, so a caller holding a stale id still
    /// renders the rest of its list. Order does not follow `ids`. More than
    /// [`MAX_LOOKUP_IDS`] ids is `Invalid` (400).
    #[retry_safe]
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error>;

    /// The player whose `"Name#1234"` handle matches, case-insensitively on the name
    /// half. `Ok(None)` is a genuine miss — an unknown handle and a malformed one are
    /// the same answer, so the method is not a grammar oracle. Over
    /// [`MAX_HANDLE_BYTES`] is `Invalid` (400).
    #[retry_safe]
    async fn find_by_handle(&self, handle: String) -> Result<Option<PlayerSummary>, Error>;
}

/// The public half of an ES256 signing key as a JWK (RFC 7517); it has no member a private
/// key could fill.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
    pub kid: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub alg: String,
}

/// A player or client cut-off: it refuses that subject's access tokens whose `iat` is at
/// or before `revoked_at_unix`, the revocation time in Unix seconds rounded up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    pub id: String,
    pub revoked_at_unix: i64,
}

/// What a verifier needs to check a player access token without calling accounts per
/// request. Every list holds only revocations that can still refuse an unexpired token.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenStateSnapshot {
    /// The `iss` of every access token.
    pub issuer: String,
    /// The signing keys a token's `kid` may name.
    pub keys: Vec<PublicJwk>,
    /// Token families (`sid`) whose every token is refused.
    pub revoked_families: Vec<String>,
    /// Cut-offs by player (`sub`).
    pub revoked_players: Vec<Revoked>,
    /// Cut-offs by client (`client_id`).
    pub revoked_clients: Vec<Revoked>,
}

/// The verification state of player access tokens, which a verifier pulls and refreshes.
/// WIRE-ONLY: no leading `Identity` and no `#[http]`; it rides the internal mTLS edge.
#[rpc(prefix = "accounts")]
#[async_trait]
pub trait TokenState: Send + Sync {
    #[retry_safe]
    async fn token_state(&self) -> Result<TokenStateSnapshot, Error>;
}

/// The accounts module's player-facing capability: the operations that establish or
/// read a player identity. `register`/`login`/`login_federated`/`create_guest`/`refresh`
/// are `auth = "none"` (they CREATE or RENEW the session, so they take no caller
/// identity — `refresh` authenticates by the refresh token in its body); `me` and
/// `link` are
/// `auth = "player"` — they take their caller identity as the leading `Identity` param
/// (injected by the gateway after bearer verification), NEVER a body field. The
/// `body_names` remap keeps Go's public body key `displayName` byte-identical.
///
/// Every op that mints a session carries the `clientId` of the program the tokens are
/// for. An id longer than [`MAX_CLIENT_ID_BYTES`], an unknown client, or a client that
/// does not list the op's grant ([`grant_types`]) is one `Unauthorized` (401): register
/// and login need `password`, `create_guest` and a `"guest"` federated login need
/// `guest`, `refresh` needs the family's client to list `refresh_token`, and any other
/// federated provider needs only an existing client.
#[rpc(prefix = "accounts")]
#[async_trait]
pub trait Auth: Send + Sync {
    /// Password self-registration: creates a player + `password` identity, emits
    /// `player.registered` (durably, inside the store tx), mints a session. `NotFound`
    /// (404) unless the deployment opened registration; missing email/password →
    /// `Invalid` (400); an email already registered, compared trimmed and
    /// ASCII-case-insensitively → `Conflict` (409). 201.
    #[http(verb = "POST", path = "/accounts/register", auth = "none", success = 201, body_names(display_name = "displayName", client_id = "clientId"))]
    async fn register(
        &self,
        email: String,
        password: String,
        display_name: String,
        client_id: String,
    ) -> Result<Session, Error>;

    /// Password login; the email is compared trimmed and ASCII-case-insensitively. Bad
    /// credentials — an unknown email or a wrong password, deliberately
    /// indistinguishable so the endpoint does not leak which emails exist — are
    /// `Unauthorized` (401). 200.
    #[http(verb = "POST", path = "/accounts/login", auth = "none", success = 200, body_names(client_id = "clientId"))]
    async fn login(&self, email: String, password: String, client_id: String) -> Result<Session, Error>;

    /// Federated (external identity provider) login: verifies `credential` with the
    /// named `provider` and logs the player in, provisioning on first sight (implicit
    /// registration, emitting `player.registered` then). An over-long provider name
    /// (checked first, before the name is ever used as a lookup key), a provider name
    /// this build cannot verify, an empty or over-long credential → `Invalid` (400); a
    /// provider this build can verify but this deployment did not configure, or an
    /// identity-provider outage → `Unavailable` (503); a rejected credential →
    /// `Unauthorized` (401). 200.
    #[http(verb = "POST", path = "/accounts/login/federated", auth = "none", success = 200, body_names(client_id = "clientId"))]
    async fn login_federated(&self, provider: String, credential: String, client_id: String) -> Result<Session, Error>;

    /// Mints a brand-new guest player: provisions the player + its `guest` identity,
    /// emits `player.registered` durably in the same transaction, mints a session and
    /// reveals the device ticket EXACTLY ONCE (only its SHA-256 digest is stored, so
    /// no later read can return it). Takes only the client — a guest has nothing to
    /// present yet; the returning device replays the ticket through
    /// [`Auth::login_federated`] under the `"guest"` provider. 201.
    #[http(verb = "POST", path = "/accounts/guest", auth = "none", success = 201, body_names(client_id = "clientId"))]
    async fn create_guest(&self, client_id: String) -> Result<GuestSession, Error>;

    /// Rotates a refresh token: consumes the presented one and answers with its
    /// successor plus a freshly minted access token. Unknown, expired, and replayed
    /// tokens are one indistinguishable `Unauthorized` (401) — a replay outside the
    /// grace window ALSO revokes the whole token family (every access session and
    /// refresh token descended from that login), since a consumed token presented
    /// late is a stolen credential. 200.
    #[http(verb = "POST", path = "/accounts/refresh", auth = "none", success = 200)]
    async fn refresh(&self, refresh_token: String) -> Result<Session, Error>;

    /// The caller's own player + identities (identity injected by the gateway after
    /// bearer verification — the AuthPlayer trust boundary). 200.
    #[http(verb = "GET", path = "/accounts/me", auth = "player", success = 200)]
    #[retry_safe]
    async fn me(&self, identity: Identity) -> Result<MeView, Error>;

    /// Attaches a second credential to the CALLING player (identity injected by the
    /// gateway after bearer verification): the credential is verified through the same
    /// provider registry, guard order and per-provider byte caps as
    /// [`Auth::login_federated`], so the same 400/401/503 answers apply. Re-linking an
    /// identity the caller already owns is an idempotent success; an identity owned by
    /// ANOTHER player is `Conflict` (409) — accounts are never merged. A guest player
    /// gaining its first non-guest identity emits `player.promoted` durably, in the
    /// transaction that writes the identity. Answers with the caller's refreshed
    /// view. 200.
    #[http(verb = "POST", path = "/accounts/link", auth = "player", success = 200)]
    async fn link(&self, identity: Identity, provider: String, credential: String) -> Result<MeView, Error>;

    /// Mints the confirmation ticket [`Auth::delete_account`] requires — the second
    /// step that keeps a single stray POST from erasing a player. The ticket is
    /// server-minted, short-lived, and UPSERTED: a player that already holds a live
    /// ticket gets that SAME ticket back instead of a second one, so a client repeating
    /// this call can never invalidate the ticket it already showed its user. Not
    /// `#[retry_safe]` — minting is a mutation; the upsert is what makes a repeat safe.
    /// 200.
    #[http(verb = "POST", path = "/accounts/delete/begin", auth = "player", success = 200)]
    async fn begin_delete(&self, identity: Identity) -> Result<DeleteTicket, Error>;

    /// Erases the CALLING player: the ticket is consumed, its handle is reserved
    /// permanently (it can never be minted again), a receipt is written, a player revocation
    /// is written and its token families deleted, the player row is deleted — cascading its
    /// identities — and `player.deleted` is appended, ALL in one transaction. An unknown
    /// ticket, an expired one, and one belonging to another player are the same `NotFound` (404),
    /// never `Forbidden` — a 403 would confirm the ticket exists.
    ///
    /// `#[retry_safe]` covers exactly one replay: the gateway→accounts **internal-edge**
    /// replay after a reconnect (`opsapi::RetryMode::OnceAfterReconnect`), which the
    /// receipt answers with the original success instead of re-executing. It does NOT
    /// cover a client re-POST: once the gateway's next `token_state` pull carries the player
    /// revocation, its verifier answers 401 before the request reaches this method. A client
    /// learns the outcome from the first response or not at all. 200.
    #[http(verb = "POST", path = "/accounts/delete", auth = "player", success = 200)]
    #[retry_safe]
    async fn delete_account(&self, identity: Identity, ticket: String) -> Result<DeleteReceipt, Error>;
}


//! `accountsapi` — the accounts module's PURE, transport-free capability contract
//! (port of Go's `api/accounts/accountsapi`). It declares the capabilities accounts
//! exposes and applies `#[rpc(prefix = "accounts")]` to each wire capability so
//! the transport-FREE surface (per-method wire envelopes, `METHOD_*` consts, and —
//! for `#[http]` methods — `operations()`/`route_bindings()`) is GENERATED into
//! child `*_rpc` modules. The edge-dependent glue (`Client`, `register_server`,
//! `provide_remote`) lives in the sibling `accountsrpc` crate, which expands this
//! crate's metadata-callback macros (`accounts_sessions_meta!` /
//! `accounts_auth_meta!` / `accounts_directory_meta!`) — so THIS crate never depends
//! on `edge`.
//!
//! The gateway's verifier adapter imports this crate ONLY to name `dyn Sessions`
//! for `registry::require` (rule 4); it never imports the `accounts` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Maximum accepted opaque token size in bytes, for BOTH token kinds accounts mints:
/// the access token a bearer presents and the refresh token [`Auth::refresh`] rotates
/// (one mint function, 43-byte base64url). The wider cap leaves format headroom while
/// bounding lookup and internal-RPC work from attacker-controlled input at every
/// topology's auth boundary.
pub const MAX_SESSION_TOKEN_BYTES: usize = 128;

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
    /// expiry a rotation never extends.
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
    /// [`Session`]'s. It matters most here: a guest is the longest-lived client class
    /// and the only one that cannot re-authenticate from an external identity provider.
    pub refresh_token: String,
    /// The lifetime of `token`, not of the refresh family behind it.
    pub access_expires_in_secs: i64,
    /// The `"<subject>.<secret>"` ticket the device stores and replays through
    /// `login_federated("guest", …)`. Revealed exactly once, here.
    pub device_secret: String,
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
/// list, flattened to the exact `{player_id, display_name, identities}` external
/// body Go's `MeView` (embedded `Player`) produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeView {
    pub player_id: String,
    pub display_name: String,
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

/// Resolves a bearer token to its player — the capability the gateway's auth-once
/// seam verifies sessions through. It is WIRE-ONLY: no leading `Identity` (it
/// ESTABLISHES identity, so there is none yet) and no `#[http]` (not a gateway
/// route; it rides the internal mTLS edge like `characters.ownerOf`). Returns
/// `Ok(None)` for a genuine unknown/expired token so a transport/store failure (an
/// `Err`) surfaces distinctly from an invalid session — the Rust twin of Go's
/// `(playerID, ok, err)`.
#[rpc(prefix = "accounts")]
#[async_trait]
pub trait Sessions: Send + Sync {
    #[retry_safe]
    async fn verify_session(&self, token: String) -> Result<Option<String>, Error>;
}

/// The accounts module's player-facing capability: the operations that establish or
/// read a player identity. `register`/`login`/`login_federated`/`create_guest`/`refresh`
/// are `auth = "none"` (they CREATE or RENEW the session, so they take no caller
/// identity — `refresh` authenticates by the refresh token in its body); `me` and
/// `link` are
/// `auth = "player"` — they take their caller identity as the leading `Identity` param
/// (injected by the gateway after bearer verification), NEVER a body field. The
/// `body_names` remap keeps Go's public body key `displayName` byte-identical.
#[rpc(prefix = "accounts")]
#[async_trait]
pub trait Auth: Send + Sync {
    /// dev/password self-registration: creates a player + dev identity, emits
    /// `player.registered` (durably, inside the store tx), mints a session. Missing
    /// email/password → `Invalid` (400); duplicate email → `Conflict` (409). 201.
    #[http(verb = "POST", path = "/accounts/register", auth = "none", success = 201, body_names(display_name = "displayName"))]
    async fn register(&self, email: String, password: String, display_name: String) -> Result<Session, Error>;

    /// dev/password login. Bad credentials — an unknown email or a wrong password,
    /// deliberately indistinguishable so the endpoint does not leak which emails
    /// exist — are `Unauthorized` (401). 200.
    #[http(verb = "POST", path = "/accounts/login", auth = "none", success = 200)]
    async fn login(&self, email: String, password: String) -> Result<Session, Error>;

    /// Federated (external identity provider) login: verifies `credential` with the
    /// named `provider` and logs the player in, provisioning on first sight (implicit
    /// registration, emitting `player.registered` then). An over-long provider name
    /// (checked first, before the name is ever used as a lookup key), a provider name
    /// this build cannot verify, an empty or over-long credential → `Invalid` (400); a
    /// provider this build can verify but this deployment did not configure, or an
    /// identity-provider outage → `Unavailable` (503); a rejected credential →
    /// `Unauthorized` (401). 200.
    #[http(verb = "POST", path = "/accounts/login/federated", auth = "none", success = 200)]
    async fn login_federated(&self, provider: String, credential: String) -> Result<Session, Error>;

    /// Mints a brand-new guest player: provisions the player + its `guest` identity,
    /// emits `player.registered` durably in the same transaction, mints a session and
    /// reveals the device ticket EXACTLY ONCE (only its SHA-256 digest is stored, so
    /// no later read can return it). Takes no input — a guest has nothing to present
    /// yet; the returning device replays the ticket through
    /// [`Auth::login_federated`] under the `"guest"` provider. 201.
    #[http(verb = "POST", path = "/accounts/guest", auth = "none", success = 201)]
    async fn create_guest(&self) -> Result<GuestSession, Error>;

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
}

// The admin fan-out capability now lives in the cross-cutting `adminapi::AdminData`
// `#[rpc]` trait (Step 7): the accounts `Service` implements it and exposes it on its
// edge as `admin.adminData`, so a remote admin process pulls the Players page over the
// QUIC edge. No per-domain `Admin` trait remains.

/// The admin extension POINTS accounts OWNS on its portal pages. A contributor
/// (characters, inventory) imports THIS const to target the point by id — it never
/// imports the accounts impl, and accounts never learns who extends it (the same
/// Open/Closed inversion the bus/registry seams enforce).
pub mod admin {
    use adminapi::{ExtensionKind, ExtensionPoint};

    /// The `⋯` menu on each Players-page row. Contributors add drill-down entries
    /// ("View Characters", "View Inventory"); the row `context` supplies `id` as
    /// `"player:<uuid>"` and `name` as the player's display name (so a drill-down
    /// page can show WHO it is scoped to without knowing the accounts module).
    pub const PLAYERS_ROW_MENU: ExtensionPoint = ExtensionPoint {
        id: "accounts.players.row-menu",
        kind: ExtensionKind::EntityMenu,
        context_keys: &["id", "name"],
    };
}

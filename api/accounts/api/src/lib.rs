//! `accountsapi` — the accounts module's PURE, transport-free capability contract
//! (port of Go's `api/accounts/accountsapi`). It declares the capabilities accounts
//! exposes and applies `#[rpc(prefix = "accounts")]` to the two wire capabilities so
//! the transport-FREE surface (per-method wire envelopes, `METHOD_*` consts, and —
//! for `#[http]` methods — `operations()`/`route_bindings()`) is GENERATED into
//! child `*_rpc` modules. The edge-dependent glue (`Client`, `register_server`,
//! `provide_remote`) lives in the sibling `accountsrpc` crate, which expands this
//! crate's metadata-callback macros (`accounts_sessions_meta!` /
//! `accounts_auth_meta!`) — so THIS crate never depends on `edge`.
//!
//! The gateway's verifier adapter imports this crate ONLY to name `dyn Sessions`
//! for `registry::require` (rule 4); it never imports the `accounts` impl crate.

use async_trait::async_trait;
use opsapi::{Error, Identity};
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// Maximum accepted opaque session-token size in bytes. Accounts mints 43-byte
/// base64url tokens; the wider cap leaves format headroom while bounding lookup and
/// internal-RPC work from attacker-controlled input at every topology's auth boundary.
pub const MAX_SESSION_TOKEN_BYTES: usize = 128;

/// The result of a successful register/login: the caller's product-scoped
/// `player_id` plus the opaque bearer token minted for it. The serde field names
/// are the public HTTP response shape (`{player_id, token}`), unchanged from Go.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub player_id: String,
    pub token: String,
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
/// read a player identity. `register`/`login`/`login_epic` are `auth = "none"` (they
/// CREATE the session, so they take no caller identity); `me` is `auth = "player"` —
/// it takes its caller identity as the leading `Identity` param (injected by the
/// gateway after bearer verification), NEVER a body field. The `body_names` remap
/// keeps Go's public body keys (`displayName`, `id_token`) byte-identical.
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

    /// Epic (EOS Connect / OIDC) login: verifies an `id_token` and logs the player
    /// in, provisioning on first sight (implicit registration, emitting
    /// `player.registered` then). Missing id_token → `Invalid` (400); a rejected
    /// token → `Unauthorized` (401). The public body key stays `id_token`. 200.
    #[http(verb = "POST", path = "/accounts/login/epic", auth = "none", success = 200, body_names(id_token = "id_token"))]
    async fn login_epic(&self, id_token: String) -> Result<Session, Error>;

    /// The caller's own player + identities (identity injected by the gateway after
    /// bearer verification — the AuthPlayer trust boundary). 200.
    #[http(verb = "GET", path = "/accounts/me", auth = "player", success = 200)]
    #[retry_safe]
    async fn me(&self, identity: Identity) -> Result<MeView, Error>;
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

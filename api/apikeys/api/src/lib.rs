//! `apikeysapi` — the apikeys module's PURE, transport-free capability contract. It
//! declares the single capability apikeys exposes and applies
//! `#[rpc(prefix = "apikeys")]` to it so the transport-FREE surface (per-method wire
//! envelopes and `METHOD_*` consts) is GENERATED into the child `*_rpc` module. The
//! edge-dependent glue (`Client`, `register_server`, `provide_remote`) lives in the
//! sibling `apikeysrpc` crate, which expands this crate's metadata-callback macro
//! (`apikeys_keys_meta!`) — so THIS crate never depends on `edge`.
//!
//! The gateway's key verifier adapter imports this crate ONLY to name `dyn Keys` for
//! `registry::require` (rule 4); it never imports the `apikeys` impl crate.

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One resolved API key: the client-class `name` it identifies and the `policy` that
/// governs which wire methods it may call. The policy is either the literal string
/// `full` (every method) or a comma-separated list of wire method names (e.g.
/// `accounts.login,characters.create`); the gateway evaluates it. The `key` string
/// itself is deliberately absent — a verified lookup returns only what the gateway
/// needs to authorize the request, never the secret back over the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRecord {
    pub name: String,
    pub policy: String,
}

/// Resolves an API key string to its [`KeyRecord`] — the capability the gateway's
/// per-request key check consults. It is WIRE-ONLY: no leading `Identity` (a key is a
/// client-class credential, not a player) and no `#[http]` (not a gateway route; it
/// rides the internal mTLS edge like `accounts.sessions`). Returns `Ok(None)` for a
/// genuine unknown OR revoked key so a transport/store failure (an `Err`) surfaces
/// distinctly from an invalid key — the same `(record, ok, err)` shape as
/// `accountsapi::Sessions::verify_session`.
#[rpc(prefix = "apikeys")]
#[async_trait]
pub trait Keys: Send + Sync {
    #[retry_safe]
    async fn lookup_key(&self, key: String) -> Result<Option<KeyRecord>, Error>;
}

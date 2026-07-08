//! `opsapi` — the leaf that declares the vocabulary of internal *operations* plus
//! the transport seam a generated RPC client calls through. A leaf in the strict
//! sense: it imports no module and is importable by everyone (the port of Go's
//! `opsapi/opsapi.go`).
//!
//! Two independent things live here, both foundational to the unified operation
//! transport:
//!
//!   - [`Caller`] — the minimal transport a generated RPC client (Step 5's
//!     `#[rpc]` macro) calls through. The generated client targets `Caller`, not a
//!     concrete type, so it is transport-agnostic: it composes over `edge::Client`
//!     directly AND over `remote`'s reconnecting edge conn. Depend on the
//!     capability, not the package.
//!
//!   - [`Operation`] + the three slots — the *declaration* seam. A module
//!     Contributes one [`OpSet`] per capability it exposes (into [`SLOT`] /
//!     [`BINDING_SLOT`] / [`LOCAL_SLOT`], the same `contrib` mechanism admin uses);
//!     the gateway (Step 10) reads every contribution to build its HTTP route
//!     table. A module lights up a route by contributing, never by the gateway
//!     importing it.
//!
//! ## Identity — the port of Go's `WithPlayerID` / `PlayerID`
//! Go carries a caller's VERIFIED `player_id` in `context.Context`. Rust has no
//! ambient context, so identity is modelled as an explicit [`Identity`] value
//! **threaded through the call** — set at exactly two trusted seams (the gateway
//! front-handler after bearer verification; the generated edge server adapter, from
//! the mTLS-authenticated request envelope's identity field) and read back by a
//! domain operation via [`Identity::player_id`]. It is NEVER derived from a
//! client-supplied field mid-stack; that is the whole trust boundary.
//!
//! ## Bytes, not `any`
//! Go's `Caller.Call(ctx, method, req, resp any)` serialises internally and Go's
//! in-process `LocalInvoker` crosses the call as the exact decoded structs (its D3
//! "no re-serialise on the monolith path" optimisation). This sketch is
//! deliberately **bytes-based** at every transport seam: [`Caller::call`] and
//! [`LocalInvoker`] both take/return already-encoded wire payloads and the
//! generated glue (Step 5) owns the serde around them. Local and Remote dispatch
//! the identical wire request by construction; the D3 no-re-serialise-local
//! optimisation is a documented non-goal of the sketch.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

// ---------------------------------------------------------------------------
// Identity — port of WithPlayerID / PlayerID
// ---------------------------------------------------------------------------

/// The caller's VERIFIED identity, threaded explicitly through an operation call
/// in place of Go's ambient `context.Context` player_id.
///
/// `Identity::none()` (Go: no value in ctx) is the `AuthNone` path; a
/// `Identity::player(pid)` (Go: `WithPlayerID`) is set at exactly two trusted
/// seams and read back with [`Identity::player_id`] (Go: `PlayerID`). A domain
/// operation MUST take its caller identity ONLY from here — never from an HTTP
/// header, query param, or body field, which are attacker-controlled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity(Option<String>);

impl Identity {
    /// No identity established — the `AuthNone` path (Go: an empty `context.Context`).
    pub fn none() -> Self {
        Identity(None)
    }

    /// Carries `pid` as the caller's verified player identity (Go: `WithPlayerID`).
    /// An empty string is treated as no identity, mirroring Go's `pid != ""` guard.
    pub fn player(pid: impl Into<String>) -> Self {
        let pid = pid.into();
        if pid.is_empty() {
            Identity(None)
        } else {
            Identity(Some(pid))
        }
    }

    /// The verified player_id, or `None` when no identity was established (Go:
    /// `PlayerID`, whose `ok` maps to `Some`/`None` here). An operation that
    /// requires one should return [`Error::invalid`] on `None`, never proceed with
    /// an empty player_id.
    pub fn player_id(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Caller — the transport seam
// ---------------------------------------------------------------------------

/// The minimal transport a generated RPC client calls through. `edge::Client` (and
/// later `remote`'s reconnecting conn) implement it. Bytes-based: `payload` is the
/// already-encoded wire request and the returned `Vec<u8>` is the wire response —
/// the generated client owns the serde on both sides.
///
/// `identity` carries the caller's verified player_id (`None` when the call is
/// unauthenticated, i.e. `AuthNone`); the edge client stamps it into the request
/// envelope's identity field so the peer's generated adapter can read it.
#[async_trait::async_trait]
pub trait Caller: Send + Sync {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, Error>;
}

// ---------------------------------------------------------------------------
// Status taxonomy + Error
// ---------------------------------------------------------------------------

/// The operation error taxonomy carried through a generated RPC response envelope.
/// The edge transport carries only a bare error string, which cannot distinguish a
/// 404 from a 403 from a 503; the generated response envelope carries a `Status` so
/// the gateway maps a domain failure onto the right HTTP status instead of
/// collapsing everything to 500.
///
/// `Serialize`/`Deserialize` are derived because the generated RPC response
/// envelope (Step 5's `#[rpc]` macro) carries a `Status` field INSIDE the payload
/// — the domain outcome rides in the envelope, distinct from an edge-level
/// transport failure. serde encodes each variant by its name (a self-consistent
/// Rust-only wire; both ends are macro-generated).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    /// Success; a response carrying it has no error.
    Ok,
    /// The addressed entity does not exist (→ HTTP 404).
    NotFound,
    /// The caller is not permitted (→ HTTP 403).
    Forbidden,
    /// The request was malformed or failed validation (→ HTTP 400).
    Invalid,
    /// A dependency was unreachable; retry may succeed (→ HTTP 503).
    Unavailable,
    /// An unclassified server failure (→ HTTP 500). The fallback [`status_of`]
    /// assigns to any error that is not an [`Error`].
    Internal,
    /// The request lacked valid credentials (→ HTTP 401). Distinct from
    /// [`Status::Forbidden`] (403, an authenticated caller lacking permission) and
    /// [`Status::Invalid`] (400, a malformed request).
    Unauthorized,
    /// The request conflicts with existing durable state (→ HTTP 409), e.g.
    /// registering an email already taken.
    Conflict,
}

impl Status {
    /// The HTTP status code this operation `Status` maps onto — the mapping the
    /// gateway (Step 10) applies. `Status::Ok` maps to 200, but the gateway uses
    /// the operation's declared [`Operation::success`] on the success path instead.
    pub fn http(self) -> u16 {
        match self {
            Status::Ok => 200,
            Status::NotFound => 404,
            Status::Forbidden => 403,
            Status::Invalid => 400,
            Status::Unavailable => 503,
            Status::Internal => 500,
            Status::Unauthorized => 401,
            Status::Conflict => 409,
        }
    }
}

/// A typed operation error a handler returns to select the [`Status`] that rides
/// the response envelope. Mirrors Go's `opsapi.Error{Status, Msg}`.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{msg}")]
pub struct Error {
    pub status: Status,
    pub msg: String,
}

impl Error {
    pub fn new(status: Status, msg: impl Into<String>) -> Self {
        Error {
            status,
            msg: msg.into(),
        }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::new(Status::NotFound, msg)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::new(Status::Forbidden, msg)
    }
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::new(Status::Invalid, msg)
    }
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Error::new(Status::Unavailable, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Error::new(Status::Internal, msg)
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Error::new(Status::Unauthorized, msg)
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::new(Status::Conflict, msg)
    }
}

/// Extracts the operation [`Status`] an [`Error`] maps to. The Rust [`Error`]
/// always carries a `Status` (unlike Go's `error`, where `StatusOf` falls back to
/// `Internal` for a plain error); the generated server adapter calls it to fill the
/// response envelope. A `None` (nil-error) argument is [`Status::Ok`].
pub fn status_of(err: Option<&Error>) -> Status {
    match err {
        None => Status::Ok,
        Some(e) => e.status,
    }
}

// ---------------------------------------------------------------------------
// Declaration model — Operation / HTTPBind / OpBinding / LocalOp / OpSet
// ---------------------------------------------------------------------------

/// What identity guarantee an operation needs the gateway to establish before it
/// dispatches. Declared per operation so the auth requirement lives beside the
/// route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthReq {
    /// The operation is public; the gateway dispatches without a bearer (e.g.
    /// `match/report`, login/register, leaderboard).
    None,
    /// The gateway verifies the bearer token and injects the resolved player_id, so
    /// the backend never reads a client-supplied identity.
    Player,
}

/// One internal capability a module exposes, declared as a contribution the gateway
/// reads to bind an HTTP route to an RPC method. Pure comparable data — the
/// non-comparable invoker rides its own [`LocalOp`] slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    /// The rpc method name, e.g. `"characters.create"`.
    pub method: String,
    /// HTTP verb the gateway binds, e.g. `"POST"`.
    pub verb: String,
    /// HTTP path pattern, e.g. `"/characters"` or `"/characters/{id}"`.
    pub path: String,
    /// Identity the gateway must establish before dispatch.
    pub auth: AuthReq,
    /// HTTP status the gateway writes on a [`Status::Ok`] outcome (e.g. 201/200/204).
    pub success: u16,
}

/// The per-method HTTP-surface declaration the `#[rpc]` macro (Step 5) reads to
/// GENERATE the gateway binding for a method: the verb/path/auth/success (which
/// become the [`Operation`]) plus where each method argument is sourced from — a
/// path wildcard or the request body — so the generated `decode` builds the SAME
/// wire request both backends consume. The Rust twin of a `#[http(...)]` attribute
/// / Go's `var HTTPBindings map[string]HTTPBind`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpBind {
    pub verb: String,
    pub path: String,
    pub auth: AuthReq,
    /// HTTP status on a [`Status::Ok`] outcome — a plain int (201/200/204/202).
    pub success: u16,
    /// Maps a method PARAM NAME to the path-wildcard name it is taken from, e.g.
    /// `{"character_id": "id"}` for a `delete(character_id)` bound to
    /// `"/characters/{id}"`. A param not listed here is a BODY arg.
    pub path_args: HashMap<String, String>,
    /// Overrides the external JSON key of a BODY arg where it differs from the param
    /// name, e.g. `{"item_id": "item_id"}`. An unlisted body arg uses its param name.
    pub body_names: HashMap<String, String>,
}

/// Contribution slot the gateway reads to build its route table (the [`Operation`]
/// half). A module contributes `Operation`s here; the gateway reads them. Same
/// multi-value seam as `adminapi`'s slot.
pub const SLOT: &str = "ops.operation";

/// Contribution slot pairing each [`Operation`] with its HTTP↔wire translation
/// ([`OpBinding`]). Contributed by the module in the SAME process as the operation.
pub const BINDING_SLOT: &str = "ops.binding";

/// Contribution slot for the gateway's in-process dispatch table ([`LocalOp`]).
pub const LOCAL_SLOT: &str = "ops.local";

/// The matched path-wildcard values handed to [`OpBinding::decode`], e.g.
/// `{"id": "..."}` for `"/characters/{id}"`.
pub type PathArgs = HashMap<String, String>;

/// Builds an operation's WIRE REQUEST PAYLOAD from the raw HTTP body and the matched
/// path wildcards. `body` is `None` for a request with no body. A malformed body
/// should be an [`Error`] with [`Status::Invalid`]. The returned bytes are the SAME
/// wire request both LocalBackend (which the generated invoker deserialises) and
/// RemoteBackend (which relays them over the edge) consume — Local == Remote by
/// construction.
pub type DecodeFn =
    Arc<dyn Fn(Option<&[u8]>, &PathArgs) -> Result<Vec<u8>, Error> + Send + Sync>;

/// Reduces a WIRE RESPONSE PAYLOAD (the `{status, err, <domain>}` envelope the
/// `#[rpc]` macro emits) to the EXTERNAL HTTP body plus the operation [`Status`].
/// On [`Status::Ok`] it returns the domain-only body (dropping status/err); `None`
/// body for a no-return op (204). The `Status` lets the gateway map a non-OK
/// outcome to the right HTTP status without re-deriving it.
pub type EncodeFn = Arc<dyn Fn(&[u8]) -> Result<(Option<Vec<u8>>, Status), Error> + Send + Sync>;

/// Calls an operation's provider IN-PROCESS. Given the caller [`Identity`] and the
/// WIRE REQUEST PAYLOAD, the generated invoker deserialises the request, invokes
/// the provider (which it closed over from the registry), and returns the WIRE
/// RESPONSE PAYLOAD. A dispatch/transport failure is an `Err(Error)` carrying a
/// [`Status`]. Bytes-based twin of Go's `LocalInvoker func(ctx, req, resp any)`
/// (see the crate-level "Bytes, not any" note).
pub type LocalInvoker =
    Arc<dyn Fn(Identity, Vec<u8>) -> BoxFuture<'static, Result<Vec<u8>, Error>> + Send + Sync>;

/// Pairs an operation's method name with its in-process invoker. Kept SEPARATE from
/// [`Operation`] (pure comparable data) so `Operation` stays comparable while the
/// invoker — a closure, non-comparable — rides its own [`LOCAL_SLOT`].
#[derive(Clone)]
pub struct LocalOp {
    pub method: String,
    pub invoke: LocalInvoker,
}

/// The per-operation, topology-independent glue the gateway needs to translate an
/// HTTP request into the wire request and to reduce the wire response back to an
/// HTTP body. Deliberately transport-free (no `axum`/`http` types): the gateway
/// extracts the raw body + matched wildcards and hands them here, so the SAME
/// binding drives LocalBackend and RemoteBackend — the decode/encode happen once at
/// the HTTP boundary.
///
/// Unlike Go's `OpBinding` this carries no `NewResp`: the bytes-based [`EncodeFn`]
/// deserialises the wire response envelope itself, so there is no pre-allocated
/// typed envelope for a backend to fill.
#[derive(Clone)]
pub struct OpBinding {
    pub method: String,
    pub decode: DecodeFn,
    pub encode: EncodeFn,
}

/// Bundles the three per-operation contributions the `#[rpc]` macro generates for a
/// bound method — the [`Operation`] (route/auth/success), its [`OpBinding`]
/// (decode/encode), and the [`LocalOp`] (in-process invoker) — so a module
/// contributes them in one loop. The macro emits a
/// `fn operations(impl) -> HashMap<String, OpSet>` (keyed by wire method name) the
/// module reads and contributes to the three slots, selecting which methods to
/// expose.
#[derive(Clone)]
pub struct OpSet {
    pub operation: Operation,
    pub binding: OpBinding,
    pub local: LocalOp,
}

/// The IMPL-FREE subset of an operation's gateway wiring: the static [`Operation`]
/// paired with its [`OpBinding`], with NO [`LocalOp`] — so it needs no provider impl
/// to construct because it only ever dispatches REMOTELY (over a RemoteBackend to
/// the owning peer's edge). This is what a dedicated split front-door process (which
/// hosts no module and has no `LocalOp` to bind) builds its route table from; the
/// macro emits a `fn route_bindings() -> Vec<RouteBinding>` alongside `operations`.
#[derive(Clone)]
pub struct RouteBinding {
    pub operation: Operation,
    pub binding: OpBinding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_with_player_id_and_player_id() {
        assert_eq!(Identity::none().player_id(), None);
        assert_eq!(Identity::player("p1").player_id(), Some("p1"));
        // Empty string is treated as no identity (Go's `pid != ""` guard).
        assert_eq!(Identity::player("").player_id(), None);
    }

    #[test]
    fn status_of_matches_go_semantics() {
        assert_eq!(status_of(None), Status::Ok);
        let e = Error::not_found("nope");
        assert_eq!(status_of(Some(&e)), Status::NotFound);
        assert_eq!(e.status, Status::NotFound);
    }

    #[test]
    fn status_http_mapping() {
        assert_eq!(Status::Ok.http(), 200);
        assert_eq!(Status::NotFound.http(), 404);
        assert_eq!(Status::Forbidden.http(), 403);
        assert_eq!(Status::Invalid.http(), 400);
        assert_eq!(Status::Unavailable.http(), 503);
        assert_eq!(Status::Internal.http(), 500);
        assert_eq!(Status::Unauthorized.http(), 401);
        assert_eq!(Status::Conflict.http(), 409);
    }

    #[test]
    fn error_display_is_the_message() {
        let e = Error::conflict("email taken");
        assert_eq!(e.to_string(), "email taken");
    }

    #[test]
    fn slot_names_match_go() {
        assert_eq!(SLOT, "ops.operation");
        assert_eq!(BINDING_SLOT, "ops.binding");
        assert_eq!(LOCAL_SLOT, "ops.local");
    }

    // A LocalInvoker / OpBinding round-trip proving the closure shapes are usable:
    // a bytes-in/bytes-out invoker and a decode/encode pair compose the way the
    // gateway (Step 10) will drive them.
    #[tokio::test]
    async fn opset_closures_compose() {
        let decode: DecodeFn = Arc::new(|body, path| {
            let id = path.get("id").cloned().unwrap_or_default();
            let body = body.unwrap_or(b"null");
            Ok(format!(r#"{{"id":"{id}","body":{}}}"#, std::str::from_utf8(body).unwrap()).into_bytes())
        });
        let invoke: LocalInvoker = Arc::new(|ident, req| {
            Box::pin(async move {
                // Requires an identity, mirroring an AuthPlayer op.
                let pid = ident
                    .player_id()
                    .ok_or_else(|| Error::invalid("no identity"))?;
                Ok(format!(r#"{{"status":"ok","pid":"{pid}","echo":{}}}"#, String::from_utf8(req).unwrap())
                    .into_bytes())
            })
        });
        let encode: EncodeFn = Arc::new(|resp| Ok((Some(resp.to_vec()), Status::Ok)));

        let op = OpSet {
            operation: Operation {
                method: "demo.echo".into(),
                verb: "POST".into(),
                path: "/demo/{id}".into(),
                auth: AuthReq::Player,
                success: 200,
            },
            binding: OpBinding {
                method: "demo.echo".into(),
                decode,
                encode,
            },
            local: LocalOp {
                method: "demo.echo".into(),
                invoke,
            },
        };

        let mut path = PathArgs::new();
        path.insert("id".into(), "42".into());
        let wire_req = (op.binding.decode)(Some(b"123"), &path).unwrap();
        let wire_resp = (op.local.invoke)(Identity::player("alice"), wire_req)
            .await
            .unwrap();
        let (body, status) = (op.binding.encode)(&wire_resp).unwrap();
        assert_eq!(status, Status::Ok);
        let body = String::from_utf8(body.unwrap()).unwrap();
        assert!(body.contains(r#""pid":"alice""#), "{body}");
        assert!(body.contains(r#""id":"42""#), "{body}");

        // No identity → the invoker rejects with Invalid (the AuthPlayer contract).
        let wire_req = (op.binding.decode)(Some(b"1"), &path).unwrap();
        let err = (op.local.invoke)(Identity::none(), wire_req).await.unwrap_err();
        assert_eq!(err.status, Status::Invalid);
    }
}

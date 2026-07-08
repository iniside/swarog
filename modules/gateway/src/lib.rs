//! `gateway` — the front-door lifecycle module, present in EVERY `app::run` process
//! (the monolith and each split `*-svc`). It fronts the process's op surface on TWO
//! public planes through ONE shared façade ([`FrontDoor`]):
//!
//!   - **HTTP** — one axum FALLBACK handler mounted onto the shared `Context` router,
//!     so `app` stays topology-blind (the leaf-slot seam). Port of Go's
//!     `modules/gateway` (`gateway.go` + `backend.go`).
//!   - **Player QUIC** — an [`edge::PlayerHandler`] installed on a shared
//!     [`edge::PlayerServer`] (when `main` wires one via
//!     [`Gateway::with_player_edge`]). The player speaks the wire request shape
//!     directly, so this plane skips the HTTP body/path translation but shares
//!     EVERYTHING else: the same route table, the same auth-once boundary, the same
//!     backend selection.
//!
//! For an HTTP request it does, in order:
//!   1. **Match** the request (verb + path, with `{wild}` path segments) against the
//!      `Operation`s modules contributed to `opsapi::SLOT`. No match → 404, and the
//!      fallback is invisible: only otherwise-unmatched routes reach it, so it never
//!      shadows `/healthz`/`/readyz` (added by `app`) or `POST /events` (messaging).
//!   2. **Auth-once:** for an `AuthReq::Player` op it verifies the `Authorization:
//!      Bearer <token>` header via the [`SessionVerifier`] and threads the resolved
//!      player_id as an `opsapi::Identity`. This is the SINGLE trust boundary —
//!      downstream (local invoker or peer over the edge) never re-verifies. An
//!      `AuthReq::None` op runs with `Identity::none()`.
//!   3. **Decode** the HTTP body + matched path wildcards into the wire request via
//!      the op's `OpBinding::decode`.
//!   4. **Dispatch** on the topology-correct backend (`RouteTable::dispatch`): a
//!      [`LocalBackend`] when this process holds the op's `LocalInvoker`, else a
//!      [`RemoteBackend`] dialing the owning peer (a Remote failure evicts the cached
//!      connection so the next request re-dials).
//!   5. Reduce the wire response via `OpBinding::encode` — an encode-`Err` carries the
//!      domain `Status` (→ its HTTP code); an `Ok` writes the op's declared `success`
//!      code with the domain body.
//!
//! A player-QUIC request runs the same match/auth/dispatch, minus the HTTP
//! translation — see [`FrontDoor::player_handler`] for the pinned response grammar.
//!
//! ## Lazy route table (the init-ordering sidestep)
//! Modules contribute their `OpSet`s during their own `init`, and the gateway's
//! `init` may run first. So the table is NOT built eagerly: the [`FrontDoor`] holds a
//! [`std::sync::OnceLock`] and builds the table from `ctx.contributions(...)` on the
//! FIRST request — by which time every module's `init` has run (requests only arrive
//! after `app::run` finishes Build and starts serving).

mod backend;
mod verifier;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use contrib::Slots;
use lifecycle::{Context, Module};
use opsapi::{
    AuthReq, Caller, Error, Identity, LocalInvoker, OpBinding, Operation, PathArgs, Status,
};
use serde_json::value::RawValue;

pub use backend::{LocalBackend, OperationBackend, RemoteBackend};
pub use verifier::{DevSessionVerifier, SessionVerifier};

/// Caps the request body the gateway buffers before decoding an operation, so a
/// hostile client cannot make the front-handler allocate without bound. 1 MiB is
/// comfortably above any player operation's request (matches Go's `maxBodyBytes`,
/// and `edge::MAX_PLAYER_FRAME` mirrors it on the QUIC plane).
const MAX_BODY_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The front-door module. Stateless apart from the [`SessionVerifier`] it fronts
/// auth with and the optional shared [`edge::PlayerServer`] it wires the player
/// plane onto; the route table is read lazily from the `Context` on the first
/// request.
pub struct Gateway {
    verifier: Arc<dyn SessionVerifier>,
    /// When set, the process-wide player-facing QUIC server (built by `main`, the
    /// same shared-handle pattern as `inventory::with_edge`). `init` installs the
    /// [`FrontDoor::player_handler`] on it so the process fronts players over QUIC
    /// as well as HTTP. `None` for a process with no public player port.
    player_edge: Option<Arc<Mutex<edge::PlayerServer>>>,
}

impl Gateway {
    /// A gateway using the M1 [`DevSessionVerifier`] (accepts `Bearer dev-<player_id>`).
    pub fn new() -> Self {
        Gateway::with_verifier(Arc::new(DevSessionVerifier::new()))
    }

    /// A gateway using a caller-supplied verifier — the seam Milestone 2 uses to swap
    /// in real `accounts` sessions (or an edge-backed verifier for the split front-door).
    pub fn with_verifier(verifier: Arc<dyn SessionVerifier>) -> Self {
        Gateway { verifier, player_edge: None }
    }

    /// Additionally fronts players over the shared QUIC [`edge::PlayerServer`]
    /// (builder-style, composable with [`Gateway::with_verifier`]). `main` constructs
    /// the server, hands the SAME handle here and to `app::run` (which `listen`s it
    /// after Build) — `init` installs the front handler in between, so by the time
    /// the port is open the front is wired.
    pub fn with_player_edge(mut self, shared: Arc<Mutex<edge::PlayerServer>>) -> Self {
        self.player_edge = Some(shared);
        self
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Gateway::new()
    }
}

#[async_trait::async_trait]
impl Module for Gateway {
    fn name(&self) -> &str {
        "gateway"
    }

    // No `requires`: the gateway reads opsapi SLOTS (contributions), not services.

    /// Builds ONE [`FrontDoor`] and mounts it on every plane this process fronts:
    /// the axum fallback always, and — when `main` handed a shared player server —
    /// the player-QUIC handler too. No I/O. The façade captures the slot registry +
    /// verifier and lazily builds the route table on first request.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let front_door = Arc::new(FrontDoor::new(ctx.slots().clone(), self.verifier.clone()));
        ctx.mount(front_door.router());
        if let Some(shared) = &self.player_edge {
            shared.lock().unwrap().set_handler(front_door.player_handler());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FrontDoor — the shared two-plane façade + lazy route table
// ---------------------------------------------------------------------------

/// The front-door façade both public planes dispatch through. The HTTP fallback
/// ([`FrontDoor::router`]) and the player-QUIC handler ([`FrontDoor::player_handler`])
/// hold the SAME slot registry, verifier, and once-built route table, so route
/// matching, the auth-once trust boundary, and Local/Remote backend selection are one
/// code path regardless of which transport a request arrived on — a fix on one plane
/// cannot drift from the other.
pub struct FrontDoor {
    slots: Arc<Slots>,
    verifier: Arc<dyn SessionVerifier>,
    table: OnceLock<Arc<RouteTable>>,
}

impl FrontDoor {
    /// A façade over `slots` (the contribution registry the route table is built
    /// from) fronting auth with `verifier`.
    pub fn new(slots: Arc<Slots>, verifier: Arc<dyn SessionVerifier>) -> FrontDoor {
        FrontDoor { slots, verifier, table: OnceLock::new() }
    }

    /// The route table, built from the slots on first access. By first-request time
    /// every module's `init` (where `OpSet`s are contributed) has completed.
    fn table(&self) -> &Arc<RouteTable> {
        self.table
            .get_or_init(|| Arc::new(RouteTable::build(&self.slots)))
    }

    /// Builds the axum router carrying ONLY the gateway's fallback. `Router::merge`
    /// (used by `Context::mount`) tolerates exactly one fallback across all merged
    /// routers; messaging and `app` add plain routes, so this is the sole fallback.
    pub fn router(self: &Arc<Self>) -> Router {
        let front = self.clone();
        Router::new().fallback(move |req: Request| {
            let front = front.clone();
            async move { handle(front, req).await }
        })
    }

    /// The player-plane dispatch handler, installed on an [`edge::PlayerServer`].
    ///
    /// The PINNED response grammar: every FRONT-originated domain outcome — auth
    /// failures included — returns handler `Ok(bytes)` where `bytes` is the generated
    /// response envelope `{status, err}` (the field is `err`, exactly the `#[rpc]`
    /// macro's shape — see [`front_envelope`]). The transport-level `Err` (which the
    /// player server surfaces as `ok:false`) is reserved for transport faults and is
    /// NEVER used for a domain failure, so a player client decodes ONE grammar:
    /// check `ok`, then decode the payload and check `status`.
    pub fn player_handler(self: &Arc<Self>) -> edge::PlayerHandler {
        let front = self.clone();
        Arc::new(move |method, token, payload| {
            let front = front.clone();
            Box::pin(async move { Ok(front.handle_player(method, token, payload).await) })
        })
    }

    /// One player-plane call: the same match → auth-once → dispatch the HTTP handler
    /// runs, minus the HTTP body/path translation (the player speaks the wire request
    /// shape directly, so there is no `OpBinding::decode`/`encode` on this path).
    ///
    ///   1. **Well-formedness gate:** the payload must be JSON. Without this gate,
    ///      garbage gets topology-DEPENDENT errors — a Local invoker's parse failure
    ///      answers `Invalid`, but a Remote peer's surfaces as transport
    ///      `Unavailable`: same input, 400 vs 503. Rejecting at the front pins it.
    ///   2. **Match by method** — the allow-list gate: only `#[http]`-bound ops are
    ///      in the table, so a wire-only internal method (e.g. `characters.ownerOf`)
    ///      is NotFound here even though a peer edge would serve it.
    ///   3. **Auth-once:** `token` is ATTACKER-CONTROLLED input (a claim, not an
    ///      identity — the player envelope carries no identity field by design).
    ///      For an `AuthReq::Player` op it is required and verified via the
    ///      [`SessionVerifier`]; only the VERIFIED player_id becomes the `Identity`
    ///      threaded downstream, and nothing downstream re-verifies. `AuthReq::None`
    ///      runs with `Identity::none()`.
    ///   4. **Dispatch** and return the wire response bytes VERBATIM — the domain
    ///      `Status` already rides inside the generated response envelope. A backend
    ///      `Err(opsapi::Error)` is re-serialized as the same `{status, err}` shape.
    async fn handle_player(
        &self,
        method: String,
        token: Option<String>,
        payload: Vec<u8>,
    ) -> Vec<u8> {
        // (1) Well-formedness gate — malformed JSON is Invalid at the front.
        if serde_json::from_slice::<&RawValue>(&payload).is_err() {
            return front_envelope(Status::Invalid, "malformed request payload");
        }

        let table = self.table().clone();

        // (2) Method match — miss means not player-reachable (the allow-list gate).
        let Some(route) = table.find_by_method(&method) else {
            return front_envelope(Status::NotFound, "unknown operation");
        };

        // (3) Auth-once: the single trust boundary, same rule as the HTTP plane.
        let identity = match route.op.auth {
            AuthReq::Player => {
                let Some(token) = token else {
                    return front_envelope(Status::Unauthorized, "unauthorized");
                };
                match self.verifier.verify(&token).await {
                    Some(pid) => Identity::player(pid),
                    None => return front_envelope(Status::Unauthorized, "unauthorized"),
                }
            }
            AuthReq::None => Identity::none(),
        };

        // (4) Dispatch; the wire response IS the player response (envelope included).
        match table.dispatch(&route.op, identity, payload).await {
            Ok(bytes) => bytes,
            Err(e) => front_envelope(e.status, &e.msg),
        }
    }
}

/// A front-originated response envelope matching EXACTLY the shape the `#[rpc]`
/// macro generates (`tools/rpc-macro`'s `gen_response_struct`): the field is **`err`**
/// (not `error`), an empty `err` is omitted (`skip_serializing_if`), and [`Status`]
/// serializes as its bare variant name. Emitting the macro's own grammar means a
/// player client decodes ONE envelope shape whether the outcome came from the
/// provider or from the front. (Value-typed responses also carry a `#[serde(default)]`
/// `value` field, so its absence here still parses on the generated client.)
#[derive(serde::Serialize)]
struct FrontEnvelope<'a> {
    status: Status,
    #[serde(skip_serializing_if = "str::is_empty")]
    err: &'a str,
}

/// Serializes a front-originated domain outcome as the generated `{status, err}`
/// envelope (see [`FrontEnvelope`]).
fn front_envelope(status: Status, err: &str) -> Vec<u8> {
    serde_json::to_vec(&FrontEnvelope { status, err })
        .expect("front envelope serialization cannot fail")
}

/// One matchable route: the `Operation`, its HTTP↔wire `OpBinding`, and the parsed
/// path pattern (so matching + wildcard extraction avoid re-parsing per request).
struct Route {
    op: Operation,
    binding: OpBinding,
    pattern: Vec<Seg>,
}

/// A parsed path segment: a literal or a `{name}` wildcard capturing one segment.
enum Seg {
    Lit(String),
    Wild(String),
}

/// The gateway's operation route table + backend material, built once from the slots.
struct RouteTable {
    routes: Vec<Route>,
    /// In-process invokers (method → invoker). Presence decides Local vs Remote.
    invokers: Arc<HashMap<String, LocalInvoker>>,
    /// Lazily-dialed edge clients per provider, shared across requests to that peer.
    /// An entry is EVICTED when a call through it fails (see [`RouteTable::dispatch`]).
    remotes: tokio::sync::Mutex<HashMap<String, Arc<dyn Caller>>>,
}

impl RouteTable {
    /// Reads the three opsapi slots and assembles the table. An `Operation` with no
    /// paired `OpBinding` is a provider wiring bug and is skipped rather than bound to
    /// an undecodable route (mirrors Go's `buildOpsMux`).
    fn build(slots: &Slots) -> RouteTable {
        let operations: Vec<Operation> = slots.contributions(opsapi::SLOT);
        let bindings: Vec<OpBinding> = slots.contributions(opsapi::BINDING_SLOT);
        let locals: Vec<opsapi::LocalOp> = slots.contributions(opsapi::LOCAL_SLOT);

        let binding_by_method: HashMap<String, OpBinding> =
            bindings.into_iter().map(|b| (b.method.clone(), b)).collect();
        let invokers: HashMap<String, LocalInvoker> =
            locals.into_iter().map(|l| (l.method.clone(), l.invoke)).collect();

        let mut routes = Vec::new();
        for op in operations {
            let Some(binding) = binding_by_method.get(&op.method).cloned() else {
                tracing::warn!(method = %op.method, "gateway: operation has no binding; skipping");
                continue;
            };
            let pattern = parse_pattern(&op.path);
            routes.push(Route { op, binding, pattern });
        }

        RouteTable {
            routes,
            invokers: Arc::new(invokers),
            remotes: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Finds the first route whose verb and path pattern match, returning it with the
    /// extracted path-wildcard values.
    fn find(&self, method: &str, path: &str) -> Option<(&Route, PathArgs)> {
        let parts = path_segments(path);
        for r in &self.routes {
            if r.op.verb.eq_ignore_ascii_case(method) {
                if let Some(args) = match_pattern(&r.pattern, &parts) {
                    return Some((r, args));
                }
            }
        }
        None
    }

    /// Finds a route by its rpc METHOD name — the player plane's lookup (there is no
    /// verb/path on that plane). A miss means the method is not player-reachable:
    /// only `#[http]`-bound ops are ever contributed to the table, so a wire-only
    /// internal method is invisible here by construction.
    fn find_by_method(&self, method: &str) -> Option<&Route> {
        self.routes.iter().find(|r| r.op.method == method)
    }

    /// Dispatches `op` on the topology-correct backend: a [`LocalBackend`] when this
    /// process holds the invoker, else a [`RemoteBackend`] over the cached edge
    /// caller. Serves BOTH planes — the HTTP handler and the player handler funnel
    /// through here, so the eviction rule below protects both.
    ///
    /// **Evict-on-error:** a Remote call failure drops that provider's cached
    /// `Arc<dyn Caller>`, so the NEXT request re-dials instead of reusing a dead
    /// connection forever (a provider restart would otherwise brick the route
    /// permanently). This is the reset idea of `remote::Reconnecting` WITHOUT the
    /// inline retry: one failed request, then self-heal. Eviction is guarded by
    /// pointer identity so a concurrent request's freshly-dialed replacement is
    /// never discarded by a stale failure.
    async fn dispatch(
        &self,
        op: &Operation,
        identity: Identity,
        req: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        match select_kind(&self.invokers, &op.method) {
            BackendKind::Local => {
                LocalBackend::new(self.invokers.clone()).invoke(op, identity, req).await
            }
            BackendKind::Remote => {
                let provider = provider_of(&op.method);
                let caller = self.remote_caller(provider).await?;
                let result = RemoteBackend::new(caller.clone()).invoke(op, identity, req).await;
                if result.is_err() {
                    let mut cache = self.remotes.lock().await;
                    if let Some(cached) = cache.get(provider) {
                        if Arc::ptr_eq(cached, &caller) {
                            cache.remove(provider);
                        }
                    }
                }
                result
            }
        }
    }

    /// Gets (or lazily dials + caches) an edge client to `provider`. The peer's QUIC
    /// address comes from `<PROVIDER_UPPER>_EDGE_ADDR`; the connection is reused across
    /// requests (until a failed call evicts it — see [`RouteTable::dispatch`]). In
    /// M1's per-svc topology every op a process serves is local, so this is the seam
    /// that lets a unified front-door route cross-provider without any per-module
    /// HTTP shim — exercised directly in the `RemoteBackend` tests.
    async fn remote_caller(&self, provider: &str) -> Result<Arc<dyn Caller>, Error> {
        let mut cache = self.remotes.lock().await;
        if let Some(c) = cache.get(provider) {
            return Ok(c.clone());
        }
        let env_key = format!("{}_EDGE_ADDR", provider.to_uppercase());
        let addr_str = std::env::var(&env_key).map_err(|_| {
            Error::unavailable(format!(
                "gateway: no peer address for provider {provider:?} (set {env_key})"
            ))
        })?;
        let addr: SocketAddr = addr_str.parse().map_err(|e| {
            Error::unavailable(format!("gateway: bad {env_key}={addr_str:?}: {e}"))
        })?;
        let ca = edge::shared_dev_ca()
            .map_err(|e| Error::unavailable(format!("gateway: edge CA: {e}")))?;
        let client = edge::Client::dial(addr, &ca)
            .await
            .map_err(|e| Error::unavailable(format!("gateway: dial {provider}: {e}")))?;
        let caller: Arc<dyn Caller> = Arc::new(client);
        cache.insert(provider.to_string(), caller.clone());
        Ok(caller)
    }
}

/// Which backend an op dispatches to. Split out from materialisation (which dials the
/// wire) so the pure selection rule is unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum BackendKind {
    Local,
    Remote,
}

/// The topology decision: an op with an in-process invoker is Local (zero-hop typed
/// call), else Remote (relayed to the owning peer). Presence of the `LocalInvoker` —
/// contributed only when the provider module runs in THIS process — is the signal.
fn select_kind(invokers: &HashMap<String, LocalInvoker>, method: &str) -> BackendKind {
    if invokers.contains_key(method) {
        BackendKind::Local
    } else {
        BackendKind::Remote
    }
}

/// Derives the provider name from a method: the segment before the first `.` (e.g.
/// `"characters.create"` → `"characters"`), the name the peer edge-serves under.
fn provider_of(method: &str) -> &str {
    match method.split_once('.') {
        Some((p, _)) => p,
        None => method,
    }
}

// ---------------------------------------------------------------------------
// The per-request HTTP front handler
// ---------------------------------------------------------------------------

async fn handle(front: Arc<FrontDoor>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str();
    let path = parts.uri.path();

    let table = front.table().clone();

    // (1) Match — everything unmatched is 404 (the fallback owns only op routes).
    let (op, binding, path_args) = match table.find(method, path) {
        Some((route, args)) => (route.op.clone(), route.binding.clone(), args),
        None => return error_response(StatusCode::NOT_FOUND, "not found"),
    };

    // (2) Auth-once: the single trust boundary. For AuthPlayer verify the bearer and
    // thread the verified player_id; AuthNone runs with no identity.
    let identity = match op.auth {
        AuthReq::Player => match authenticate(&parts.headers, &*front.verifier).await {
            Ok(id) => id,
            Err(resp) => return resp,
        },
        AuthReq::None => Identity::none(),
    };

    // (3) Decode: bounded body + matched wildcards → the wire request both backends consume.
    let body_bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let body_opt: Option<&[u8]> = if body_bytes.is_empty() {
        None
    } else {
        Some(body_bytes.as_ref())
    };
    let wire_req = match (binding.decode)(body_opt, &path_args) {
        Ok(r) => r,
        Err(e) => return op_error_response(&e),
    };

    // (4) Dispatch on the topology-correct backend (Local in-process, else the Remote
    // peer; a Remote failure evicts the cached conn so the next request re-dials).
    let wire_resp = match table.dispatch(&op, identity, wire_req).await {
        Ok(r) => r,
        Err(e) => return op_error_response(&e),
    };

    // (5) Reduce the wire response to the external HTTP body + status.
    match (binding.encode)(&wire_resp) {
        // A non-OK domain outcome surfaces as an encode-Err carrying its Status.
        Err(e) => op_error_response(&e),
        // Ok → the op's declared success code with the domain-only body (may be empty).
        Ok((body, _status)) => success_response(op.success, body),
    }
}

/// Verifies the request's bearer via the [`SessionVerifier`], returning the caller
/// [`Identity`] or the failure [`Response`] to write (401 on a missing/invalid token).
async fn authenticate(
    headers: &HeaderMap,
    verifier: &dyn SessionVerifier,
) -> Result<Identity, Response> {
    let Some(token) = bearer(headers) else {
        return Err(error_response(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    match verifier.verify(&token).await {
        Some(pid) => Ok(Identity::player(pid)),
        None => Err(error_response(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

/// Extracts the token from an `Authorization: Bearer <token>` header, or `None`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(str::to_string)
}

/// A plain text error response at an explicit HTTP status.
fn error_response(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

/// Maps an operation [`Error`]'s domain [`Status`] onto its HTTP status and writes the
/// message (mirrors Go's `writeOpError`/`httpStatus`).
fn op_error_response(e: &Error) -> Response {
    let code =
        StatusCode::from_u16(e.status.http()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(code, &e.msg)
}

/// Writes a successful op response: the declared `success` code, plus the JSON body if
/// non-empty (a 204-style op returns an empty body).
fn success_response(success: u16, body: Option<Vec<u8>>) -> Response {
    let code = StatusCode::from_u16(success).unwrap_or(StatusCode::OK);
    match body {
        Some(b) if !b.is_empty() => {
            let mut resp = Response::new(Body::from(b));
            *resp.status_mut() = code;
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            resp
        }
        _ => {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = code;
            resp
        }
    }
}

// ---------------------------------------------------------------------------
// Path pattern matching (the `{wild}` support the route table needs)
// ---------------------------------------------------------------------------

/// Splits a path into its non-empty segments (`"/characters/42"` → `["characters","42"]`).
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Parses a route pattern into segments: `{name}` (a trailing `...` matcher stripped)
/// is a wildcard, everything else a literal.
fn parse_pattern(path: &str) -> Vec<Seg> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            Some(name) => Seg::Wild(name.trim_end_matches("...").to_string()),
            None => Seg::Lit(s.to_string()),
        })
        .collect()
}

/// Matches parsed pattern segments against request segments, returning the captured
/// wildcard values. Segment counts must match exactly; a wildcard binds one segment.
fn match_pattern(pattern: &[Seg], parts: &[&str]) -> Option<PathArgs> {
    if pattern.len() != parts.len() {
        return None;
    }
    let mut args = PathArgs::new();
    for (seg, part) in pattern.iter().zip(parts) {
        match seg {
            Seg::Lit(lit) => {
                if lit != part {
                    return None;
                }
            }
            Seg::Wild(name) => {
                args.insert(name.clone(), (*part).to_string());
            }
        }
    }
    Some(args)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use opsapi::{DecodeFn, EncodeFn, LocalOp, OpSet, Status};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    // ---- (a) route matching incl. {wild} extraction ----

    #[test]
    fn match_pattern_literal_and_wildcard() {
        let pat = parse_pattern("/characters/{id}");
        let args = match_pattern(&pat, &path_segments("/characters/42")).unwrap();
        assert_eq!(args.get("id").map(String::as_str), Some("42"));

        // Wrong literal, wrong arity → no match.
        assert!(match_pattern(&pat, &path_segments("/players/42")).is_none());
        assert!(match_pattern(&pat, &path_segments("/characters")).is_none());
        assert!(match_pattern(&pat, &path_segments("/characters/42/extra")).is_none());
    }

    #[test]
    fn match_pattern_no_wildcards() {
        let pat = parse_pattern("/characters");
        assert!(match_pattern(&pat, &path_segments("/characters")).unwrap().is_empty());
        assert!(match_pattern(&pat, &path_segments("/characters/1")).is_none());
    }

    // ---- (d) select_backend picks Local iff an invoker exists ----

    #[test]
    fn select_kind_local_when_invoker_present_else_remote() {
        let mut invokers: HashMap<String, LocalInvoker> = HashMap::new();
        invokers.insert("characters.create".into(), echo_invoker());
        assert_eq!(select_kind(&invokers, "characters.create"), BackendKind::Local);
        assert_eq!(select_kind(&invokers, "inventory.grant"), BackendKind::Remote);
    }

    #[test]
    fn provider_of_takes_prefix() {
        assert_eq!(provider_of("characters.create"), "characters");
        assert_eq!(provider_of("bare"), "bare");
    }

    // ---- (b) auth-once ----

    #[tokio::test]
    async fn dev_verifier_accepts_dev_token_only() {
        let v = DevSessionVerifier::new();
        assert_eq!(v.verify("dev-alice").await.as_deref(), Some("alice"));
        assert_eq!(v.verify("dev-").await, None); // empty suffix rejected
        assert_eq!(v.verify("alice").await, None); // no prefix rejected
        assert_eq!(v.verify("").await, None);
    }

    #[tokio::test]
    async fn authenticate_paths() {
        let v = DevSessionVerifier::new();

        // Valid bearer → identity threaded.
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer dev-alice"));
        let id = authenticate(&h, &v).await.unwrap();
        assert_eq!(id.player_id(), Some("alice"));

        // Missing header → 401.
        let empty = HeaderMap::new();
        let resp = authenticate(&empty, &v).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Invalid token → 401.
        let mut bad = HeaderMap::new();
        bad.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer nope"));
        let resp = authenticate(&bad, &v).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ---- (b/c) end-to-end through the axum fallback ----

    /// A LocalInvoker echoing the caller identity + the wire request bytes as a
    /// JSON object (an AuthPlayer op: rejects a missing identity with Invalid).
    fn echo_invoker() -> LocalInvoker {
        Arc::new(|ident: Identity, req: Vec<u8>| {
            Box::pin(async move {
                let pid = ident
                    .player_id()
                    .ok_or_else(|| Error::invalid("no identity"))?
                    .to_string();
                let req = String::from_utf8(req).unwrap();
                Ok(format!(r#"{{"status":"Ok","pid":"{pid}","echo":{req}}}"#).into_bytes())
            })
        })
    }

    /// Builds a demo `OpSet` for `POST /demo/{id}` (AuthPlayer, success 200) whose
    /// decode packs `{id, body}`, invoke echoes identity, and encode drops the status
    /// envelope on Ok / surfaces a non-Ok status as an `Err`.
    fn demo_opset() -> OpSet {
        let decode: DecodeFn = Arc::new(|body, path| {
            let id = path.get("id").cloned().unwrap_or_default();
            let body = body.unwrap_or(b"null");
            Ok(format!(
                r#"{{"id":"{id}","body":{}}}"#,
                std::str::from_utf8(body).map_err(|e| Error::invalid(e.to_string()))?
            )
            .into_bytes())
        });
        let encode: EncodeFn = Arc::new(|resp: &[u8]| {
            let v: serde_json::Value =
                serde_json::from_slice(resp).map_err(|e| Error::internal(e.to_string()))?;
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("Ok");
            if status != "Ok" {
                return Err(Error::new(Status::NotFound, "demo not found"));
            }
            Ok((Some(resp.to_vec()), Status::Ok))
        });
        OpSet {
            operation: Operation {
                method: "demo.echo".into(),
                verb: "POST".into(),
                path: "/demo/{id}".into(),
                auth: AuthReq::Player,
                success: 200,
            },
            binding: OpBinding { method: "demo.echo".into(), decode, encode },
            local: LocalOp { method: "demo.echo".into(), invoke: echo_invoker() },
        }
    }

    /// Wires a `FrontDoor` over a `Slots` carrying the demo op, so a test can drive
    /// either plane (the axum fallback or the player handler) through it.
    fn demo_front_door() -> Arc<FrontDoor> {
        let slots = Arc::new(Slots::new());
        let op = demo_opset();
        slots.contribute(opsapi::SLOT, op.operation);
        slots.contribute(opsapi::BINDING_SLOT, op.binding);
        slots.contribute(opsapi::LOCAL_SLOT, op.local);
        Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())))
    }

    fn demo_router() -> Router {
        demo_front_door().router()
    }

    async fn body_string(resp: Response) -> (StatusCode, String) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn end_to_end_decode_invoke_encode() {
        let router = demo_router();
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/demo/42")
            .header(header::AUTHORIZATION, "Bearer dev-alice")
            .body(Body::from("123"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let (status, body) = body_string(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#""pid":"alice""#), "{body}");
        assert!(body.contains(r#""id":"42""#), "{body}");
        assert!(body.contains(r#""body":123"#), "{body}");
    }

    #[tokio::test]
    async fn end_to_end_missing_bearer_is_401_before_dispatch() {
        let router = demo_router();
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/demo/42")
            .body(Body::from("1"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn end_to_end_unmatched_route_is_404() {
        let router = demo_router();
        let req = HttpRequest::builder()
            .method("GET")
            .uri("/nope")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn end_to_end_domain_status_maps_to_http() {
        // Drive an encode that surfaces a non-Ok status: build an op whose invoker
        // returns a non-Ok status envelope, proving encode-Err → mapped HTTP code.
        let slots = Arc::new(Slots::new());
        let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
        let invoke: LocalInvoker = Arc::new(|_id, _req| {
            Box::pin(async move { Ok(br#"{"status":"NotFound"}"#.to_vec()) })
        });
        let encode: EncodeFn = Arc::new(|resp: &[u8]| {
            let v: serde_json::Value = serde_json::from_slice(resp).unwrap();
            if v.get("status").and_then(|s| s.as_str()) != Some("Ok") {
                return Err(Error::new(Status::NotFound, "missing"));
            }
            Ok((Some(resp.to_vec()), Status::Ok))
        });
        slots.contribute(
            opsapi::SLOT,
            Operation {
                method: "demo.get".into(),
                verb: "GET".into(),
                path: "/demo/{id}".into(),
                auth: AuthReq::None,
                success: 200,
            },
        );
        slots.contribute(opsapi::BINDING_SLOT, OpBinding { method: "demo.get".into(), decode, encode });
        slots.contribute(opsapi::LOCAL_SLOT, LocalOp { method: "demo.get".into(), invoke });
        let front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));
        let router = front.router();

        let req = HttpRequest::builder().method("GET").uri("/demo/7").body(Body::empty()).unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ---- find_by_method: the player plane's lookup ----

    #[test]
    fn find_by_method_hit_and_miss() {
        let slots = Slots::new();
        let op = demo_opset();
        slots.contribute(opsapi::SLOT, op.operation);
        slots.contribute(opsapi::BINDING_SLOT, op.binding);
        slots.contribute(opsapi::LOCAL_SLOT, op.local);
        let table = RouteTable::build(&slots);

        let route = table.find_by_method("demo.echo").expect("bound op is found");
        assert_eq!(route.op.method, "demo.echo");
        assert_eq!(route.op.auth, AuthReq::Player);

        // A wire-only internal method was never contributed → invisible (the
        // allow-list gate the player plane relies on).
        assert!(table.find_by_method("characters.ownerOf").is_none());
    }

    // ---- the player handler: the pinned {status, err} grammar on every outcome ----

    /// Drives the player handler exactly as the `edge::PlayerServer` would, returning
    /// the response payload as a string. The handler is Ok on EVERY domain outcome
    /// (the pinned grammar), so unwrapping here is itself an assertion.
    async fn call_player(
        front: &Arc<FrontDoor>,
        method: &str,
        token: Option<&str>,
        payload: &[u8],
    ) -> String {
        let h = front.player_handler();
        let bytes = h(method.to_string(), token.map(str::to_string), payload.to_vec())
            .await
            .expect("domain outcomes never surface as transport Err");
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn player_missing_token_on_auth_op_is_unauthorized_envelope() {
        let front = demo_front_door();
        let body = call_player(&front, "demo.echo", None, br#"{"n":1}"#).await;
        // Exact macro grammar: field `err`, Status as bare variant name.
        assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
    }

    #[tokio::test]
    async fn player_bad_token_is_unauthorized_envelope() {
        let front = demo_front_door();
        let body = call_player(&front, "demo.echo", Some("nope-x"), br#"{"n":1}"#).await;
        assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
    }

    #[tokio::test]
    async fn player_unknown_method_is_not_found_envelope() {
        let front = demo_front_door();
        // `characters.ownerOf` is the canonical wire-only internal: a peer edge
        // serves it, but it is absent from the route table → not player-reachable.
        let body = call_player(&front, "characters.ownerOf", Some("dev-alice"), b"{}").await;
        assert_eq!(body, r#"{"status":"NotFound","err":"unknown operation"}"#);
    }

    #[tokio::test]
    async fn player_malformed_json_is_invalid_at_the_front() {
        let front = demo_front_door();
        let body = call_player(&front, "demo.echo", Some("dev-alice"), b"{not json").await;
        assert_eq!(body, r#"{"status":"Invalid","err":"malformed request payload"}"#);
    }

    #[tokio::test]
    async fn player_happy_path_returns_wire_response_verbatim() {
        let front = demo_front_door();
        // No OpBinding::decode on this plane: the payload IS the wire request.
        let body = call_player(&front, "demo.echo", Some("dev-alice"), br#"{"n":1}"#).await;
        assert!(body.contains(r#""status":"Ok""#), "{body}");
        assert!(body.contains(r#""pid":"alice""#), "{body}");
        assert!(body.contains(r#""echo":{"n":1}"#), "{body}");
    }

    #[tokio::test]
    async fn player_auth_none_op_runs_with_no_identity() {
        // An AuthNone op whose invoker ASSERTS it received no identity.
        let slots = Arc::new(Slots::new());
        let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
        let encode: EncodeFn = Arc::new(|resp: &[u8]| Ok((Some(resp.to_vec()), Status::Ok)));
        let invoke: LocalInvoker = Arc::new(|ident: Identity, _req| {
            Box::pin(async move {
                if ident.player_id().is_some() {
                    return Err(Error::internal("AuthNone op must see Identity::none()"));
                }
                Ok(br#"{"status":"Ok","anon":true}"#.to_vec())
            })
        });
        slots.contribute(
            opsapi::SLOT,
            Operation {
                method: "demo.public".into(),
                verb: "GET".into(),
                path: "/public".into(),
                auth: AuthReq::None,
                success: 200,
            },
        );
        slots.contribute(
            opsapi::BINDING_SLOT,
            OpBinding { method: "demo.public".into(), decode, encode },
        );
        slots.contribute(opsapi::LOCAL_SLOT, LocalOp { method: "demo.public".into(), invoke });
        let front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));

        // No token at all — must dispatch, not 401.
        let body = call_player(&front, "demo.public", None, b"{}").await;
        assert_eq!(body, r#"{"status":"Ok","anon":true}"#);
    }

    #[tokio::test]
    async fn player_backend_error_is_reserialized_as_status_err_envelope() {
        // A backend failure (an Err(opsapi::Error), not a status-carrying payload)
        // must still come back in the pinned {status, err} grammar. Drive it with an
        // op that has no local invoker AND no <PROVIDER>_EDGE_ADDR: dispatch fails
        // with Unavailable, which the front re-serializes as the envelope.
        let slots = Arc::new(Slots::new());
        let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
        let encode: EncodeFn = Arc::new(|resp: &[u8]| Ok((Some(resp.to_vec()), Status::Ok)));
        slots.contribute(
            opsapi::SLOT,
            Operation {
                method: "ghostprov.op".into(),
                verb: "GET".into(),
                path: "/ghost".into(),
                auth: AuthReq::None,
                success: 200,
            },
        );
        slots.contribute(
            opsapi::BINDING_SLOT,
            OpBinding { method: "ghostprov.op".into(), decode, encode },
        );
        // NO LOCAL_SLOT contribution → Remote; GHOSTPROV_EDGE_ADDR is unset.
        let remote_front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));
        let body = call_player(&remote_front, "ghostprov.op", None, b"{}").await;
        assert!(body.starts_with(r#"{"status":"Unavailable","err":""#), "{body}");
        assert!(body.contains("GHOSTPROV_EDGE_ADDR"), "{body}");
    }

    // ---- RemoteBackend exercised against a fake Caller ----

    /// What a `FakeCaller` records for one relayed call: (method, identity, payload).
    type Seen = (String, Option<String>, Vec<u8>);

    struct FakeCaller {
        seen: std::sync::Mutex<Option<Seen>>,
    }

    #[async_trait::async_trait]
    impl Caller for FakeCaller {
        async fn call(
            &self,
            method: &str,
            identity: Option<&str>,
            payload: &[u8],
        ) -> Result<Vec<u8>, Error> {
            *self.seen.lock().unwrap() =
                Some((method.to_string(), identity.map(str::to_string), payload.to_vec()));
            Ok(br#"{"status":"Ok","relayed":true}"#.to_vec())
        }
    }

    #[tokio::test]
    async fn remote_backend_relays_method_identity_and_payload() {
        let caller = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
        let backend = RemoteBackend::new(caller.clone());
        let op = Operation {
            method: "characters.create".into(),
            verb: "POST".into(),
            path: "/characters".into(),
            auth: AuthReq::Player,
            success: 201,
        };
        let resp = backend
            .invoke(&op, Identity::player("bob"), b"{\"name\":\"x\"}".to_vec())
            .await
            .unwrap();
        assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
        let seen = caller.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.0, "characters.create");
        assert_eq!(seen.1.as_deref(), Some("bob"));
        assert_eq!(seen.2, b"{\"name\":\"x\"}");
    }

    #[tokio::test]
    async fn local_backend_missing_invoker_is_internal_error() {
        let backend = LocalBackend::new(Arc::new(HashMap::new()));
        let op = Operation {
            method: "x.y".into(),
            verb: "POST".into(),
            path: "/x".into(),
            auth: AuthReq::None,
            success: 200,
        };
        let err = backend.invoke(&op, Identity::none(), vec![]).await.unwrap_err();
        assert_eq!(err.status, Status::Internal);
    }

    // ---- evict-on-error: a dead cached remote conn heals on the next request ----

    /// A `Caller` that always fails, counting how many times it was tried — the
    /// stand-in for a cached edge conn whose peer restarted.
    #[derive(Default)]
    struct FailingCaller {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Caller for FailingCaller {
        async fn call(&self, _m: &str, _i: Option<&str>, _p: &[u8]) -> Result<Vec<u8>, Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::unavailable("fake: connection lost"))
        }
    }

    #[tokio::test]
    async fn remote_dispatch_evicts_failed_caller_so_next_request_redials() {
        // A table with NO local invokers → every dispatch selects Remote.
        let table = RouteTable::build(&Slots::new());
        let op = Operation {
            method: "fakeprov.op".into(),
            verb: "POST".into(),
            path: "/fake".into(),
            auth: AuthReq::None,
            success: 200,
        };

        // Seed the cache as if a previous request had dialed this provider.
        let failing = Arc::new(FailingCaller::default());
        table
            .remotes
            .lock()
            .await
            .insert("fakeprov".into(), failing.clone() as Arc<dyn Caller>);

        // First request: the cached caller fails → the error propagates AND the
        // dead entry is evicted.
        let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
        assert_eq!(err.status, Status::Unavailable);
        assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
        assert!(
            !table.remotes.lock().await.contains_key("fakeprov"),
            "failed caller must be evicted"
        );

        // Second request goes back through remote_caller (re-dial). With no
        // FAKEPROV_EDGE_ADDR the re-dial path itself errors — and crucially the DEAD
        // caller was NOT reused (its call count is unchanged).
        let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
        assert!(err.msg.contains("FAKEPROV_EDGE_ADDR"), "{}", err.msg);
        assert_eq!(failing.calls.load(Ordering::SeqCst), 1, "dead caller must not be reused");

        // And once a re-dial succeeds (simulated: a fresh healthy caller lands in the
        // cache, exactly what remote_caller does after dialing), the route works again
        // — the self-heal, with exactly one failed request in between.
        let healthy = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
        table.remotes.lock().await.insert("fakeprov".into(), healthy as Arc<dyn Caller>);
        let resp = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap();
        assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
        assert!(
            table.remotes.lock().await.contains_key("fakeprov"),
            "a successful call must keep its caller cached"
        );
    }
}

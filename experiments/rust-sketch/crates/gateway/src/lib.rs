//! `gateway` — the HTTP front-door lifecycle module, present in EVERY `app::run`
//! process (the monolith and each split `*-svc`). It mounts one axum FALLBACK
//! handler onto the shared `Context` router; that handler fronts the process's op
//! surface WITHOUT `app` importing the gateway (the leaf-slot seam — `app` stays
//! topology-blind). Port of Go's `modules/gateway` (`gateway.go` + `backend.go`).
//!
//! For a request it does, in order:
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
//!   4. **Select the backend** (`select_kind`): a `LocalBackend` when this process
//!      holds the op's `LocalInvoker`, else a `RemoteBackend` dialing the owning peer.
//!   5. **Invoke**, then reduce the wire response via `OpBinding::encode` — an
//!      encode-`Err` carries the domain `Status` (→ its HTTP code); an `Ok` writes the
//!      op's declared `success` code with the domain body.
//!
//! ## Lazy route table (the init-ordering sidestep)
//! Modules contribute their `OpSet`s during their own `init`, and the gateway's
//! `init` may run first. So the table is NOT built eagerly: the fallback holds a
//! [`std::sync::OnceLock`] and builds the table from `ctx.contributions(...)` on the
//! FIRST request — by which time every module's `init` has run (requests only arrive
//! after `app::run` finishes Build and starts serving).

mod backend;
mod verifier;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use contrib::Slots;
use lifecycle::{Context, Module};
use opsapi::{AuthReq, Caller, Error, Identity, LocalInvoker, OpBinding, Operation, PathArgs};

pub use backend::{LocalBackend, OperationBackend, RemoteBackend};
pub use verifier::{DevSessionVerifier, SessionVerifier};

/// Caps the request body the gateway buffers before decoding an operation, so a
/// hostile client cannot make the front-handler allocate without bound. 1 MiB is
/// comfortably above any player operation's request (matches Go's `maxBodyBytes`).
const MAX_BODY_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The front-door module. Stateless apart from the [`SessionVerifier`] it fronts
/// auth with; the route table is read lazily from the `Context` on the first request.
pub struct Gateway {
    verifier: Arc<dyn SessionVerifier>,
}

impl Gateway {
    /// A gateway using the M1 [`DevSessionVerifier`] (accepts `Bearer dev-<player_id>`).
    pub fn new() -> Self {
        Gateway::with_verifier(Arc::new(DevSessionVerifier::new()))
    }

    /// A gateway using a caller-supplied verifier — the seam Milestone 2 uses to swap
    /// in real `accounts` sessions (or an edge-backed verifier for the split front-door).
    pub fn with_verifier(verifier: Arc<dyn SessionVerifier>) -> Self {
        Gateway { verifier }
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

    /// Mounts the single fallback handler. No I/O. The fallback captures the slot
    /// registry + verifier and lazily builds the route table on first request.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let state = Arc::new(GatewayState {
            slots: ctx.slots().clone(),
            verifier: self.verifier.clone(),
            table: OnceLock::new(),
        });
        ctx.mount(front_router(state));
        Ok(())
    }
}

/// Builds the axum router carrying ONLY the gateway's fallback. `Router::merge`
/// (used by `Context::mount`) tolerates exactly one fallback across all merged
/// routers; messaging and `app` add plain routes, so this is the sole fallback.
fn front_router(state: Arc<GatewayState>) -> Router {
    Router::new().fallback(move |req: Request| {
        let state = state.clone();
        async move { handle(state, req).await }
    })
}

// ---------------------------------------------------------------------------
// Shared state + lazy route table
// ---------------------------------------------------------------------------

/// What the fallback closure holds for the life of the process: the slot registry to
/// build the table from, the auth verifier, and the once-built table.
struct GatewayState {
    slots: Arc<Slots>,
    verifier: Arc<dyn SessionVerifier>,
    table: OnceLock<Arc<RouteTable>>,
}

impl GatewayState {
    /// The route table, built from the slots on first access. By first-request time
    /// every module's `init` (where `OpSet`s are contributed) has completed.
    fn table(&self) -> &Arc<RouteTable> {
        self.table
            .get_or_init(|| Arc::new(RouteTable::build(&self.slots)))
    }
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

    /// Materialises the topology-correct backend for `op`: a [`LocalBackend`] when this
    /// process holds the invoker, else a [`RemoteBackend`] dialing the owning peer.
    async fn backend_for(&self, op: &Operation) -> Result<Box<dyn OperationBackend>, Error> {
        match select_kind(&self.invokers, &op.method) {
            BackendKind::Local => Ok(Box::new(LocalBackend::new(self.invokers.clone()))),
            BackendKind::Remote => {
                let caller = self.remote_caller(provider_of(&op.method)).await?;
                Ok(Box::new(RemoteBackend::new(caller)))
            }
        }
    }

    /// Gets (or lazily dials + caches) an edge client to `provider`. The peer's QUIC
    /// address comes from `<PROVIDER_UPPER>_EDGE_ADDR`; the connection is reused across
    /// requests. In M1's per-svc topology every op a process serves is local, so this
    /// is the seam that lets a unified front-door route cross-provider without any
    /// per-module HTTP shim — exercised directly in the `RemoteBackend` tests.
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
// The per-request front handler
// ---------------------------------------------------------------------------

async fn handle(state: Arc<GatewayState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str();
    let path = parts.uri.path();

    let table = state.table().clone();

    // (1) Match — everything unmatched is 404 (the fallback owns only op routes).
    let (op, binding, path_args) = match table.find(method, path) {
        Some((route, args)) => (route.op.clone(), route.binding.clone(), args),
        None => return error_response(StatusCode::NOT_FOUND, "not found"),
    };

    // (2) Auth-once: the single trust boundary. For AuthPlayer verify the bearer and
    // thread the verified player_id; AuthNone runs with no identity.
    let identity = match op.auth {
        AuthReq::Player => match authenticate(&parts.headers, &*state.verifier).await {
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

    // (4) Select the topology-correct backend (Local same-process, else Remote peer).
    let backend = match table.backend_for(&op).await {
        Ok(b) => b,
        Err(e) => return op_error_response(&e),
    };

    // (5) Invoke, then reduce the wire response to the external HTTP body + status.
    let wire_resp = match backend.invoke(&op, identity, wire_req).await {
        Ok(r) => r,
        Err(e) => return op_error_response(&e),
    };
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

    /// Wires a `GatewayState` over a `Slots` carrying the demo op, then the axum
    /// fallback router, so a test can drive real HTTP requests through it.
    fn demo_router() -> Router {
        let slots = Arc::new(Slots::new());
        let op = demo_opset();
        slots.contribute(opsapi::SLOT, op.operation);
        slots.contribute(opsapi::BINDING_SLOT, op.binding);
        slots.contribute(opsapi::LOCAL_SLOT, op.local);
        let state = Arc::new(GatewayState {
            slots,
            verifier: Arc::new(DevSessionVerifier::new()),
            table: OnceLock::new(),
        });
        front_router(state)
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
        let state = Arc::new(GatewayState {
            slots,
            verifier: Arc::new(DevSessionVerifier::new()),
            table: OnceLock::new(),
        });
        let router = front_router(state);

        let req = HttpRequest::builder().method("GET").uri("/demo/7").body(Body::empty()).unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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
}

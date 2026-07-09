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
//!      shadows `/healthz`/`/readyz` (added by `app`) or `POST /events` (the
//!      durable-events plane, mounted by `app::run` when the process has a DB).
//!   2. **API-key check** (post-match, pre-auth): every op-dispatched request must
//!      carry an `X-Api-Key` header naming a known, unrevoked key whose policy allows
//!      the matched method (see [`KeyVerifier`]) — missing → 401, unknown/revoked →
//!      401, policy miss → 403. Non-op routes (`/healthz`, `/metrics`, `POST /events`,
//!      the passthroughs) never reach this check by construction.
//!   3. **Auth-once:** for an `AuthReq::Player` op it verifies the `Authorization:
//!      Bearer <token>` header via the [`SessionVerifier`] and threads the resolved
//!      player_id as an `opsapi::Identity`. This is the SINGLE trust boundary —
//!      downstream (local invoker or peer over the edge) never re-verifies. An
//!      `AuthReq::None` op runs with `Identity::none()`.
//!   4. **Decode** the HTTP body + matched path wildcards into the wire request via
//!      the op's `OpBinding::decode`.
//!   5. **Dispatch** on the topology-correct backend (`RouteTable::dispatch`): a
//!      [`LocalBackend`] when this process holds the op's `LocalInvoker`, else a
//!      [`RemoteBackend`] dialing the owning peer (a Remote failure evicts the cached
//!      connection so the next request re-dials).
//!   6. Reduce the wire response via `OpBinding::encode` — an encode-`Err` carries the
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
mod keys;
mod proxy;
mod verifier;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
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
pub use keys::{AllowAllKeyVerifier, KeyVerifier, RealKeyVerifier};
pub use verifier::{DevSessionVerifier, SessionVerifier, SessionsVerifier};

use keys::{check_api_key, KeyDenial};

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
    /// The auth verifier. `None` (the [`Gateway::new`] default) defers resolution
    /// to `init`: the REAL `accounts.sessions` capability from the registry, or —
    /// only when `ACCOUNTS_DEV_AUTH` is explicitly set — the dev fallback; absent
    /// both, startup FAILS loudly (see `verifier::resolve_verifier`). `Some` is a
    /// caller-supplied override (tests).
    verifier: Option<Arc<dyn SessionVerifier>>,
    /// The API-key verifier. `None` (the [`Gateway::new`] default) defers resolution
    /// to `init`: the REAL `apikeys.keys` capability from the registry (TTL-cached),
    /// or — only when `APIKEYS_DEV_ALLOW` is explicitly set — the allow-all dev
    /// fallback; absent both, startup FAILS loudly (see `keys::resolve_key_verifier`).
    /// `Some` is a caller-supplied override (tests).
    key_verifier: Option<Arc<dyn KeyVerifier>>,
    /// When set, the process-wide player-facing QUIC server (built by `main` and
    /// passed as a shared handle). `init` installs the
    /// [`FrontDoor::player_handler`] on it so the process fronts players over QUIC
    /// as well as HTTP. `None` for a process with no public player port.
    player_edge: Option<Arc<Mutex<edge::PlayerServer>>>,
    /// HTTP reverse-proxy passthrough routes `(prefix, origin)` the composition root
    /// wired via [`Gateway::with_passthrough`] (e.g. `("/admin", "127.0.0.1:8085")`).
    /// Handed to the [`FrontDoor`]'s [`proxy::ProxyTable`] at `init`. Empty on the
    /// monolith (no split peers to proxy to), so every unmatched route stays a 404 —
    /// exactly the prior behaviour. Topology lives in `cmd/*`, never read from env here.
    passthroughs: Vec<(String, String)>,
}

impl Gateway {
    /// A gateway that resolves its verifier at `init` (Step 6): the real
    /// `accounts.sessions` capability — provided locally by the accounts module or
    /// by an `accountsrpc` remote stub — else a loud startup failure (unless
    /// `ACCOUNTS_DEV_AUTH=1` explicitly enables the dev fallback).
    pub fn new() -> Self {
        Gateway {
            verifier: None,
            key_verifier: None,
            player_edge: None,
            passthroughs: Vec::new(),
        }
    }

    /// A gateway using a caller-supplied verifier — bypasses the `init`-time
    /// resolution entirely (unit tests construct a [`DevSessionVerifier`] here).
    pub fn with_verifier(verifier: Arc<dyn SessionVerifier>) -> Self {
        Gateway {
            verifier: Some(verifier),
            key_verifier: None,
            player_edge: None,
            passthroughs: Vec::new(),
        }
    }

    /// Overrides the API-key verifier — bypasses the `init`-time resolution of the
    /// `apikeys.keys` capability entirely (builder-style; tests inject a fake here).
    pub fn with_key_verifier(mut self, key_verifier: Arc<dyn KeyVerifier>) -> Self {
        self.key_verifier = Some(key_verifier);
        self
    }

    /// Adds one HTTP reverse-proxy passthrough: an unmatched request under `prefix`
    /// (`/admin`, `/accounts/epic`) is proxied to `origin` (a bare `host:port` or a
    /// full URL) instead of 404-ing. Builder-style + accumulating, so a composition
    /// root can wire several. `origin` is resolved by `cmd/*` (typically from env via
    /// its `env_addr` helper); a blank origin is dropped by [`proxy::ProxyTable`], so
    /// the prefix stays a 404 — mirroring the old `from_env` skip-empty semantics.
    pub fn with_passthrough(mut self, prefix: &str, origin: &str) -> Self {
        self.passthroughs.push((prefix.to_string(), origin.to_string()));
        self
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
    ///
    /// BOTH verifiers are resolved HERE (phase 2) when none was injected: every
    /// provider's phase-1 `register` — the accounts/apikeys module or its remote
    /// stub — has already run, so `accounts.sessions`/`apikeys.keys` is present iff
    /// this process was wired for it. Absent capability + no explicit
    /// `ACCOUNTS_DEV_AUTH`/`APIKEYS_DEV_ALLOW` fails startup loudly (no silent dev
    /// fallback).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let verifier = match &self.verifier {
            Some(v) => v.clone(),
            None => verifier::resolve_verifier(ctx)?,
        };
        let key_verifier = match &self.key_verifier {
            Some(v) => v.clone(),
            None => keys::resolve_key_verifier(ctx)?,
        };
        let front_door = Arc::new(FrontDoor::new(
            ctx.slots().clone(),
            verifier,
            key_verifier,
            self.passthroughs.clone(),
        ));
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
    /// The API-key policy verifier — consulted post-match, pre-auth on BOTH planes.
    key_verifier: Arc<dyn KeyVerifier>,
    table: OnceLock<Arc<RouteTable>>,
    /// The HTTP reverse-proxy passthrough for non-operation routes (`/admin`,
    /// `/accounts/epic`), built from the routes the composition root wired via
    /// [`Gateway::with_passthrough`]. Empty when nothing is configured, so an
    /// unmatched route stays a 404 (the prior behaviour).
    proxy: proxy::ProxyTable,
}

impl FrontDoor {
    /// A façade over `slots` (the contribution registry the route table is built
    /// from) fronting auth with `verifier` and the key check with `key_verifier`.
    /// `passthroughs` are the `(prefix, origin)` reverse-proxy routes the composition
    /// root supplied — empty for a process that proxies nothing (every unmatched
    /// route stays a 404).
    pub fn new(
        slots: Arc<Slots>,
        verifier: Arc<dyn SessionVerifier>,
        key_verifier: Arc<dyn KeyVerifier>,
        passthroughs: Vec<(String, String)>,
    ) -> FrontDoor {
        FrontDoor {
            slots,
            verifier,
            key_verifier,
            table: OnceLock::new(),
            proxy: proxy::ProxyTable::from_routes(passthroughs),
        }
    }

    /// The route table, built from the slots on first access. By first-request time
    /// every module's `init` (where `OpSet`s are contributed) has completed.
    fn table(&self) -> &Arc<RouteTable> {
        self.table
            .get_or_init(|| Arc::new(RouteTable::build(&self.slots)))
    }

    /// Builds the axum router carrying ONLY the gateway's fallback. `Router::merge`
    /// (used by `Context::mount`) tolerates exactly one fallback across all merged
    /// routers; the durable-events plane and `app` add plain routes, so this is the
    /// sole fallback.
    pub fn router(self: &Arc<Self>) -> Router {
        let front = self.clone();
        // `Option<ConnectInfo>`: the real server wires connection info
        // (`into_make_service_with_connect_info` in `app::run`), so the passthrough can
        // set `X-Forwarded-For`; the unit tests call `oneshot` without it → `None`,
        // and the proxy simply omits the direct-peer hop.
        Router::new().fallback(
            move |peer: Option<ConnectInfo<SocketAddr>>, req: Request| {
                let front = front.clone();
                async move { handle(front, peer.map(|c| c.0), req).await }
            },
        )
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
        Arc::new(move |method, token, api_key, payload| {
            let front = front.clone();
            Box::pin(async move { Ok(front.handle_player(method, token, api_key, payload).await) })
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
    ///   3. **API-key check** — AFTER the method match (an unknown method stays
    ///      NotFound; the key check must not leak which methods exist) and before
    ///      session auth, same order as the HTTP plane: missing → Unauthorized,
    ///      unknown/revoked → Unauthorized, policy miss → Forbidden.
    ///   4. **Auth-once:** `token` is ATTACKER-CONTROLLED input (a claim, not an
    ///      identity — the player envelope carries no identity field by design).
    ///      For an `AuthReq::Player` op it is required and verified via the
    ///      [`SessionVerifier`]; only the VERIFIED player_id becomes the `Identity`
    ///      threaded downstream, and nothing downstream re-verifies. `AuthReq::None`
    ///      runs with `Identity::none()`.
    ///   5. **Dispatch** and return the wire response bytes VERBATIM — the domain
    ///      `Status` already rides inside the generated response envelope. A backend
    ///      `Err(opsapi::Error)` is re-serialized as the same `{status, err}` shape.
    async fn handle_player(
        &self,
        method: String,
        token: Option<String>,
        api_key: Option<String>,
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

        // (3) API-key check — post-match, pre-auth (Decision 5's exact three-way).
        if let Err(denial) =
            check_api_key(&*self.key_verifier, api_key.as_deref(), &route.op.method).await
        {
            return front_envelope(denial.status(), denial.message());
        }

        // (4) Auth-once: the single trust boundary, same rule as the HTTP plane.
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

        // (5) Dispatch; the wire response IS the player response (envelope included).
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
    /// Peer edge addresses per provider (provider → UNPARSED `host:port`), collected
    /// from `opsapi::PEER_SLOT` — one entry per `remote::Stub` the composition root
    /// wired. `remote_caller` looks a provider up here (and parses lazily) instead of
    /// reading a per-provider edge-address env var: topology is injected by the
    /// composition root, never read inside this module.
    peers: HashMap<String, String>,
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
        let peer_addrs: Vec<opsapi::PeerAddr> = slots.contributions(opsapi::PEER_SLOT);

        let binding_by_method: HashMap<String, OpBinding> =
            bindings.into_iter().map(|b| (b.method.clone(), b)).collect();
        let invokers: HashMap<String, LocalInvoker> =
            locals.into_iter().map(|l| (l.method.clone(), l.invoke)).collect();
        let peers: HashMap<String, String> =
            peer_addrs.into_iter().map(|p| (p.provider, p.addr)).collect();

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
            peers,
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
    /// address comes from the `opsapi::PEER_SLOT` contribution the composition root's
    /// `remote::Stub` wired (collected into [`RouteTable::peers`] at build); the
    /// connection is reused across requests (until a failed call evicts it — see
    /// [`RouteTable::dispatch`]). The address is parsed HERE, lazily, so a bad address
    /// is a per-request `Unavailable` (503), never a startup panic — the `remote::Stub`
    /// contributes the raw string for exactly this reason. In M1's per-svc topology
    /// every op a process serves is local, so this is the seam that lets a unified
    /// front-door route cross-provider without any per-module HTTP shim — exercised
    /// directly in the `RemoteBackend` tests.
    async fn remote_caller(&self, provider: &str) -> Result<Arc<dyn Caller>, Error> {
        let mut cache = self.remotes.lock().await;
        if let Some(c) = cache.get(provider) {
            return Ok(c.clone());
        }
        let addr_str = self.peers.get(provider).ok_or_else(|| {
            Error::unavailable(format!(
                "gateway: no peer contributed for provider {provider:?} \
                 (wire a remote::Stub in this process's main)"
            ))
        })?;
        let addr: SocketAddr = addr_str.parse().map_err(|e| {
            Error::unavailable(format!(
                "gateway: bad peer addr {addr_str:?} for provider {provider:?}: {e}"
            ))
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

async fn handle(front: Arc<FrontDoor>, peer: Option<SocketAddr>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str();
    let path = parts.uri.path();

    let table = front.table().clone();

    // (1) Match. A non-operation route is offered to the HTTP passthrough (Go's
    // reverse proxy: `/admin`, `/accounts/epic` are HTML/browser flows served by
    // another process) — the body is still unconsumed, so the proxy streams it. When
    // no prefix is configured the passthrough returns 404, exactly as before. A proxied
    // request is labelled with its prefix pattern (e.g. `/admin/*`) so `metrics` records
    // it under one bounded series rather than the fixed `"unmatched"`.
    let (op, binding, path_args) = match table.find(method, path) {
        Some((route, args)) => (route.op.clone(), route.binding.clone(), args),
        None => {
            let proxy_pattern = front.proxy.pattern_for(path);
            let mut resp = front.proxy.forward(parts, body, peer).await;
            stamp_route_pattern(&mut resp, proxy_pattern);
            return resp;
        }
    };

    // Every response past a successful match — success, auth failure, decode/dispatch
    // error alike — is stamped with the op's route PATTERN (`op.path`, e.g. `/characters`
    // or `/characters/{id}`), which `metrics::record` reads in place of the absent
    // `MatchedPath` (the front door dispatches from an axum fallback).
    let pattern = op.path.clone();
    let mut resp = dispatch_matched_op(&front, op, binding, path_args, parts.headers, body).await;
    stamp_route_pattern(&mut resp, Some(pattern));
    resp
}

/// Steps (2)–(6) for a matched operation: key check → auth-once → decode → dispatch →
/// encode. Split out of [`handle`] so the caller can stamp the route-pattern label on
/// EVERY outcome (including an early key/auth/decode failure) at one place.
async fn dispatch_matched_op(
    front: &FrontDoor,
    op: Operation,
    binding: OpBinding,
    path_args: PathArgs,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // (2) API-key check: post-match, pre-auth (Decision 5's exact three-way) — the
    // client-class gate every op-dispatched request passes, `AuthReq::None` included.
    if let Err(denial) =
        check_api_key(&*front.key_verifier, api_key_header(&headers).as_deref(), &op.method).await
    {
        return key_denial_response(&denial);
    }

    // (3) Auth-once: the single trust boundary. For AuthPlayer verify the bearer and
    // thread the verified player_id; AuthNone runs with no identity.
    let identity = match op.auth {
        AuthReq::Player => match authenticate(&headers, &*front.verifier).await {
            Ok(id) => id,
            Err(resp) => return resp,
        },
        AuthReq::None => Identity::none(),
    };

    // (4) Decode: bounded body + matched wildcards → the wire request both backends consume.
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

    // (5) Dispatch on the topology-correct backend (Local in-process, else the Remote
    // peer; a Remote failure evicts the cached conn so the next request re-dials).
    let wire_resp = match front.table().dispatch(&op, identity, wire_req).await {
        Ok(r) => r,
        Err(e) => return op_error_response(&e),
    };

    // (6) Reduce the wire response to the external HTTP body + status.
    match (binding.encode)(&wire_resp) {
        // A non-OK domain outcome surfaces as an encode-Err carrying its Status.
        Err(e) => op_error_response(&e),
        // Ok → the op's declared success code with the domain-only body (may be empty).
        Ok((body, _status)) => success_response(op.success, body),
    }
}

/// Inserts the metrics route-pattern label into a response's extensions (a no-op when
/// `pattern` is `None`, e.g. an unmatched request with no proxy prefix, which stays
/// `"unmatched"`). Read back by `metrics::record`.
fn stamp_route_pattern(resp: &mut Response, pattern: Option<String>) {
    if let Some(p) = pattern {
        resp.extensions_mut().insert(httpmw::RoutePattern::new(p));
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

/// Extracts the API key from the `X-Api-Key` header, or `None` (header names match
/// case-insensitively). A non-UTF-8 value reads as absent — it cannot match any key.
fn api_key_header(headers: &HeaderMap) -> Option<String> {
    headers.get("x-api-key")?.to_str().ok().map(str::to_string)
}

/// Writes a [`KeyDenial`] as its HTTP response: the denial's domain [`Status`] mapped
/// to its HTTP code (401/401/403) with the plane-independent message.
fn key_denial_response(denial: &KeyDenial) -> Response {
    let code = StatusCode::from_u16(denial.status().http())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(code, denial.message())
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
mod tests;

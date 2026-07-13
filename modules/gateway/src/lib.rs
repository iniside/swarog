//! `gateway` — the front-door lifecycle module, hosted ONLY by the front processes
//! (the monolith `cmd/server` and `cmd/gateway-svc`; a domain `*-svc` NEVER hosts it,
//! serving its ops over the internal mTLS edge instead — archcheck-enforced). It
//! fronts the process's op surface on TWO public planes through ONE shared façade
//! ([`FrontDoor`]):
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
//!      shadows `/healthz`/`/readyz` (added by `app`).
//!   2. **API-key check** (post-match, pre-auth): every op-dispatched request must
//!      carry an `X-Api-Key` header naming a known, unrevoked key whose policy allows
//!      the matched method (see [`KeyVerifier`]) — missing → 401, unknown/revoked →
//!      401, policy miss → 403. Non-op routes (`/healthz`, `/metrics`,
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
//!      [`RemoteBackend`] dialing the owning peer (a Remote *transport* failure
//!      evicts the cached connection so the next request re-dials; a definitive
//!      peer answer keeps it).
//!   6. Reduce the wire response via `OpBinding::encode` — an encode-`Err` carries the
//!      domain `Status` (→ its HTTP code); an `Ok` writes the op's declared `success`
//!      code with the domain body.
//!
//! A player-QUIC request runs the same match/auth/dispatch, minus the HTTP
//! translation — see [`FrontDoor::player_handler`] for the pinned response grammar.
//!
//! ## Lazy route table (the init-ordering sidestep) + eager startup validation
//! Modules contribute their `OpSet`s during their own `init`, and the gateway's
//! `init` may run first. So the SERVING table is NOT built during `init`: the
//! [`FrontDoor`] holds a [`std::sync::OnceLock`] and builds the table from
//! `ctx.contributions(...)` on the FIRST request — by which time every module's
//! `init` has run (requests only arrive after `app::run` finishes Build and starts
//! serving). But collisions in the contributed slots (two modules claiming the same
//! method id, two routes matching the same request set, two peers for one provider)
//! must not lurk until the first request hits them: [`Gateway::start`] — which runs
//! after ALL module `init`s — eagerly calls [`FrontDoor::build_table`] once, turning
//! any such collision into a loud startup failure in BOTH topologies. The lazy path
//! then rebuilds without re-checking (validation has already passed).

mod backend;
pub mod conformance;
mod keys;
mod proxy;
mod verifier;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use contrib::Slots;
use lifecycle::{Context, Module};
use opsapi::{
    AuthReq, Caller, Error, Identity, LocalInvoker, OpBinding, Operation, PathArgs, Seg, Status,
    parse_pattern, pattern_overlaps,
};
use serde_json::value::RawValue;

pub use backend::{LocalBackend, OperationBackend, RemoteBackend};
pub use keys::{AllowAllKeyVerifier, KeyVerifier, LookupUnavailable, RealKeyVerifier};
pub use verifier::{DevSessionVerifier, SessionVerifier, SessionsVerifier, VerifyUnavailable};

use keys::{check_api_key, KeyDenial};

/// Caps the request body the gateway buffers before decoding an operation, so a
/// hostile client cannot make the front-handler allocate without bound. 1 MiB is
/// comfortably above any player operation's request (matches Go's `maxBodyBytes`,
/// and `edge::MAX_PLAYER_FRAME` mirrors it on the QUIC plane).
const MAX_BODY_BYTES: usize = 1 << 20;

/// The default whole-credential-admission deadline (`CREDENTIAL_ADMISSION_TIMEOUT_MS`)
/// bounding the front door's api-key check + session verify on BOTH planes. The
/// underlying `edge::Client` bounds only the DIAL (5s), NOT the RPC round-trip, so a
/// hung apikeys/accounts backend would otherwise pin the per-key flight lock and the
/// global lookup permits forever — every request behind it shedding 503. This budget
/// makes the whole admission fail-closed within a bounded time; a fired timeout maps
/// into the EXISTING Unavailable class (503 / player `Unavailable` envelope), never a
/// new status. The front processes' `main.rs` parse `CREDENTIAL_ADMISSION_TIMEOUT_MS`
/// and override it via [`Gateway::with_admission_budget`]; the module never reads env.
const DEFAULT_ADMISSION_BUDGET: Duration = Duration::from_millis(5000);

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
    /// The credential-admission budget the composition root parsed from
    /// `CREDENTIAL_ADMISSION_TIMEOUT_MS` (via [`Gateway::with_admission_budget`]).
    /// `None` (the [`Gateway::new`] default) leaves the [`FrontDoor`] on
    /// [`DEFAULT_ADMISSION_BUDGET`]. Topology/env lives in `cmd/*`, never read here.
    admission_budget: Option<Duration>,
    /// The [`FrontDoor`] built and mounted in `init`, stored so `start` can eagerly
    /// validate the route table (a collision then fails startup, not the first
    /// request). Interior-mutable because `Module` phases take `&self`; set exactly
    /// once in `init`, read in `start`.
    front_door: OnceLock<Arc<FrontDoor>>,
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
            admission_budget: None,
            front_door: OnceLock::new(),
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
            admission_budget: None,
            front_door: OnceLock::new(),
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

    /// Overrides the credential-admission budget (builder-style, mirrors
    /// [`Gateway::with_passthrough`]). The composition root parses
    /// `CREDENTIAL_ADMISSION_TIMEOUT_MS` and calls this; absent the call the
    /// [`FrontDoor`] stays on [`DEFAULT_ADMISSION_BUDGET`]. See [`FrontDoor::admit`].
    pub fn with_admission_budget(mut self, budget: Duration) -> Self {
        self.admission_budget = Some(budget);
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
        let mut front_door = FrontDoor::new(
            ctx.slots().clone(),
            verifier,
            key_verifier,
            self.passthroughs.clone(),
        );
        if let Some(budget) = self.admission_budget {
            front_door = front_door.with_admission_budget(budget);
        }
        let front_door = Arc::new(front_door);
        ctx.mount(front_door.router());
        if let Some(shared) = &self.player_edge {
            shared.lock().unwrap().set_handler(front_door.player_handler());
        }
        // Stash the façade so `start` can eagerly validate the route table.
        let _ = self.front_door.set(front_door);
        Ok(())
    }

    /// Eager route-table validation. `start` runs after EVERY module's `init`, so all
    /// `opsapi` slot contributions are present — building the table here turns a
    /// duplicate method id, an overlapping verb+path route, or a duplicate peer
    /// provider into a loud startup failure in BOTH topologies (monolith and
    /// gateway-svc), instead of a silent last-write-wins hybrid discovered on the
    /// first request. The built table is discarded; the [`FrontDoor`] rebuilds it
    /// lazily on first request (validation has passed by then).
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let front_door = self
            .front_door
            .get()
            .expect("gateway: init runs before start and sets the FrontDoor");
        front_door.build_table()?;
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
    /// The whole-credential-admission deadline both planes wrap [`FrontDoor::admit`] in
    /// (default [`DEFAULT_ADMISSION_BUDGET`], overridden from
    /// `CREDENTIAL_ADMISSION_TIMEOUT_MS` via [`Gateway::with_admission_budget`]).
    admission_budget: Duration,
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
            admission_budget: DEFAULT_ADMISSION_BUDGET,
            table: OnceLock::new(),
            proxy: proxy::ProxyTable::from_routes(passthroughs),
        }
    }

    /// Overrides the credential-admission budget (builder-style). Called by
    /// [`Gateway::init`] when the composition root parsed
    /// `CREDENTIAL_ADMISSION_TIMEOUT_MS`; the unit tests use it to pin a small budget
    /// for the hung-backend proofs.
    pub fn with_admission_budget(mut self, budget: Duration) -> FrontDoor {
        self.admission_budget = budget;
        self
    }

    /// The route table, built from the slots on first access. By first-request time
    /// every module's `init` (where `OpSet`s are contributed) has completed AND
    /// [`Gateway::start`] has already validated the same slots via [`build_table`],
    /// so the lazy build here cannot surface a NEW collision — hence the `expect`.
    ///
    /// [`build_table`]: FrontDoor::build_table
    fn table(&self) -> &Arc<RouteTable> {
        self.table.get_or_init(|| {
            self.build_table()
                .expect("route table already validated in Gateway::start")
        })
    }

    /// Builds the route table from the current slots, failing on any collision (see
    /// [`RouteTable::build`]). Shared by the eager startup validation
    /// ([`Gateway::start`]) and the lazy first-request path ([`FrontDoor::table`]).
    fn build_table(&self) -> anyhow::Result<Arc<RouteTable>> {
        Ok(Arc::new(RouteTable::build(&self.slots)?))
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

        // (3)+(4) Credential admission — the api-key check THEN (for a player-auth op)
        // the session verify, the WHOLE thing bounded by the process's admission
        // budget (see [`FrontDoor::admit`]). A hung key/session backend surfaces as the
        // pinned `Unavailable` envelope, never a leaked handler. The denial renders
        // through the SAME `{status, err}` grammar as before — no new status.
        let identity = match self
            .admit(api_key.as_deref(), token.as_deref(), route.op.auth, &route.op.method)
            .await
        {
            Ok(id) => id,
            Err(denial) => return front_envelope(denial.status(), denial.message()),
        };

        // (5) Dispatch; the wire response IS the player response (envelope included).
        match table.dispatch(&route.op, identity, payload).await {
            Ok(bytes) => bytes,
            Err(e) => front_envelope(e.status, &e.msg),
        }
    }

    /// The ONE credential-admission seam both public planes funnel through: the api-key
    /// policy check THEN — for an `AuthReq::Player` op — the session verify, resolved to
    /// the caller [`Identity`] threaded downstream (`Identity::none()` for `AuthReq::None`).
    /// The WHOLE thing is wrapped in a single [`tokio::time::timeout`] on the process's
    /// [`FrontDoor::admission_budget`], because the underlying `edge::Client` bounds only
    /// the DIAL, not the RPC round-trip: without this a hung apikeys/accounts backend
    /// pins the per-key flight lock ([`keys::RealKeyVerifier`]) and the global lookup
    /// permits forever, shedding 503 for every request behind it.
    ///
    /// A fired timeout is [`AdmissionDenial::Timeout`], which each front renders into the
    /// SAME existing `Unavailable` class it already uses for a verifier outage (503 on
    /// HTTP, the `Unavailable` envelope on the player plane) — zero new status mappings.
    ///
    /// **RAII-safety the timeout relies on:** dropping the timed-out future is safe.
    /// `RealKeyVerifier`'s flight lock is an `Arc<tokio::sync::Mutex<()>>` held via
    /// `lock_owned()` — the drop releases it, and its `Weak` table entry then upgrades to
    /// nothing and is purged, so the NEXT lookup for that key mints a fresh flight; the
    /// TTL cache is written ONLY on a completed `Ok`, never on cancel/`Err`, so a
    /// cancelled admission poisons nothing. A healed backend therefore serves the very
    /// next request for the same key.
    pub(crate) async fn admit(
        &self,
        api_key: Option<&str>,
        bearer: Option<&str>,
        auth: AuthReq,
        method: &str,
    ) -> Result<Identity, AdmissionDenial> {
        match tokio::time::timeout(
            self.admission_budget,
            self.admit_inner(api_key, bearer, auth, method),
        )
        .await
        {
            Ok(result) => result,
            // The whole api-key + session admission exceeded the budget — a hung
            // backend. Fail closed into the existing Unavailable class.
            Err(_elapsed) => Err(AdmissionDenial::Timeout),
        }
    }

    /// The unbounded body of [`FrontDoor::admit`] — the key check then the session
    /// verify. Kept separate so the single `timeout` in `admit` covers BOTH awaits as
    /// one deadline (a hung key lookup and a hung session verify are equally bounded).
    async fn admit_inner(
        &self,
        api_key: Option<&str>,
        bearer: Option<&str>,
        auth: AuthReq,
        method: &str,
    ) -> Result<Identity, AdmissionDenial> {
        // (a) API-key check — post-match, pre-auth (Decision 5's exact three-way).
        check_api_key(&*self.key_verifier, api_key, method)
            .await
            .map_err(AdmissionDenial::Key)?;

        // (b) Auth-once: the single trust boundary. For a player-auth op the bearer is
        // required and verified; only the VERIFIED player_id becomes the identity.
        match auth {
            AuthReq::Player => {
                let Some(token) = bearer else {
                    return Err(AdmissionDenial::MissingBearer);
                };
                match self.verifier.verify(token).await {
                    Ok(Some(pid)) => Ok(Identity::player(pid)),
                    Ok(None) => Err(AdmissionDenial::InvalidSession),
                    Err(VerifyUnavailable) => Err(AdmissionDenial::SessionUnavailable),
                }
            }
            AuthReq::None => Ok(Identity::none()),
        }
    }
}

/// Why the front refused a request at the credential-admission seam ([`FrontDoor::admit`]).
/// One evaluation serves BOTH planes; each front renders this into its own response
/// grammar (an HTTP status via [`Status::http`], or the player `{status, err}` envelope),
/// so the api-key/session/timeout → status mapping cannot drift between planes and adds
/// no status class the fronts didn't already use.
pub(crate) enum AdmissionDenial {
    /// The api-key check refused the request (missing / invalid / policy miss / the
    /// verifier itself unavailable — see [`KeyDenial`]).
    Key(KeyDenial),
    /// An `AuthReq::Player` op arrived with no bearer token.
    MissingBearer,
    /// The bearer was definitively rejected by the session verifier.
    InvalidSession,
    /// The session verifier could not answer (accounts outage / load-shed).
    SessionUnavailable,
    /// The whole admission (api-key + session) exceeded the admission budget — a hung
    /// backend. Rendered in the SAME `Unavailable` class as a verifier outage.
    Timeout,
}

impl AdmissionDenial {
    /// The domain [`Status`] each front maps to its transport code — reusing ONLY the
    /// classes the fronts already emit (Unauthorized / Forbidden / Unavailable).
    pub(crate) fn status(&self) -> Status {
        match self {
            AdmissionDenial::Key(k) => k.status(),
            AdmissionDenial::MissingBearer | AdmissionDenial::InvalidSession => {
                Status::Unauthorized
            }
            AdmissionDenial::SessionUnavailable | AdmissionDenial::Timeout => Status::Unavailable,
        }
    }

    /// The plane-independent denial message.
    pub(crate) fn message(&self) -> &'static str {
        match self {
            AdmissionDenial::Key(k) => k.message(),
            AdmissionDenial::MissingBearer | AdmissionDenial::InvalidSession => "unauthorized",
            AdmissionDenial::SessionUnavailable => "session verification unavailable",
            AdmissionDenial::Timeout => {
                "credential admission timed out (CREDENTIAL_ADMISSION_TIMEOUT_MS)"
            }
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
/// `Seg`/parsing/overlap-detection live in `opsapi` — the shared authority also
/// used by `routecheck` (see [`pattern_overlaps`]'s doc).
struct Route {
    op: Operation,
    binding: OpBinding,
    pattern: Vec<Seg>,
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
    /// A `std::sync::Mutex` locked only for synchronous get/insert/remove — never
    /// held across an await (the `keys.rs` cache rule), so a slow dial to one
    /// provider can never block cache hits for the others.
    remotes: Mutex<HashMap<String, Arc<dyn Caller>>>,
    /// Per-provider dial flights (the `keys.rs` singleflight shape): while one
    /// request dials a provider, concurrent requests to the SAME provider queue on
    /// that provider's flight mutex and re-check the cache after it; requests to
    /// OTHER providers never touch it — a dead peer stalls only its own routes.
    /// Entries are `Weak` so a finished flight self-GCs (dead entries are purged on
    /// each [`RouteTable::flight`] call — the map is bounded by the fleet's provider
    /// count, so no saturation shed is needed here, unlike the attacker-keyed
    /// api-key flight table).
    flights: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl RouteTable {
    /// Reads the four opsapi slots and assembles the table, FAILING on any collision:
    /// two `OpBinding`s / `LocalOp`s / `Operation`s for one method, two `PeerAddr`s for
    /// one provider, or two operations whose verb + path pattern match the same request
    /// set. The slots are append-only multi-value contributions, so a duplicate is a
    /// wiring bug (two modules claiming the same op) that would otherwise resolve to a
    /// silent last-write-wins hybrid — [`Gateway::start`] calls this eagerly so the bug
    /// is a loud startup failure. An `Operation` with no paired `OpBinding` is a
    /// different wiring bug and is skipped rather than bound to an undecodable route
    /// (mirrors Go's `buildOpsMux`).
    fn build(slots: &Slots) -> anyhow::Result<RouteTable> {
        let operations: Vec<Operation> = slots.contributions(opsapi::SLOT);
        let bindings: Vec<OpBinding> = slots.contributions(opsapi::BINDING_SLOT);
        let locals: Vec<opsapi::LocalOp> = slots.contributions(opsapi::LOCAL_SLOT);
        let peer_addrs: Vec<opsapi::PeerAddr> = slots.contributions(opsapi::PEER_SLOT);

        let mut binding_by_method: HashMap<String, OpBinding> = HashMap::new();
        for b in bindings {
            if binding_by_method.contains_key(&b.method) {
                anyhow::bail!(
                    "gateway: duplicate OpBinding for method {:?} — two modules \
                     contributed a binding for the same op",
                    b.method
                );
            }
            binding_by_method.insert(b.method.clone(), b);
        }

        let mut invokers: HashMap<String, LocalInvoker> = HashMap::new();
        for l in locals {
            if invokers.contains_key(&l.method) {
                anyhow::bail!(
                    "gateway: duplicate LocalOp for method {:?} — two modules claim \
                     to serve the same op locally",
                    l.method
                );
            }
            invokers.insert(l.method.clone(), l.invoke);
        }

        let mut peers: HashMap<String, String> = HashMap::new();
        for p in peer_addrs {
            if let Some(existing) = peers.get(&p.provider) {
                anyhow::bail!(
                    "gateway: duplicate peer for provider {:?} — {:?} and {:?} both \
                     contributed (two remote::Stubs for one provider)",
                    p.provider,
                    existing,
                    p.addr
                );
            }
            peers.insert(p.provider, p.addr);
        }

        let mut routes: Vec<Route> = Vec::new();
        for op in operations {
            if routes.iter().any(|r| r.op.method == op.method) {
                anyhow::bail!(
                    "gateway: duplicate Operation for method {:?} — two modules \
                     contributed an operation with the same method id",
                    op.method
                );
            }
            let Some(binding) = binding_by_method.get(&op.method).cloned() else {
                tracing::warn!(method = %op.method, "gateway: operation has no binding; skipping");
                continue;
            };
            let pattern = parse_pattern(&op.path);
            if let Some(existing) = routes
                .iter()
                .find(|r| r.op.verb.eq_ignore_ascii_case(&op.verb) && pattern_overlaps(&r.pattern, &pattern))
            {
                anyhow::bail!(
                    "gateway: route {} {:?} and {} {:?} may overlap — the same request \
                     could match both (methods {:?} and {:?})",
                    existing.op.verb,
                    existing.op.path,
                    op.verb,
                    op.path,
                    existing.op.method,
                    op.method
                );
            }
            routes.push(Route { op, binding, pattern });
        }

        Ok(RouteTable {
            routes,
            invokers: Arc::new(invokers),
            peers,
            remotes: Mutex::new(HashMap::new()),
            flights: Mutex::new(HashMap::new()),
        })
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
    /// **Evict-on-transport-error:** a Remote *transport* failure drops that
    /// provider's cached `Arc<dyn Caller>`, so the NEXT request re-dials instead of
    /// reusing a dead connection forever (a provider restart would otherwise brick
    /// the route permanently). A DEFINITIVE peer answer
    /// ([`opsapi::Status::is_definitive_answer`], e.g. the typed unknown-method →
    /// `NotFound`) proves the connection is healthy and is NOT evicted; anything
    /// else — `Unavailable`, `Internal`, any future status — still evicts (the safe
    /// default). This is the reset idea of `remote::Reconnecting` WITHOUT the
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
                // Skip eviction ONLY on a definitive peer answer (the connection
                // demonstrably carried a round trip); every other error — including
                // any future status — evicts, keeping eviction the safe default.
                if matches!(&result, Err(e) if !e.status.is_definitive_answer()) {
                    let mut cache = self.remotes.lock().unwrap();
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
    ///
    /// **Per-provider singleflight (the `keys.rs` flight shape):** no lock is ever
    /// held across an await. A cache miss resolves this provider's flight mutex
    /// synchronously, awaits ONLY that flight, re-checks the cache (a concurrent
    /// winner's client is reused, not re-dialed), then dials — bounded by the edge
    /// client's `DIAL_DEADLINE` — and publishes the client. A provider whose dial
    /// hangs therefore stalls only its own requests, never first dials to healthy
    /// peers (previously one `tokio::sync::Mutex` was held across the dial await,
    /// serialising ALL providers behind the slowest).
    async fn remote_caller(&self, provider: &str) -> Result<Arc<dyn Caller>, Error> {
        if let Some(c) = self.cached_remote(provider) {
            return Ok(c);
        }
        let flight = self.flight(provider);
        let _flight_guard = flight.lock_owned().await;
        if let Some(c) = self.cached_remote(provider) {
            return Ok(c);
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
        self.remotes.lock().unwrap().insert(provider.to_string(), caller.clone());
        Ok(caller)
    }

    /// Serves `provider`'s client from the cache. The lock is never held across an
    /// await.
    fn cached_remote(&self, provider: &str) -> Option<Arc<dyn Caller>> {
        self.remotes.lock().unwrap().get(provider).cloned()
    }

    /// Resolves (or creates) `provider`'s dial-flight mutex — synchronous, the
    /// flights lock is never held across an await. Dead flights (every holder
    /// finished, `Weak` no longer upgrades) are purged on each call; the map is
    /// bounded by the provider count, so unlike `keys.rs` there is no saturation
    /// shed.
    fn flight(&self, provider: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut flights = self.flights.lock().unwrap();
        flights.retain(|_, weak| weak.strong_count() != 0);
        if let Some(lock) = flights.get(provider).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        flights.insert(provider.to_string(), Arc::downgrade(&lock));
        lock
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
    // (2)+(3) Credential admission: the api-key check (post-match, pre-auth —
    // Decision 5's exact three-way, `AuthReq::None` included) THEN, for an AuthPlayer
    // op, the auth-once bearer verify — the WHOLE thing bounded by the process's
    // admission budget via [`FrontDoor::admit`], so a hung apikeys/accounts backend
    // surfaces as the existing 503 class instead of pinning the handler. The denial →
    // HTTP mapping is byte-identical to the pre-admit split paths.
    let identity = match front
        .admit(
            api_key_header(&headers).as_deref(),
            bearer(&headers).as_deref(),
            op.auth,
            &op.method,
        )
        .await
    {
        Ok(id) => id,
        Err(denial) => return admission_denial_response(&denial),
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
    // peer; a Remote transport failure evicts the cached conn so the next request
    // re-dials).
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
        Ok(Some(pid)) => Ok(Identity::player(pid)),
        Ok(None) => Err(error_response(StatusCode::UNAUTHORIZED, "unauthorized")),
        Err(VerifyUnavailable) => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "session verification unavailable",
        )),
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

/// Writes an [`AdmissionDenial`] as its HTTP response: the denial's domain [`Status`]
/// mapped to its HTTP code (401/403/503) with the plane-independent message — the same
/// codes and bodies the pre-admit split paths (key check + `authenticate`) produced.
fn admission_denial_response(denial: &AdmissionDenial) -> Response {
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
// Path pattern matching (the `{wild}` support the route table needs). `Seg`,
// `parse_pattern`, and the overlap predicate live in `opsapi` — the shared
// authority `routecheck` also calls (see `opsapi::pattern_overlaps`'s doc).
// ---------------------------------------------------------------------------

/// Splits a path into its non-empty segments (`"/characters/42"` → `["characters","42"]`).
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
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

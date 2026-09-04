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
//!      [`RemoteBackend`] over the provider's self-healing `remote::Pool` (round-robin
//!      across the resolved instance set; a dead instance recovers internally via the
//!      pool's per-instance reconnect + probe skip-dead — the pool is permanent, never
//!      evicted on a call error).
//!   6. Reduce the wire response via `OpBinding::encode` — an encode-`Err` carries the
//!      domain `Status` (→ its HTTP code); an `Ok` writes the op's declared `success`
//!      code with the domain body.
//!
//! A player-QUIC request runs the same match/auth/dispatch, minus the HTTP
//! translation — see [`FrontDoor::player_handler`] for the pinned response grammar.
//!
//! Beside the fallback the router carries ONE fixed route, `GET /push`: the server→client
//! WebSocket (see `push_ws`). It is credentialed through the same authorities — the api
//! key is checked for presence and validity, the bearer through the same verifier — but
//! answers a typed close instead of an HTTP status, because a client only ever sees the
//! socket.
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
//!
//! ## D2 routing-as-data (managed `cmd/gateway-svc`) — the dynamic table
//! A dedicated front-door process hosts no provider module, so its slots carry NO
//! `Operation`s: there is nothing to build a route from at compile time. When the
//! composition root calls [`Gateway::with_describe_routing`], the [`FrontDoor`] instead
//! holds a SWAPPABLE table ([`TableCell::Dynamic`]) that a `start`-driven loop rebuilds
//! from each peer's runtime `__describe` manifest (`opsapi::databind` turns every
//! `OpManifest` into an `Operation`+`OpBinding`), re-fetched on [`DESCRIBE_REFRESH_INTERVAL`]
//! so a peer that was DOWN at boot is routed once it comes up. The first pass runs
//! synchronously in `start` (routes ready before serving; a BUILD rejection — a collision
//! across describe-contributed peers via the SAME `build_from_parts` authority, or a peer
//! advertising a method outside its own provider prefix — fails startup loudly), a describe
//! FETCH failure is tolerated (error-keeps-last per peer). The
//! module stays topology-blind — the composition root decides the mode, exactly as it
//! decides [`Gateway::with_player_edge`]. The monolith/standalone path above is UNCHANGED.

mod backend;
pub mod conformance;
mod keys;
mod proxy;
mod push_ws;
mod verifier;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
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
pub use push_ws::PushLimits;
pub use keys::{AllowAllKeyVerifier, KeyVerifier, LookupUnavailable, RealKeyVerifier};
pub use verifier::{DevSessionVerifier, SessionVerifier, SessionsVerifier, VerifyUnavailable};

use keys::{check_api_key, KeyCheck, KeyDenial};
use push_ws::PushHub;

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

/// How often the describe-driven route table (D2, managed `cmd/gateway-svc`) re-fetches
/// each peer's `__describe` manifest and rebuilds itself. Mirrors `remote`'s
/// `POOL_REFRESH_INTERVAL` (5s) cadence: frequent enough that a peer that was DOWN at boot
/// (or a describe change) is picked up within seconds without restarting the front door, but
/// not per-request. A core-leaf constant (never reads env — Hard Constraint 1/5); this is
/// dev-fleet scaffolding, so a fixed value is sufficient.
///
/// SCOPE — what refreshes is each peer's MANIFEST, not the fleet. The per-provider peer
/// ADDRESS SET is the BOOT snapshot `opsapi::PEER_SLOT` carries (contributed in `init`
/// before any I/O, so it cannot re-resolve), exactly as for the slot-built C2 table — see
/// the SHARED-VS-SEPARATE note on the dispatch pool. So "dynamic route table" means the
/// OPS a known peer advertises can change while the process runs; a scale event that adds
/// or moves an instance reaches HTTP dispatch on the next process boot. Deliberate for D2;
/// a live route-table re-resolve is out of scope.
const DESCRIBE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Bound on ONE peer's `__describe` fetch inside a refresh pass ([`DescribeRouter::refresh_once`]).
/// A peer that times out is treated exactly like a peer whose fetch ERRORED: keep-last (it retains
/// its prior manifest, or stays absent if never seen), never a dropped route table.
///
/// This value is squeezed between a FLOOR and a CEILING, and the floor is the one that is easy
/// to get wrong:
///
/// ```text
///   5000ms  = edge's DIAL_DEADLINE      (cold-dial floor — see below)
/// + ~1000ms   margin for the describe round-trip on the freshly-dialed connection
/// = 6000ms  = DESCRIBE_PEER_TIMEOUT
/// < 30000ms = edge's EDGE_STREAM_GRACE  (the server-side ceiling this exists to beat)
/// ```
///
/// FLOOR — it must exceed a COLD DIAL plus one describe round-trip. The fetch it wraps is
/// `remote::describe` → `Pool::call` → `Reconnecting::get()` → `edge::Client::dial`, and that
/// dial is itself allowed 5s (`DIAL_DEADLINE`, `core/edge/src/client.rs:34,82` — `pub(crate)`,
/// hence named by file:line rather than imported). `Reconnecting::get` caches the connection
/// only on SUCCESS (`core/remote/src/lib.rs:331-336`), so cancelling mid-handshake DISCARDS the
/// partial dial: the next pass starts from zero. A timeout below the dial budget therefore does
/// not merely delay a slow peer, it can permanently EXCLUDE a reachable one — gateway and peers
/// booting together, a legitimate ~2s QUIC+mTLS handshake, and every pass cancels it at the same
/// point, so the provider never enters the table and every op to it 404s. The first pass is
/// awaited in `Gateway::start` precisely so routes are ready before serving; silently dropping a
/// reachable peer from THAT table is the worst outcome available here, strictly worse than
/// waiting.
///
/// CEILING — the server bounds a stalled internal stream only at `edge`'s `EDGE_STREAM_GRACE`
/// (30s) and `remote`'s client `describe` adds no timeout of its own (`edge::Client` bounds the
/// DIAL, not the round-trip), so without this bound a half-alive peer holds the pass — and thus
/// startup — for 30s. That is the defect this constant closes.
///
/// It does NOT sit under [`DESCRIBE_STOP_GRACE`] (2s), and deliberately so: a stop landing
/// mid-pass force-aborts the task rather than draining it (bounded and safe — see that
/// constant). Shutdown politeness loses to boot correctness, and the grace cannot be raised to
/// chase this because it shares a budget with `POOL_STOP_BUDGET`.
///
/// NO separate whole-pass budget exists, deliberately. Per-peer timeout x bounded fan-out
/// already bounds a pass to `ceil(N_peers / DESCRIBE_FETCH_CONCURRENCY) * DESCRIBE_PEER_TIMEOUT`
/// — one wave, 6s, for any fleet within the concurrency below. A second, overall deadline would
/// add an invariant nothing enforces — `PASS_BUDGET >= wave_count * DESCRIBE_PEER_TIMEOUT` —
/// and the failure mode of getting it wrong is the same silent exclusion of a slow-but-
/// REACHABLE peer described above. Do not "add the missing budget": raise the concurrency
/// (fewer waves) instead.
const DESCRIBE_PEER_TIMEOUT: Duration = Duration::from_secs(6);

/// How many peers a refresh pass fetches CONCURRENTLY. Bounded rather than unbounded fan-out so
/// a large fleet cannot open one QUIC dial per provider at once, and chosen `>=` the provider
/// count (11 today) so a pass over the real fleet is ONE wave: the pass bound is
/// `ceil(N / this) * DESCRIBE_PEER_TIMEOUT`, and with a per-peer timeout that must clear a 5s
/// cold dial ([`DESCRIBE_PEER_TIMEOUT`]) a second wave would double an already-long boot pass.
/// Growing the fleet past this number costs a wave; that is the only reason to change it.
const DESCRIBE_FETCH_CONCURRENCY: usize = 16;

/// Grace given to the describe-refresh task to observe the stop signal before
/// [`Gateway::stop`] aborts it. Reuses the 2s shape of `remote`'s `PROBE_STOP_GRACE`
/// (the other module-owned background loop torn down this way).
///
/// INVARIANT (the WHOLE `stop`, not just this grace): `lifecycle::App` wraps each module in
/// `timeout(MODULE_STOP_GRACE_MS, m.stop())` and DROPS the future on elapse, so the SUM of
/// everything `Gateway::stop` awaits must stay strictly under it. `Gateway::stop` has exactly
/// three sequential awaits, each separately bounded:
///
/// ```text
///   PUSH_STOP_GRACE       500ms   (typed close flushed to every live socket, then abort)
/// + DESCRIBE_STOP_GRACE  2000ms   (task join, then abort)
/// + POOL_STOP_BUDGET     2000ms   (concurrent dispatch-pool teardown)
/// = 4500ms  <  5000ms = MODULE_STOP_GRACE_MS (default)   → 500ms headroom
/// ```
///
/// Adding a fourth awaited step to `stop`, or raising any of the three constants, requires
/// re-checking that sum. If it exceeded the budget the app would abandon `stop` mid-flight — the very
/// leak this ownership exists to close, plus (for pools) a teardown strictly worse than none
/// at all (see [`RouteTable::stop_pools`]).
///
/// MID-PASS STOP — the task is FORCE-ABORTED, by design, and that is fine. The loop's `select!`
/// observes the stop signal only BETWEEN passes (at `ticker.tick()`); the `biased;` there only
/// guarantees the stop wins at that boundary, it cannot interrupt a pass already running. A pass
/// is bounded at `ceil(N_peers / DESCRIBE_FETCH_CONCURRENCY) * DESCRIBE_PEER_TIMEOUT` — one wave
/// of 6s for the real fleet, since the concurrency is >= the provider count — which EXCEEDS this
/// 2s grace. So a stop landing mid-pass hits the abort path rather than draining, and the grace
/// is not raised to chase it (it shares `MODULE_STOP_GRACE_MS` with `POOL_STOP_BUDGET`, per the
/// arithmetic above) because [`DESCRIBE_PEER_TIMEOUT`]'s floor — a cold QUIC dial — is a boot-
/// correctness requirement that outranks shutdown politeness.
///
/// The abort is safe by construction and leaks nothing: the fetch tasks live in a `JoinSet`
/// local to the pass future, so dropping that future aborts every in-flight fetch with it, and
/// `refresh_once` mutates nothing observable (`last_known`, then `install_table`) until its
/// synchronous tail after all fetches have been joined — there is no torn or half-installed
/// table. What an abort costs is one partially-completed refresh of a process that is stopping
/// anyway.
///
/// KNOWN GAP (repo-wide, unenforced convention — NOT closed here): the
/// `< MODULE_STOP_GRACE_MS` invariant above is prose only. `core/app` parses that env var
/// with no floor or clamp, so `MODULE_STOP_GRACE_MS=1000` inverts the relation: `App::stop`
/// drops this whole `stop` future at 1s, before the inner 2s timeout fires, leaving the
/// `JoinHandle` detached and never aborted — the original leak, restored by an env var. The
/// same unenforced convention already exists at `modules/scheduler/src/lib.rs` (4s task
/// grace) and `core/remote/src/lib.rs` (`PROBE_STOP_GRACE`), so the fix belongs in `core/app`
/// (a floor on the parsed knob, or a published minimum) rather than in any one module.
const DESCRIBE_STOP_GRACE: Duration = Duration::from_secs(2);

/// Budget for tearing down ALL of the installed route table's dispatch `remote::Pool`s
/// ([`RouteTable::stop_pools`]) — the second and last awaited step of [`Gateway::stop`]. See
/// the arithmetic block on [`DESCRIBE_STOP_GRACE`]: 2000 + 2000 < 5000ms, the whole point
/// being that `App::stop` must never be the thing that cancels this.
///
/// It is a WHOLE-fan-out budget, not per pool, which is why the fan-out is concurrent: a
/// single `Pool::stop` can legitimately take ~2s (`remote`'s probe grace) and, with a
/// half-open peer, up to `edge`'s 5s `DIAL_DEADLINE` on the connection close. Concurrency
/// makes the fleet's provider count irrelevant to the budget; the timeout then bounds the
/// worst straggler. A pool aborted by this budget loses only its graceful CONNECTION_CLOSE —
/// the process is about to exit regardless.
///
/// KNOWN GAP (belongs in `core/remote`, deliberately NOT closed here): `Pool::stop` empties
/// `instances` but sets no `stopped` fence, and `stop_pools` drains `pools` while leaving the
/// same pool reachable in `remotes`. A dispatch arriving AFTER stop would therefore reach
/// `Pool::call` → `refresh()`, re-resolve the constant address list, reconcile from empty and
/// MINT fresh instances — new probe tasks, new QUIC dials — with no entry in `pools` and no
/// owner able to stop them. This is unreachable today ONLY because of an ordering invariant
/// owned by another crate: `app::run` fully returns from `serve_http` and `shutdown(grace)`s
/// BOTH QUIC fronts BEFORE `ordered_teardown` calls any module's `stop`, so no request can be
/// in flight here. The robust fix is a `stopped: AtomicBool` on `remote::Pool` checked in
/// `refresh()`/`call()` (fail with the existing all-down error). `remote::Stub::stop` has the
/// identical hole, so the fence belongs there, once, for both.
const POOL_STOP_BUDGET: Duration = Duration::from_secs(2);

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
    /// The `/push` WebSocket bounds the composition root parsed from env (via
    /// [`Gateway::with_push_limits`]). `None` (the [`Gateway::new`] default) leaves the
    /// [`FrontDoor`] on [`PushLimits::default`]. Topology/env lives in `cmd/*`, never
    /// read here.
    push_limits: Option<PushLimits>,
    /// D2 routing-as-data: when set (by the managed `cmd/gateway-svc` via
    /// [`Gateway::with_describe_routing`]), the route table is NOT built from the process
    /// slots (which carry no `Operation`s in that process) but from each peer's runtime
    /// `__describe` manifest, re-fetched on [`DESCRIBE_REFRESH_INTERVAL`]. The module stays
    /// topology-blind: the composition root decides this exactly as it decides
    /// [`Gateway::with_player_edge`]/passthroughs — the module never reads env. `false` (the
    /// default) is the monolith/standalone lazy-from-slots path, UNCHANGED.
    describe_routing: bool,
    /// The [`FrontDoor`] built and mounted in `init`, stored so `start` can eagerly
    /// validate the route table (a collision then fails startup, not the first
    /// request). Interior-mutable because `Module` phases take `&self`; set exactly
    /// once in `init`, read in `start`.
    front_door: OnceLock<Arc<FrontDoor>>,
    /// Stop signal for the D2 describe-refresh loop (`Some` only between `start` and
    /// `stop`, and only on the describe-routing path). Interior-mutable because the
    /// `Module` phases take `&self`; `stop` `take`s it, so a second `stop` is a no-op.
    stop_tx: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    /// Join handle of that same loop — the module OWNS the task it spawns (lifecycle
    /// constraint 8), so `stop` joins it (or aborts it after [`DESCRIBE_STOP_GRACE`]),
    /// ending the re-fetch and dropping the describe-FETCHER pools the task alone holds.
    /// The DISPATCH pools are NOT covered by this handle — they belong to the installed
    /// `RouteTable` behind the long-lived `FrontDoor`; `stop` releases those separately
    /// through [`FrontDoor::stop_pools`].
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
            push_limits: None,
            describe_routing: false,
            front_door: OnceLock::new(),
            stop_tx: Mutex::new(None),
            task: Mutex::new(None),
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
            push_limits: None,
            describe_routing: false,
            front_door: OnceLock::new(),
            stop_tx: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    /// Switches this gateway to D2 routing-as-data (builder-style, cmd-injected like
    /// [`Gateway::with_player_edge`]): the route table is built from each peer's runtime
    /// `__describe` manifest instead of the process slots, and re-fetched on
    /// [`DESCRIBE_REFRESH_INTERVAL`] so a peer that was DOWN at boot is routed once it comes
    /// up. The managed `cmd/gateway-svc` calls this; the monolith/standalone never does (its
    /// modules contribute `Operation`s to the slots directly). The module stays
    /// topology-blind — it does not decide the mode, the composition root does.
    pub fn with_describe_routing(mut self) -> Self {
        self.describe_routing = true;
        self
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

    /// Overrides the `/push` WebSocket bounds (builder-style, mirrors
    /// [`Gateway::with_admission_budget`]). The composition root parses the `PUSH_*`
    /// knobs and the trusted-proxy set and calls this; absent the call the front door
    /// stays on [`PushLimits::default`].
    pub fn with_push_limits(mut self, limits: PushLimits) -> Self {
        self.push_limits = Some(limits);
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
        if let Some(limits) = &self.push_limits {
            front_door = front_door.with_push_limits(limits.clone());
        }
        if self.describe_routing {
            // D2: the table is swapped in by the `start`-driven describe refresh, not built
            // lazily from the (op-less) slots. Seeded empty → un-routable (404) until the
            // first successful describe fetch installs real routes (fail-closed cold start).
            front_door = front_door.into_dynamic_routing();
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
    async fn start(&self, ctx: &Context) -> anyhow::Result<()> {
        let front_door = self
            .front_door
            .get()
            .expect("gateway: init runs before start and sets the FrontDoor")
            .clone();
        if self.describe_routing {
            // D2 managed path: the route table comes from each peer's runtime `__describe`,
            // not the slots. The peer address SET per provider is the `PEER_SLOT` the
            // composition root's `remote::Stub`s contributed (present by now — start runs
            // after every module's init). Run one refresh SYNCHRONOUSLY so routes are ready
            // before serving AND so a collision across describe-contributed entries fails
            // startup loudly (build error propagates); a peer merely DOWN at boot is tolerated
            // (its routes are simply absent, picked up by the loop when it comes up). Then
            // spawn the periodic re-fetch.
            let peers: Vec<opsapi::PeerAddr> = ctx.slots().contributions(opsapi::PEER_SLOT);
            let mut router = DescribeRouter::new(front_door, peers, production_describe_fetcher());
            router.refresh_once().await?;
            // Own the loop (lifecycle constraint 8): keep both the stop sender and the join
            // handle so `stop` tears the task down instead of leaving it running past module
            // teardown with the `Arc<FrontDoor>` + per-provider `Pool`s alive.
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let task = router.spawn(stop_rx);
            *self.stop_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
            *self.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        } else {
            // Monolith/standalone: eager validation of the slot-built table (a collision is a
            // loud startup failure, not a first-request surprise). UNCHANGED.
            front_door.build_table()?;
        }
        Ok(())
    }

    /// Tears down every background resource this module owns, in this order:
    ///
    /// 0. the live `/push` WebSocket connections (`PushHub::shutdown`) — each upgraded
    ///    socket runs in a task the module spawned and OUTLIVES the HTTP drain, so
    ///    nothing else would ever end them. They are flushed a typed close within
    ///    `PUSH_STOP_GRACE` and then aborted; that grace is part of the arithmetic on
    ///    [`DESCRIBE_STOP_GRACE`], which now sums three awaited steps, not two;
    /// 1. the D2 describe-refresh task (grace-then-abort, mirroring `remote::Stub::stop`):
    ///    signal, join within [`DESCRIBE_STOP_GRACE`], else abort and await the abort so the
    ///    task is not leaked;
    /// 2. the currently-installed route table's per-provider dispatch `remote::Pool`s
    ///    ([`FrontDoor::stop_pools`]) — each pool runs a probe task per instance plus a live
    ///    QUIC connection, and NOTHING releases them during teardown: the `FrontDoor` (and
    ///    through it the table and its pools) is retained by [`Gateway::front_door`], the
    ///    mounted axum router, and the player-edge handler, all of which outlive `stop`. The
    ///    Arcs do of course die when the process later returns from `main` — so what this
    ///    step buys is not leak-avoidance but a graceful per-instance CONNECTION_CLOSE to
    ///    each peer while there is still a runtime to send it on (see
    ///    [`RouteTable::stop_pools`], and [`POOL_STOP_BUDGET`] for why it is bounded).
    ///
    /// The order matters: the refresh task must be down FIRST, or a pass in flight could
    /// install a table whose adopted pools were just stopped. Both steps are bounded and
    /// their sum is checked against `MODULE_STOP_GRACE_MS` — see [`DESCRIBE_STOP_GRACE`].
    ///
    /// Safe on every path where a resource was never created (monolith/standalone routing has
    /// no task; a front door that never served has no pools; a start-unwind before `start`
    /// ran has neither): the `Option::take`/`drain` guards leave nothing behind, so a second
    /// `stop` is a no-op.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(front_door) = self.front_door.get() {
            front_door.push_hub().shutdown().await;
        }
        if let Some(tx) = self.stop_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(true);
        }
        // Take the handle out into a local so the std guard is dropped BEFORE the await
        // below (a `MutexGuard` is not `Send` and must never cross `.await`).
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut task) = task {
            match tokio::time::timeout(DESCRIBE_STOP_GRACE, &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    // A stop fired mid-`refresh_once` cannot be observed until the pass ends
                    // (see [`DESCRIBE_STOP_GRACE`]); force the task down rather than hold the
                    // app's whole module-stop budget.
                    task.abort();
                    let _ = task.await; // await the abort so we don't leak the task
                }
            }
        }
        // Then the dispatch pools — only after the refresh task is provably down.
        if let Some(front_door) = self.front_door.get() {
            front_door.stop_pools().await;
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
    /// The whole-credential-admission deadline both planes wrap [`FrontDoor::admit`] in
    /// (default [`DEFAULT_ADMISSION_BUDGET`], overridden from
    /// `CREDENTIAL_ADMISSION_TIMEOUT_MS` via [`Gateway::with_admission_budget`]).
    admission_budget: Duration,
    /// The route table, in one of two modes (see [`TableCell`]): built ONCE lazily from the
    /// process slots (monolith/standalone), or an externally-refreshed swappable table
    /// (managed describe gateway, D2).
    table: TableCell,
    /// The `/push` WebSocket connections this process owns, with the bounds they are
    /// admitted under. Always present — the route behaves identically in both
    /// topologies, so nothing here is conditional on how the process was composed.
    push: Arc<PushHub>,
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
            push: Arc::new(PushHub::new(PushLimits::default())),
            table: TableCell::Slots(OnceLock::new()),
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

    /// Overrides the `/push` bounds (builder-style). Called by [`Gateway::init`] when the
    /// composition root parsed the `PUSH_*` knobs; the unit tests use it to pin small
    /// caps and deadlines.
    pub fn with_push_limits(mut self, limits: PushLimits) -> FrontDoor {
        self.push = Arc::new(PushHub::new(limits));
        self
    }

    /// This process's push connection registry.
    pub(crate) fn push_hub(&self) -> Arc<PushHub> {
        self.push.clone()
    }

    /// Switches the table to the D2 dynamic mode (builder-style): a swappable
    /// [`TableCell::Dynamic`] the `start`-driven describe refresh rebuilds, seeded with an
    /// EMPTY table so requests before the first fetch are un-routable (404) rather than
    /// reading the (op-less) slots. The lazy-from-slots build is never used in this mode.
    fn into_dynamic_routing(mut self) -> FrontDoor {
        let empty = Arc::new(
            RouteTable::build_from_parts(Vec::new(), Vec::new(), Vec::new(), Vec::new())
                .expect("empty route table cannot collide"),
        );
        self.table = TableCell::Dynamic(RwLock::new(empty));
        self
    }

    /// Swaps in a freshly-built route table (D2 describe refresh). A no-op on a
    /// [`TableCell::Slots`] front door — the monolith/standalone table is immutable once
    /// built, so a stray call cannot corrupt it.
    fn install_table(&self, table: Arc<RouteTable>) {
        if let TableCell::Dynamic(cell) = &self.table {
            *cell.write().unwrap() = table;
        }
    }

    /// The route table, built from the slots on first access. By first-request time
    /// every module's `init` (where `OpSet`s are contributed) has completed AND
    /// [`Gateway::start`] has already validated the same slots via [`build_table`],
    /// so the lazy build here cannot surface a NEW collision — hence the `expect`.
    ///
    /// [`build_table`]: FrontDoor::build_table
    fn table(&self) -> Arc<RouteTable> {
        match &self.table {
            // Monolith/standalone: build once from the slots (validated in `Gateway::start`).
            TableCell::Slots(cell) => cell
                .get_or_init(|| {
                    self.build_table()
                        .expect("route table already validated in Gateway::start")
                })
                .clone(),
            // Managed describe gateway: whatever the last successful refresh installed (seeded
            // empty until the first fetch). Never held across an await — cloned out here.
            TableCell::Dynamic(cell) => cell.read().unwrap().clone(),
        }
    }

    /// Gracefully stops the currently-materialized route table's dispatch pools
    /// ([`RouteTable::stop_pools`]). Reads the cell WITHOUT materializing it: a `Slots` front
    /// door that never served a request has no table (and thus no pools), and `stop` must not
    /// build one — `table()`'s lazy build would run a collision `expect` during teardown.
    async fn stop_pools(&self) {
        // The RwLock guard is a temporary of this `let` statement, so it is dropped before
        // the await below (the never-held-across-await rule). Poison-tolerant like the rest
        // of the teardown path: a panic elsewhere must not turn this into a second panic that
        // unwinds `App::stop` and skips every remaining module's teardown.
        let table = match &self.table {
            TableCell::Slots(cell) => cell.get().cloned(),
            TableCell::Dynamic(cell) => {
                Some(cell.read().unwrap_or_else(|e| e.into_inner()).clone())
            }
        };
        if let Some(table) = table {
            table.stop_pools().await;
        }
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
        // `/push` is a REAL route, added ahead of the fallback: the fallback only sees
        // otherwise-unmatched requests, and an operation can never claim this path (the
        // route table is matched inside the fallback, below this one).
        let push_front = self.clone();
        Router::new()
            .route(
                "/push",
                axum::routing::get(
                    move |peer: Option<ConnectInfo<SocketAddr>>,
                          headers: axum::http::HeaderMap,
                          ws: axum::extract::ws::WebSocketUpgrade| {
                        let front = push_front.clone();
                        async move { push_ws::upgrade(front, peer, headers, ws).await }
                    },
                ),
            )
            .fallback(
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

        // One table snapshot for the whole player call (match + dispatch); `table()` returns
        // an owned `Arc`, so no `.clone()` is needed.
        let table = self.table();

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
            .admit(
                api_key.as_deref(),
                token.as_deref(),
                route.op.auth,
                KeyCheck::Policy(&route.op.method),
            )
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
        key: KeyCheck<'_>,
    ) -> Result<Identity, AdmissionDenial> {
        match tokio::time::timeout(
            self.admission_budget,
            self.admit_inner(api_key, bearer, auth, key),
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
        key: KeyCheck<'_>,
    ) -> Result<Identity, AdmissionDenial> {
        // (a) API-key check — post-match, pre-auth (Decision 5's exact three-way), in the
        // mode the caller's surface can prove: a policy match for an op, presence and
        // validity for a fixed route.
        check_api_key(&*self.key_verifier, api_key, key)
            .await
            .map_err(AdmissionDenial::Key)?;

        // (b) Auth-once: the single trust boundary. For a player-auth op the bearer is
        // required and verified; only the VERIFIED player_id becomes the identity.
        verify_bearer(&*self.verifier, bearer, auth).await
    }

    /// Re-verifies a live push connection's bind-time bearer, bounded by the same
    /// admission budget. The api key is NOT re-checked: it authorizes the client class at
    /// admission, while a revoked SESSION is what must not survive on an open socket.
    ///
    /// The caller decides what each denial means for the connection — in particular
    /// [`AdmissionDenial::SessionUnavailable`]/[`AdmissionDenial::Timeout`] must not end
    /// it, or an accounts blip would disconnect every player at once.
    ///
    /// It applies the budget itself rather than going through [`FrontDoor::admit`]: there
    /// is no key check to share the deadline with, and re-running one would consult the
    /// apikeys store once per tick per connection for a decision that was made at
    /// admission.
    pub(crate) async fn reverify_push(&self, bearer: &str) -> Result<Identity, AdmissionDenial> {
        match tokio::time::timeout(
            self.admission_budget,
            verify_bearer(&*self.verifier, Some(bearer), AuthReq::Player),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(AdmissionDenial::Timeout),
        }
    }
}

/// The ONE bearer admission: for an `AuthReq::Player` op (or a `/push` dial) the token is
/// required and verified, and only the VERIFIED player_id becomes an [`Identity`];
/// `AuthReq::None` runs with [`Identity::none`].
///
/// UNBUDGETED on purpose. [`FrontDoor::admit`] wraps its key check and this call in ONE
/// `timeout` so a hung key lookup and a hung session verify share a single deadline; a
/// timeout of its own here would nest a second deadline inside that one and break the
/// invariant. Every other caller applies the budget at its own call site.
pub(crate) async fn verify_bearer(
    verifier: &dyn SessionVerifier,
    bearer: Option<&str>,
    auth: AuthReq,
) -> Result<Identity, AdmissionDenial> {
    match auth {
        AuthReq::Player => {
            let Some(token) = bearer else {
                return Err(AdmissionDenial::MissingBearer);
            };
            match verifier.verify(token).await {
                Ok(Some(pid)) => Ok(Identity::player(pid)),
                Ok(None) => Err(AdmissionDenial::InvalidSession),
                Err(VerifyUnavailable) => Err(AdmissionDenial::SessionUnavailable),
            }
        }
        AuthReq::None => Ok(Identity::none()),
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
    /// Peer edge address SET per provider (provider → ALL its live instances as UNPARSED
    /// `host:port` strings), collected from `opsapi::PEER_SLOT` — one entry per
    /// `remote::Stub` the composition root wired. `remote_caller` looks a provider up here
    /// and builds a round-robin `remote::Pool` over the set (C2), instead of reading a
    /// per-provider edge-address env var: topology is injected by the composition root,
    /// never read inside this module. A single-instance provider is a one-element set — a
    /// pool-of-1.
    peers: HashMap<String, Vec<String>>,
    /// Lazily-built per-provider callers (a self-healing `remote::Pool` over the instance
    /// set), shared across requests to that provider and PERMANENT for the process life —
    /// an entry is NEVER evicted on a call error (the pool recovers a dead instance
    /// internally; see [`RouteTable::dispatch`]). Torn down only at module `stop`
    /// ([`RouteTable::stop_pools`], via the concrete handles in [`RouteTable::pools`]).
    /// A `std::sync::Mutex` locked only for synchronous get/insert/remove — never
    /// held across an await (the `keys.rs` cache rule), so a slow dial to one
    /// provider can never block cache hits for the others.
    remotes: Mutex<HashMap<String, Arc<dyn Caller>>>,
    /// The TEARDOWN handle for the subset of [`RouteTable::remotes`] this module actually
    /// minted (provider → the concrete `remote::Pool` behind the type-erased `Arc<dyn
    /// Caller>`). `remotes` is deliberately type-erased (unit tests inject fake callers), so
    /// the erased side cannot be stopped; a `Pool` is only cleaned up by its `Drop` safety
    /// net (probe ABORT, no connection close) and only once its LAST `Arc` goes — which, for
    /// the installed table, is long after module teardown (the `FrontDoor` is retained by
    /// `Gateway::front_door`, the mounted axum router, and the player handler). Keeping the
    /// concrete `Arc<remote::Pool>` here gives [`RouteTable::stop_pools`] the graceful
    /// `Pool::stop` (probe grace-then-abort + connection close) the owning module calls.
    /// Carried across a describe refresh together with its `remotes` entry
    /// ([`RouteTable::adopt_remote`]) so a pool that survives rebuilds stays stoppable.
    pools: Mutex<HashMap<String, Arc<remote::Pool>>>,
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
        RouteTable::build_from_parts(
            slots.contributions(opsapi::SLOT),
            slots.contributions(opsapi::BINDING_SLOT),
            slots.contributions(opsapi::LOCAL_SLOT),
            slots.contributions(opsapi::PEER_SLOT),
        )
    }

    /// Assembles the table from the explicit contribution vectors, FAILING on any collision
    /// (the shared authority both the slot-built path — [`RouteTable::build`] — and the D2
    /// describe-built path ([`build_describe_table`]) run through, so the collision-`bail!`
    /// fires identically no matter where the `Operation`s came from). The describe path
    /// passes `locals` empty (every stub-fronted op is Remote) and rebuilds this on EVERY
    /// re-fetch, so a duplicated method across describe-contributed peers is caught on each
    /// pass, not only at boot.
    fn build_from_parts(
        operations: Vec<Operation>,
        bindings: Vec<OpBinding>,
        locals: Vec<opsapi::LocalOp>,
        peer_addrs: Vec<opsapi::PeerAddr>,
    ) -> anyhow::Result<RouteTable> {
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

        let mut peers: HashMap<String, Vec<String>> = HashMap::new();
        for p in peer_addrs {
            if let Some(existing) = peers.get(&p.provider) {
                anyhow::bail!(
                    "gateway: duplicate peer for provider {:?} — {:?} and {:?} both \
                     contributed (two remote::Stubs for one provider)",
                    p.provider,
                    existing,
                    p.addrs
                );
            }
            // Carry the WHOLE instance set (C2): `remote_caller` builds a round-robin
            // `remote::Pool` over it so HTTP-dispatched Remote ops spread across every live
            // instance. An empty set surfaces as a per-request 503 (the pool is un-routable),
            // mirroring the lazy-resolve contract.
            peers.insert(p.provider, p.addrs);
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
            pools: Mutex::new(HashMap::new()),
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
    /// process holds the invoker, else a [`RemoteBackend`] over the provider's cached
    /// `remote::Pool`. Serves BOTH planes — the HTTP handler and the player handler
    /// funnel through here.
    ///
    /// **No evict-on-error (C2).** The cached per-provider caller is a self-healing
    /// `remote::Pool` (per-instance `Reconnecting` reconnect + probe-fed skip-dead), so
    /// it is PERMANENT for the process life — built once by [`remote_caller`], torn down
    /// only at module `stop` ([`RouteTable::stop_pools`]; `remote::Pool`'s own `Drop` is
    /// merely the probe-abort safety net for a pool that is dropped instead). A per-instance
    /// or transient call error must NOT tear the whole pool down: doing so would defeat
    /// the pool's skip-dead (the aborted probe never completes to mark the dead instance
    /// non-selectable) and, worse, a fresh pool resets the round-robin cursor to 0 —
    /// re-picking a dead first instance every request, 100% failure to a provider one of
    /// whose N instances is down. The pool recovers a dead instance internally within a
    /// probe cycle, exactly like the capability-stub pool. This mirrors the pre-pool
    /// evict-and-redial logic being subsumed by the pool's OWN reconnect; there is no
    /// error class a rebuild fixes that the pool doesn't already handle (a definitive
    /// `NotFound`/unknown-method is a ROUTING fact, not peer health — rebuilding the pool
    /// changes nothing).
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
                RemoteBackend::new(caller).invoke(op, identity, req).await
            }
        }
    }

    /// Gets (or lazily builds + caches) the provider's round-robin `remote::Pool`. The
    /// peer's QUIC instance SET comes from the `opsapi::PEER_SLOT` contribution the
    /// composition root's `remote::Stub` wired (collected into [`RouteTable::peers`] at
    /// build); the pool is reused across requests and PERMANENT — never evicted on a call
    /// error (see [`RouteTable::dispatch`]). Each instance's address is parsed lazily by
    /// the pool at dial time, so a bad address is a per-request `Unavailable` (503), never
    /// a startup panic — the `remote::Stub` contributes the raw strings for exactly this
    /// reason. In M1's per-svc topology
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
        let addrs = self
            .peers
            .get(provider)
            .ok_or_else(|| {
                Error::unavailable(format!(
                    "gateway: no peer contributed for provider {provider:?} \
                     (wire a remote::Stub in this process's main)"
                ))
            })?
            .clone();
        // Build a client-side round-robin `remote::Pool` over this provider's resolved
        // instance SET (C2). Construction is synchronous (no dial): the pool dials +
        // probes each instance lazily on its first `call`, so a slow/dead instance is a
        // per-request 503 inside the pool, never a construction-time hang here. A
        // single-instance provider degenerates to a pool-of-1 — behaviourally the same
        // self-healing dial the bare `edge::Client` gave, plus retry-mode-honouring replay.
        //
        // [SHARED-VS-SEPARATE — flagged] This pool is DISTINCT from the capability stub's
        // pool (in `remote::Stub`): they cannot share a live object across the
        // module/`core` boundary (the stub contributes DATA to PEER_SLOT, not its Pool).
        // The stub's pool re-resolves the LIST live off the agent; this one round-robins
        // over the BOOT snapshot PEER_SLOT carries (contributed in `init` before any I/O,
        // so it cannot re-resolve). Both spread across every instance the boot resolve saw
        // — a scale event reaches HTTP dispatch on the next process boot, the capability
        // path live. Deliberate for C2; a live route-table re-resolve is out of scope.
        let list: remote::PeerListResolver = {
            let addrs = addrs.clone();
            Arc::new(move || {
                let addrs = addrs.clone();
                Box::pin(async move { Ok(addrs.clone()) })
            })
        };
        Ok(self.insert_caller(provider, Arc::new(remote::Pool::new(list))))
    }

    /// Publishes a freshly-minted pool as BOTH this table's dispatch caller and its teardown
    /// handle, returning the type-erased caller.
    ///
    /// ONE function on purpose: `remotes` (type-erased, dispatch) and `pools` (concrete,
    /// teardown) are two maps under two independent locks, and the invariant "a pool visible
    /// in `remotes` is stoppable via `pools`" is exactly what [`RouteTable::stop_pools`]
    /// rests on. Two inserts at the call site would make that invariant depend on their
    /// ORDER — publish `remotes` first and a concurrent [`RouteTable::adopt_remote`] (which
    /// reads `remotes` first) can carry a live pool into the next table WITHOUT its teardown
    /// handle, silently restoring the leak with no compile error and no test failure. Keeping
    /// both inserts here means the order cannot be split by a later edit.
    fn insert_caller(&self, provider: &str, pool: Arc<remote::Pool>) -> Arc<dyn Caller> {
        let caller: Arc<dyn Caller> = pool.clone();
        self.publish_caller(provider, Some(pool), caller.clone());
        caller
    }

    /// The SINGLE publisher for both maps (see [`RouteTable::insert_caller`] for why the two
    /// inserts must never be split across call sites). `pool` is `None` for a caller with no
    /// teardown handle — the unit tests' fake `Caller`s, carried for dispatch only.
    fn publish_caller(
        &self,
        provider: &str,
        pool: Option<Arc<remote::Pool>>,
        caller: Arc<dyn Caller>,
    ) {
        // Teardown handle FIRST, dispatch entry second: a reader that sees the caller in
        // `remotes` always sees its pool in `pools` too.
        if let Some(pool) = pool {
            self.pools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(provider.to_string(), pool);
        }
        self.remotes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider.to_string(), caller);
    }

    /// Adopts `provider`'s live caller from a previously-installed table — the C2 no-evict
    /// carry-over at the describe-refresh boundary — together with its teardown handle when
    /// the caller is a table-minted [`remote::Pool`]. Without the second half a pool that
    /// survives rebuilds (the steady-state case) would be reachable for dispatch but
    /// invisible to [`RouteTable::stop_pools`]. Returns `false` when the previous table never
    /// built one (that provider simply dials lazily on its next request).
    fn adopt_remote(&self, provider: &str, from: &RouteTable) -> bool {
        let Some(caller) = from.cached_remote(provider) else {
            return false;
        };
        // Bind out of each lock before taking the next — no guard is ever held across
        // another table's lock (nor, here, across an await: this whole fn is synchronous).
        let carried = from
            .pools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
            .cloned();
        // Republish through the single publisher (pool + caller, in that order) so the new
        // table's two maps cannot disagree. `carried` is `None` for a non-pool `Caller` (the
        // unit tests' fakes): carried for dispatch, nothing to tear down. The ORIGINAL
        // `caller` object is moved across, not a re-erased clone, so identity is preserved.
        self.publish_caller(provider, carried, caller);
        true
    }

    /// Gracefully tears down every dispatch [`remote::Pool`] this table owns: per-instance
    /// probe grace-then-abort + connection close (`Pool::stop`, the same call
    /// `remote::Stub::stop` makes). Called by [`Gateway::stop`]. `drain`s, so a second call is
    /// a no-op. Only the CURRENTLY-installed table's pools are covered; a pool superseded by
    /// an earlier refresh (its provider's describe changed, so it was neither adopted nor
    /// reachable) already had its last `Arc` dropped at that swap, where `Pool`'s `Drop`
    /// aborted its probes.
    ///
    /// What this buys, precisely: NOT preventing an eternal leak — after `ordered_teardown`
    /// the process returns from `main` and every `Arc` dies anyway — but a graceful
    /// per-instance CONNECTION_CLOSE to each peer before exit, instead of peers discovering
    /// the front door's death by timeout. Cancellation destroys exactly that value, which is
    /// why the whole fan-out is bounded by [`POOL_STOP_BUDGET`] and runs CONCURRENTLY: serial
    /// `Pool::stop` is unbounded in practice (each instance is `stop_probe`, up to `remote`'s
    /// 2s probe grace, PLUS `Reconnecting::close` awaiting a tokio mutex that a dial holds for
    /// up to `edge`'s 5s `DIAL_DEADLINE`), so ONE half-open peer among 11 providers would burn
    /// the module's whole stop budget: `App::stop` would cancel this future, providers after
    /// the stuck one would never be stopped at all, and the stuck one would be worse off than
    /// unstopped — `Pool::stop` has already `mem::take`n its instances, so they sit in the
    /// cancelled future's frame, invisible even to `Drop`'s probe-abort net.
    async fn stop_pools(&self) {
        // Drain into a local so the std guard is dropped BEFORE the awaits below.
        let pools: Vec<Arc<remote::Pool>> = {
            let mut guard = self.pools.lock().unwrap_or_else(|e| e.into_inner());
            guard.drain().map(|(_, p)| p).collect()
        };
        if pools.is_empty() {
            return;
        }
        let total = pools.len();
        // `JoinSet` rather than a `futures` combinator: one slow pool no longer delays the
        // others, and the set ABORTS every still-running stop when it drops at the end of this
        // function — including the timeout path below — so nothing is left detached.
        let mut set = tokio::task::JoinSet::new();
        for pool in pools {
            set.spawn(async move { pool.stop().await });
        }
        let mut done = 0usize;
        let drain = async {
            while set.join_next().await.is_some() {
                done += 1;
            }
        };
        if tokio::time::timeout(POOL_STOP_BUDGET, drain).await.is_err() {
            tracing::warn!(
                stopped = done,
                total,
                budget_ms = POOL_STOP_BUDGET.as_millis(),
                "gateway: dispatch pool teardown exceeded its budget; aborting the rest \
                 (peers will see the connection drop instead of a graceful close)"
            );
        }
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

// ---------------------------------------------------------------------------
// TableCell — the two route-table storage modes
// ---------------------------------------------------------------------------

/// How a [`FrontDoor`] stores its route table. Two disjoint modes so the monolith path is
/// untouched while the managed describe gateway (D2) gets a refreshable table:
///
/// * [`TableCell::Slots`] — built ONCE lazily from the process contribution slots
///   (`opsapi::SLOT`/…) on first access, then immutable. The monolith and every standalone
///   domain svc use this; `Gateway::start` validates it eagerly. UNCHANGED from before D2.
/// * [`TableCell::Dynamic`] — a swappable `Arc<RouteTable>` an external refresh loop (the
///   `start`-driven describe re-fetch) rebuilds on each successful pass. Read out (cloned)
///   per request; the `RwLock` is never held across an await.
enum TableCell {
    Slots(OnceLock<Arc<RouteTable>>),
    Dynamic(RwLock<Arc<RouteTable>>),
}

// ---------------------------------------------------------------------------
// DescribeRouter — the D2 periodic describe re-fetch driving the dynamic table
// ---------------------------------------------------------------------------

/// Fetches one peer's `__describe` manifest given its provider name and address SET. Injected
/// so the refresh loop is testable with an in-process fake (production dials the real edge via
/// [`production_describe_fetcher`]).
type DescribeFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<opsapi::DescribeManifest, opsapi::Error>> + Send>,
>;
type DescribeFetcher = Arc<dyn Fn(String, Vec<String>) -> DescribeFuture + Send + Sync>;

/// The production [`DescribeFetcher`]: builds (and reuses) one self-healing `remote::Pool`
/// per provider over its address set and calls `remote::describe` on it. A persistent per-
/// provider caller is what makes the "peer DOWN at boot, UP later" property work — the Pool
/// reconnects internally, so a later refresh's describe succeeds without rebuilding anything.
///
/// SCOPE — "DOWN at boot, UP later" is about REACHABILITY of an address already known at
/// boot, never about discovering a new one. The address set reaching this fetcher is the
/// `opsapi::PEER_SLOT` boot snapshot (see [`DESCRIBE_REFRESH_INTERVAL`]), and the pool is
/// keyed by provider and REUSED across passes — so a changed address set for an existing
/// provider is not picked up either. Deliberate for D2; a live re-resolve is out of scope.
fn production_describe_fetcher() -> DescribeFetcher {
    let callers: Arc<Mutex<HashMap<String, Arc<dyn Caller>>>> = Arc::new(Mutex::new(HashMap::new()));
    Arc::new(move |provider: String, addrs: Vec<String>| {
        let callers = callers.clone();
        Box::pin(async move {
            // Reuse this provider's caller across refreshes; build it once on first sight.
            let caller = {
                // Poison-TOLERANT (the convention elsewhere in this file), and load-bearing now
                // that a pass fetches peers in PARALLEL TASKS: this closure is what runs in those
                // tasks, so one panic under this guard would poison the mutex permanently, and a
                // bare `unwrap()` would then panic EVERY later fetch task for EVERY provider —
                // turning one bad peer into "all peers absent forever" while the process keeps
                // serving and `/readyz` stays green. Keep-last would hide it indefinitely.
                let mut map = callers.lock().unwrap_or_else(|e| e.into_inner());
                map.entry(provider.clone())
                    .or_insert_with(|| {
                        let list: remote::PeerListResolver = {
                            let addrs = addrs.clone();
                            Arc::new(move || {
                                let addrs = addrs.clone();
                                Box::pin(async move { Ok(addrs.clone()) })
                            })
                        };
                        Arc::new(remote::Pool::new(list)) as Arc<dyn Caller>
                    })
                    .clone()
            };
            remote::describe(caller.as_ref()).await
        })
    })
}

/// Builds a route table PURELY from describe data (no compile-time `<name>rpc` import): each
/// `OpManifest` becomes an `Operation` + `OpBinding` via `opsapi::databind`, and each fetched
/// provider contributes its address SET as a `PeerAddr`. `locals` is empty — every route here
/// is Remote (dispatched to the owning peer). Returns the SAME collision `Err` as the
/// slot-built path (via `build_from_parts`), so a duplicated method is loud.
///
/// **Fail-closed on the provider prefix.** A manifest may only carry the fetched peer's OWN
/// ops (`"<provider>.<op>"`); an entry under a foreign (or malformed) prefix `bail!`s the
/// whole build rather than becoming a route dispatched to a peer that does not own it. Like
/// the two collision guards in [`RouteTable::build_from_parts`], this runs on EVERY pass —
/// including the first one awaited inside `Gateway::start` — so a misbehaving peer (or a
/// codegen bug) takes the gateway down LOUDLY at boot instead of serving a half-table. This
/// is the BUILD side: a per-peer describe FETCH failure is still keep-last (see
/// [`DescribeRouter::refresh_once`]) — only a manifest we DID receive can trip this.
///
/// **What a rejection costs on a LATER pass: a FROZEN table, not a skipped swap.**
/// `refresh_once` writes each successful fetch into `last_known` BEFORE building, so a
/// rejected build leaves the bad manifest recorded while `last_built` never advances — the
/// next pass therefore sees a change, rebuilds, and is rejected again, indefinitely. ONE peer
/// emitting ONE foreign-prefix (or colliding) op freezes the WHOLE table for EVERY provider:
/// a new `#[http]` op on a healthy peer never lights up and a peer that was down at boot
/// never appears, while `/readyz` stays green (this module contributes no readiness check).
/// It is at least loud — one `tracing::error!` per [`DESCRIBE_REFRESH_INTERVAL`] — and it
/// self-clears the moment the peer stops advertising the bad entry. The guard is NOT
/// downgraded for this: serving a route to a peer that does not own it is worse than a stale
/// table. The mitigation belongs to the readiness surface already deferred in
/// [`DescribeRouter::refresh_once`] (a repeated-failure liveness signal), not here.
///
/// **KNOWN GAP — this validates the METHOD-ID dimension only.** The rest of an `OpManifest`
/// (`verb`/`path`/`auth`/`success`/`args`) is still trusted verbatim, so a buggy peer can
/// advertise a correctly-prefixed `inventory.evil` bound `POST /accounts/login` with
/// `AuthReq::None`; if `accounts` happens to be absent from `last_known` at that instant (down
/// at boot — tolerated by design) no overlapping route trips `build_from_parts` and the front
/// door serves an unauthenticated route on another provider's path. Closing that needs a
/// path-ownership rule (which prefix may claim which URL space), which does not exist today.
fn build_describe_table(
    fetched: &HashMap<String, (Vec<String>, opsapi::DescribeManifest)>,
) -> anyhow::Result<RouteTable> {
    let mut operations: Vec<Operation> = Vec::new();
    let mut bindings: Vec<OpBinding> = Vec::new();
    let mut peer_addrs: Vec<opsapi::PeerAddr> = Vec::new();
    for (provider, (addrs, manifest)) in fetched {
        for m in &manifest.ops {
            // The advertised owner (`provider_of` — the SAME split-on-first-`.` the dispatch
            // path uses to pick the peer, so this validates exactly what routing will read)
            // must be the peer we fetched from. `provider_of` returns the WHOLE method when
            // there is no `.`, so an equal comparison alone would let a dotless method
            // (`"inventory"` from peer `inventory`) or an empty op name (`"inventory."`)
            // through with no routable suffix; requiring the method to be longer than
            // `"<provider>."` rejects both. An empty provider key is rejected outright — it
            // would otherwise compare equal to the empty prefix of a method like `".op"`.
            let advertised = provider_of(&m.method);
            if provider.is_empty()
                || advertised != provider.as_str()
                || m.method.len() <= provider.len() + 1
            {
                anyhow::bail!(
                    "gateway: peer {:?} advertised method {:?} in its describe manifest — \
                     a peer may only advertise its own ops (expected prefix {:?})",
                    provider,
                    m.method,
                    format!("{provider}.")
                );
            }
            operations.push(opsapi::databind::operation(m));
            bindings.push(opsapi::databind::binding(m));
        }
        peer_addrs.push(opsapi::PeerAddr {
            provider: provider.clone(),
            addrs: addrs.clone(),
        });
    }
    RouteTable::build_from_parts(operations, bindings, Vec::new(), peer_addrs)
}

/// Drives the D2 describe-driven route table: on each pass it re-fetches every peer's
/// `__describe` (error-keeps-last per peer — a transient describe failure keeps that peer's
/// last-known routes, exactly like `remote::Pool::refresh` keeps its last instance set on a
/// resolver error), rebuilds the table, and swaps it into the [`FrontDoor`]. Built once in
/// `Gateway::start`, its first pass run synchronously (so routes are ready + a collision fails
/// startup), then [`DescribeRouter::spawn`] runs the periodic loop.
struct DescribeRouter {
    front: Arc<FrontDoor>,
    peers: Vec<opsapi::PeerAddr>,
    fetch: DescribeFetcher,
    /// Last-known-good manifest per provider (provider → (addrs, manifest)). Only successful
    /// fetches update it; a failed fetch leaves the prior entry, so the rebuilt table keeps
    /// that peer's routes until the next success (error-keeps-last).
    last_known: HashMap<String, (Vec<String>, opsapi::DescribeManifest)>,
    /// The `last_known` snapshot the CURRENTLY-installed table was built from (`None` until the
    /// first build). A pass whose `last_known` is unchanged skips the rebuild+swap entirely —
    /// so in steady state (describe stable) the installed table (and its permanent per-provider
    /// dispatch `Pool`s + round-robin cursors, the C2 no-evict invariant) survives untouched;
    /// only an actual describe change (a peer appearing, an op added/removed) rebuilds.
    last_built: Option<HashMap<String, (Vec<String>, opsapi::DescribeManifest)>>,
}

impl DescribeRouter {
    fn new(front: Arc<FrontDoor>, peers: Vec<opsapi::PeerAddr>, fetch: DescribeFetcher) -> Self {
        DescribeRouter {
            front,
            peers,
            fetch,
            last_known: HashMap::new(),
            last_built: None,
        }
    }

    /// One re-fetch pass: fetch each peer's describe (keep-last on failure), rebuild the table
    /// from all last-known manifests, and swap it in. A describe FETCH failure is tolerated
    /// (logged, that peer keeps its prior routes / stays absent if never seen). A BUILD
    /// rejection (a method collision, or a peer advertising a foreign provider prefix) is
    /// returned as `Err` so the synchronous first pass can fail startup loudly; the periodic
    /// loop logs it and keeps the last good table (no swap) — and, since the offending
    /// manifest is already in `last_known` while `last_built` did not advance, keeps failing
    /// every pass until that peer stops advertising it (the frozen-table cost spelled out in
    /// [`build_describe_table`]). Unchanged providers keep their warm dispatch pool + cursor
    /// across the swap (see below) — a change to ONE peer never evicts the others (C2 no-evict
    /// at the refresh boundary).
    ///
    /// The pass is BOUNDED: each peer's fetch is capped at [`DESCRIBE_PEER_TIMEOUT`] and at most
    /// [`DESCRIBE_FETCH_CONCURRENCY`] run at once, so the whole pass — including the one awaited
    /// by `Gateway::start` — ends within `ceil(N_peers / CONCURRENCY) * DESCRIBE_PEER_TIMEOUT`
    /// (ONE wave, 6s, for the 11-provider fleet), never the 30s `EDGE_STREAM_GRACE` a stalled
    /// peer used to impose. The per-peer bound clears a cold QUIC dial rather than cutting into
    /// it — see [`DESCRIBE_PEER_TIMEOUT`] for that floor, and for why there is no separate
    /// whole-pass budget.
    async fn refresh_once(&mut self) -> anyhow::Result<()> {
        // Results land here indexed BY PEER POSITION, not by completion order, and are applied to
        // `last_known` afterwards in `self.peers` order. Concurrency must not become an ordering
        // input: if two `PeerAddr` contributions named the same provider, "who wins" would
        // otherwise depend on which fetch happened to finish first (and the pass would be
        // nondeterministic across runs). Applying in peer order keeps the serial semantics —
        // last contribution for a provider wins — exactly as before.
        let mut fetched: Vec<Option<opsapi::DescribeManifest>> =
            (0..self.peers.len()).map(|_| None).collect();
        // `JoinSet` (as in [`RouteTable::stop_pools`]) rather than a `futures` combinator: this
        // crate has no `futures` dependency, and the set aborts every still-running fetch when it
        // drops — so a `Gateway::stop` that force-aborts the loop mid-pass leaves nothing behind.
        let mut set: tokio::task::JoinSet<(usize, Option<opsapi::DescribeManifest>)> =
            tokio::task::JoinSet::new();
        // Task id → peer index, so a panicking fetcher can still be reported by provider name
        // (the panic payload takes the return value, index included, with it).
        let mut ids: HashMap<tokio::task::Id, usize> = HashMap::new();
        let mut next = 0usize;
        while next < self.peers.len() || !set.is_empty() {
            while next < self.peers.len() && set.len() < DESCRIBE_FETCH_CONCURRENCY {
                let idx = next;
                let p = &self.peers[idx];
                let provider = p.provider.clone();
                let addrs = p.addrs.clone();
                let fetch = self.fetch.clone();
                let handle = set.spawn(async move {
                    // The bound lives HERE, in the pass owner: `remote::describe` has no timeout
                    // of its own and the server side only gives up at `EDGE_STREAM_GRACE` (30s).
                    let call = fetch(provider.clone(), addrs);
                    match tokio::time::timeout(DESCRIBE_PEER_TIMEOUT, call).await {
                        Ok(Ok(manifest)) => (idx, Some(manifest)),
                        // ONE keep-last branch for both per-peer failure modes: an errored fetch
                        // and a timed-out fetch are the same thing to the table — this peer is
                        // ABSENT for this pass, so it keeps its prior manifest (or stays unseen).
                        Ok(Err(e)) => {
                            tracing::warn!(
                                provider = %provider,
                                error = %e,
                                "gateway: describe fetch failed; keeping this peer's \
                                 last-known routes"
                            );
                            (idx, None)
                        }
                        Err(_elapsed) => {
                            tracing::warn!(
                                provider = %provider,
                                timeout_ms = DESCRIBE_PEER_TIMEOUT.as_millis(),
                                "gateway: describe fetch timed out; keeping this peer's \
                                 last-known routes"
                            );
                            (idx, None)
                        }
                    }
                });
                ids.insert(handle.id(), idx);
                next += 1;
            }
            match set.join_next().await {
                Some(Ok((idx, manifest))) => fetched[idx] = manifest,
                Some(Err(e)) => {
                    // A panicking fetcher is a per-peer failure like any other (keep-last), not a
                    // reason to fail the pass — the injected fetcher is the only code that can
                    // panic here, and one bad peer must not take the route table down.
                    //
                    // KNOWN GAP (readiness surface, deliberately NOT decided here): a REPEATED
                    // task-level failure is a different animal from a repeated fetch error — it
                    // means the pass machinery itself is broken, and keep-last hides it behind a
                    // stale-but-plausible table with `/readyz` still green. Whether that deserves
                    // a liveness signal (a readiness check, a counter) is a readiness-surface
                    // decision beyond this bound; only the log records it today.
                    let provider = ids
                        .get(&e.id())
                        .map(|i| self.peers[*i].provider.as_str())
                        .unwrap_or("<unknown>");
                    tracing::error!(
                        provider = %provider,
                        error = %e,
                        "gateway: describe fetch task failed; keeping this peer's last-known routes"
                    );
                }
                None => break,
            }
        }
        for (idx, p) in self.peers.iter().enumerate() {
            if let Some(manifest) = fetched[idx].take() {
                self.last_known
                    .insert(p.provider.clone(), (p.addrs.clone(), manifest));
            }
        }
        // Nothing changed since the installed table was built → keep it (and its permanent
        // pools/cursors). `last_built` is `None` on the first pass, so the initial build (and
        // its collision check) always runs.
        if self.last_built.as_ref() == Some(&self.last_known) {
            return Ok(());
        }
        let table = build_describe_table(&self.last_known)?;
        // C2 no-evict at the refresh boundary: `install_table` swaps the WHOLE table `Arc`, so a
        // naive rebuild would drop EVERY provider's warm `remote::Pool` (and reset its round-
        // robin cursor to 0) whenever ANY one provider's describe changes — the exact eviction
        // the pool caching forbids, triggered by the feature's own primary path (peer B appears
        // → its change must not cool peer A's pool). So harvest the currently-installed table's
        // per-provider caller for every provider whose (addrs, manifest) is UNCHANGED and seed
        // the new table's `remotes` with it — that Arc IS the live `Pool`, so its instances +
        // cursor survive. Only a provider whose describe/addrs actually changed (re)dials, and
        // then lazily on its next request. `flights` stay empty (transient dial coordination,
        // self-GC); a provider never seen (`adopt_remote` `false`) simply builds lazily as before.
        // The adoption carries the pool's TEARDOWN handle across too, so a pool that survives
        // rebuilds stays reachable for `Gateway::stop` (see [`RouteTable::adopt_remote`]).
        if let Some(prev) = &self.last_built {
            let installed = self.front.table();
            for (provider, entry) in &self.last_known {
                if prev.get(provider) == Some(entry) {
                    table.adopt_remote(provider, &installed);
                }
            }
        }
        self.front.install_table(Arc::new(table));
        self.last_built = Some(self.last_known.clone());
        Ok(())
    }

    /// Spawns the periodic re-fetch loop on the [`DESCRIBE_REFRESH_INTERVAL`] cadence. A build
    /// collision on a later pass is logged and skips the swap (keep-last) — it cannot retro-
    /// actively fail an already-serving process, but it never installs a corrupt table either.
    ///
    /// Returns the [`tokio::task::JoinHandle`] so the OWNING module (`Gateway`) can join or
    /// abort it in `stop` — a detached task would keep re-fetching every peer's describe (and
    /// keep its describe-FETCHER pools' connections + probe tasks alive, since those DO drop
    /// with the task) past module teardown. The DISPATCH pools are a separate ownership
    /// problem: they live in the installed `RouteTable`, which the `FrontDoor` — retained by
    /// `Gateway::front_door`, the axum router and the player handler — keeps alive well past
    /// this task, so `Gateway::stop` stops them explicitly via [`FrontDoor::stop_pools`].
    /// `stop_rx` is observed only between passes; see [`DESCRIBE_STOP_GRACE`].
    fn spawn(
        mut self,
        mut stop_rx: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(DESCRIBE_REFRESH_INTERVAL);
            // The first tick fires immediately; skip it — the synchronous `refresh_once` in
            // `Gateway::start` already ran one pass before serving began.
            ticker.tick().await;
            loop {
                tokio::select! {
                    // `biased`: stop ALWAYS wins a tie. `tokio::time::interval` defaults to
                    // `MissedTickBehavior::Burst`, so after a pass longer than
                    // `DESCRIBE_REFRESH_INTERVAL` the ticker is backlogged and `tick()` is
                    // immediately ready on every loop entry; an unbiased `select!` picks
                    // uniformly at random among ready branches, which would start ANOTHER
                    // pass instead of stopping with probability ~0.5 — a nondeterministic
                    // graceful stop that then force-aborts at `DESCRIBE_STOP_GRACE`.
                    biased;
                    // `changed()` also resolves (as `Err`) when the sender is dropped —
                    // i.e. the owning `Gateway` is gone. Either way the loop must end,
                    // so both outcomes break rather than spin on a closed channel.
                    _ = stop_rx.changed() => break,
                    _ = ticker.tick() => {}
                }
                if let Err(e) = self.refresh_once().await {
                    tracing::error!(
                        error = %e,
                        "gateway: describe route rebuild rejected (collision or foreign \
                         provider prefix); the last table stays installed and every later \
                         pass will fail the same way until the offending peer stops \
                         advertising it — see build_describe_table"
                    );
                }
            }
        })
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

    // Capture ONE table snapshot for the whole request — match AND dispatch run against the
    // same `Arc`, so a describe re-fetch swapping the table mid-request can never dispatch an
    // op matched in t1 against t2's pools/peers (no TOCTOU). `table()` returns an owned `Arc`,
    // so no `.clone()` is needed.
    let table = front.table();

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
    let mut resp =
        dispatch_matched_op(&front, &table, op, binding, path_args, parts.headers, body).await;
    stamp_route_pattern(&mut resp, Some(pattern));
    resp
}

/// Steps (2)–(6) for a matched operation: key check → auth-once → decode → dispatch →
/// encode. Split out of [`handle`] so the caller can stamp the route-pattern label on
/// EVERY outcome (including an early key/auth/decode failure) at one place.
async fn dispatch_matched_op(
    front: &FrontDoor,
    table: &RouteTable,
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
            KeyCheck::Policy(&op.method),
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
    // peer over its self-healing round-robin pool — the pool recovers a dead instance
    // internally, so it is never evicted on a call error).
    let wire_resp = match table.dispatch(&op, identity, wire_req).await {
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

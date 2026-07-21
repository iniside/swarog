//! `remote` — the registry SWAP that makes the split topology-blind (port of Go's
//! `modules/remote`). When a process hosts a consumer whose provider lives in ANOTHER
//! process, `main` adds a [`Stub`] for that provider. In phase-1 `register` the stub
//! `provide`s a generated edge CLIENT under the SAME capability key(s) the local impl
//! would, so the consumer's `require::<dyn Trait>(key)` resolves to a real QUIC caller
//! across the process boundary — the consumer code is unchanged, unaware which it got.
//!
//! As of Step 4 `remote` is **generic** process infrastructure in `core/`: it imports
//! only the foundations + `edge`, and NEVER any `api/` crate. The provider-swap
//! actions arrive as a `Vec<`[`RemoteFactory`]`>` — boxed closures produced by each
//! domain's `<name>rpc::remote_factories()` and passed into [`Stub::new`] by the
//! composition root (`cmd/*`). Each closure `provide`s a generated edge `Client` under
//! the SAME capability key the local impl would, and/or contributes front-door route
//! bindings. The generated `Client` implements the capability trait over an
//! [`opsapi::Caller`], and the wire shape + method names are OWNED by that generated
//! glue, so wire drift between the two sides is impossible — and `remote` never needs
//! to name the provider (it used to `match` on the provider string; that is gone).
//!
//! ## Front-door route bindings (the unified front-door end-state)
//! Beyond the capability swap, each provider arm ALSO contributes that provider's
//! `route_bindings()` — its `#[http]` [`opsapi::Operation`]+[`opsapi::OpBinding`]
//! pairs — into [`opsapi::SLOT`]/[`opsapi::BINDING_SLOT`] but NEVER [`opsapi::LOCAL_SLOT`]
//! (no local invoker exists here), so the gateway route table `select_kind`s the op
//! as `Remote` and dispatches it over the edge. The side effect is deliberate: ANY
//! process holding a `Stub` becomes front-capable for that provider. inventory-svc
//! already holds a `characters` stub, so after this it also routes `/characters` ops
//! remotely from its own front — the unified front-door end-state (a dedicated
//! `gateway-svc` is just a process whose ONLY modules are stubs).
//!
//! **Invariant — a `Stub` and its provider module are mutually exclusive in one
//! process.** A process holding BOTH `Stub("X")` and the real `X` module would
//! contribute X's routes twice (the module's own `operations()` + the stub's
//! `route_bindings()`). Stubs stand in ONLY for absent providers, so no binary does
//! this today; keep it that way (gateway-svc stays stub-only).
//!
//! ## The reconnecting caller
//! [`Reconnecting`] is a self-healing [`opsapi::Caller`]: it dials the peer LAZILY on
//! first use, holds the connection for reuse (persistent conn, stream-per-call), and
//! on a proven connection-fatal error drops the connection, but replays only methods
//! explicitly marked retry-safe; mutations return the first error and the next
//! request redials. Stream-local failures and peer answers preserve the shared
//! connection and are never replayed.
//! A dial failure — the peer is down — propagates to
//! the consumer, which maps it to a 503. The retry logic is generic over a private
//! [`Dialer`]/[`Conn`] seam so it is unit-testable with a fake transport (no QUIC).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use opsapi::{Caller, Error, RetryMode};
use tokio::sync::watch;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// The orchestrator agent's `resolve` client. It lives here (rather than in a
// `cmd/*` root) because it is `Stub`'s re-resolve source: `cmd/gateway-svc`'s main
// binds it into a [`PeerResolver`] (A5, single) or [`PeerListResolver`] (C2, the
// instance list) and threads it through [`PeerSource`], so a `Stub`'s
// [`Reconnecting`] conn or [`Pool`] re-resolves live without a consumer restart.
// ---------------------------------------------------------------------------
pub mod resolve;
pub use resolve::{resolve_peer, AddrKind, ErrorCode, ResolveError};

/// Fetches a peer's `#[http]` op manifest by calling the reserved
/// [`opsapi::DESCRIBE_METHOD`] op over `caller` — the CLIENT half of routing-as-data.
/// The gateway (D2) calls this once per resolved peer at boot and rebuilds its route
/// table from the returned [`opsapi::DescribeManifest`], so a new `#[http]` op on any
/// svc surfaces with zero gateway changes.
///
/// The op takes no arguments (empty payload, no identity) and is a read-only,
/// idempotent query, so it is allowed one replay after reconnect
/// ([`RetryMode::OnceAfterReconnect`]) — the same replay policy a `#[retry_safe]`
/// read gets. A malformed manifest is surfaced as an [`opsapi::Error`] with
/// [`opsapi::Status::Internal`], not silently dropped.
pub async fn describe(caller: &dyn Caller) -> Result<opsapi::DescribeManifest, Error> {
    let bytes = caller
        .call(
            opsapi::DESCRIBE_METHOD,
            None,
            &[],
            RetryMode::OnceAfterReconnect,
        )
        .await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::internal(format!("describe: malformed manifest: {e}")))
}

// ---------------------------------------------------------------------------
// The injected provider-swap action (the Step-4 generic-`remote` seam).
// ---------------------------------------------------------------------------

/// One provider-swap action handed to a [`Stub`] by the composition root. Applied in
/// [`Stub::register`] to the process [`Context`] and the stub's edge-backed
/// [`Caller`], it `provide`s a generated capability `Client` under the provider's
/// canonical registry key and/or contributes the provider's front-door route bindings.
///
/// The canonical type lives HERE (not in any `api/` crate) because `remote` is the
/// crate that CONSUMES the factories and already depends on both `lifecycle` and
/// `opsapi`; each domain's `<name>rpc` crate names it as `remote::RemoteFactory` and
/// its `remote_factories()` returns `Vec<remote::RemoteFactory>`. `remote` imports no
/// `api/` crate, so there is no cycle: the glue depends on `remote`, never the reverse.
pub type RemoteFactory = Box<dyn Fn(&Context, Arc<dyn Caller>) + Send + Sync>;

// ---------------------------------------------------------------------------
// The peer resolver seam — late-binding of a stub's address.
// ---------------------------------------------------------------------------

/// Resolves a peer's edge address `host:port` AT DIAL TIME (not frozen at boot), so an
/// address change is picked up without restarting the consumer. Invoked inside
/// [`EdgeDialer::dial`] where the frozen string used to be parsed, and by the stub's
/// background probe loop; because [`Reconnecting::get`] re-dials after a proven
/// connection-fatal reset, a resolver that returns a NEW address on the next call makes
/// re-resolve-on-reconnect fall out for free.
///
/// Returns an unparsed `host:port` string (parsed lazily by the dialer, preserving the
/// Unavailable-not-panic taxonomy) or a human error the dialer maps to
/// [`opsapi::Status::Unavailable`] (503) — an unresolvable peer is exactly as
/// unavailable as an unreachable one. In the single-address phase the resolver returns
/// one address; the LIST/load-balancing is a later phase.
pub type PeerResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// Where a [`Stub`]'s address(es) come from — and, with it, whether the stub's capability
/// caller is a single self-healing [`Reconnecting`] conn or a round-robin [`Pool`] across
/// N instances. Each variant carries a boot SNAPSHOT (fed to the gateway route table via
/// [`opsapi::PEER_SLOT`], contributed in `init` before any I/O — so it cannot itself
/// re-resolve):
///
/// * [`PeerSource::fixed`] — one constant address, [`Reconnecting`] over a
///   [`constant_resolver`]. The STANDALONE wiring: byte-identical boot, no re-resolve, no
///   pool. (Also the monolith / checker path via the `impl Into<PeerSource>` string
///   conversions, which keep every existing `Stub::new(provider, "host:port", …)` call
///   site unchanged.)
/// * [`PeerSource::resolving`] — one boot address plus a live single-address
///   [`PeerResolver`] (A5). Still a single [`Reconnecting`] conn; kept for the
///   single-instance managed re-resolve case and its regression tests.
/// * [`PeerSource::pooled`] — the boot instance SET plus a live [`PeerListResolver`] (C2).
///   The stub's capability caller becomes a [`Pool`] that round-robins across the
///   provider's live instances and re-resolves the LIST live. The MANAGED multi-instance
///   wiring. A one-element set degenerates to a pool-of-1.
pub enum PeerSource {
    /// A single self-healing connection over one live-resolving address.
    Single {
        boot_addr: String,
        resolver: PeerResolver,
    },
    /// A round-robin pool over the provider's live instance SET.
    Pooled {
        boot_addrs: Vec<String>,
        list: PeerListResolver,
    },
}

impl PeerSource {
    /// A constant address: the boot snapshot AND every dial resolve to the same value,
    /// so nothing re-resolves. The standalone wiring (a fixed env `host:port`).
    pub fn fixed(addr: impl Into<String>) -> PeerSource {
        let addr = addr.into();
        PeerSource::Single {
            boot_addr: addr.clone(),
            resolver: constant_resolver(addr),
        }
    }

    /// A boot snapshot (for the PEER_SLOT contribution the gateway route table reads)
    /// plus a live [`PeerResolver`] invoked on every dial — the managed single-instance
    /// wiring, where an orchestrator may move the one peer and the reconnecting caller
    /// must pick it up without a consumer restart.
    pub fn resolving(boot_addr: impl Into<String>, resolver: PeerResolver) -> PeerSource {
        PeerSource::Single {
            boot_addr: boot_addr.into(),
            resolver,
        }
    }

    /// A boot instance SET (contributed to PEER_SLOT so a co-hosted gateway route table
    /// pools across the same set) plus a live [`PeerListResolver`] — the managed
    /// multi-instance wiring (C2). The stub's capability caller is a [`Pool`] that
    /// round-robins across the live instances and re-resolves the list on its own
    /// cadence.
    pub fn pooled(boot_addrs: Vec<String>, list: PeerListResolver) -> PeerSource {
        PeerSource::Pooled { boot_addrs, list }
    }
}

impl From<&str> for PeerSource {
    fn from(addr: &str) -> Self {
        PeerSource::fixed(addr)
    }
}
impl From<&String> for PeerSource {
    fn from(addr: &String) -> Self {
        PeerSource::fixed(addr.clone())
    }
}
impl From<String> for PeerSource {
    fn from(addr: String) -> Self {
        PeerSource::fixed(addr)
    }
}

/// A [`PeerResolver`] that always yields `addr` — no re-resolution. Backs
/// [`PeerSource::fixed`] and the test-only edge caller.
fn constant_resolver(addr: String) -> PeerResolver {
    Arc::new(move || {
        let addr = addr.clone();
        Box::pin(async move { Ok(addr) })
    })
}

// ---------------------------------------------------------------------------
// The boot hook (Step 5) — a start-time async action a factory registers, run by
// the owning `Stub` in `start`.
// ---------------------------------------------------------------------------

/// The contrib slot [`RemoteBoot`] boot hooks are contributed to (by a factory in
/// [`Stub::register`]) and each [`Stub`] drains in `start`.
pub const BOOT_SLOT: contrib::Slot<RemoteBoot> = contrib::Slot::new("remote.boot");

/// Upper bound on one [`RemoteBoot`] hook (Step 11). Deliberately generous compared
/// to `edge::client::DIAL_DEADLINE` (5s, `core/edge`): a dial fails fast against a
/// dead/unreachable peer, but a boot hook's peer already answered the QUIC
/// handshake — it is presumed alive and doing real work (e.g. `configrpc`'s
/// `CachedConfig` boot-fill `snapshot()` call), so a slow-but-eventually-successful
/// boot should be given real headroom rather than racing the dial timeout. It still
/// MUST be finite: without a bound, a peer that accepts the connection but never
/// answers pins this hook forever — and because `App::start` awaits module starts
/// sequentially and unbounded, every module started after this stub never gets a
/// chance to run either. This is a core-leaf constant (never reads env — Hard
/// Constraint 1/5 in the workspace root doc); if a deployment ever needs a
/// different value, thread it through [`Stub::new`] the way the `peer` source already is,
/// NOT env.
const BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// A start-time async action bound to a provider, produced by a factory that needs a
/// boot step the pure `register` swap cannot do (a `register` is synchronous + does no
/// I/O). The canonical case is `configrpc`'s `CachedConfig`: the swap `provide`s the
/// cache in `register`, but the cache must be BOOT-FILLED by one async `snapshot()`
/// call, and that must FAIL LOUD if the peer is down. `RemoteBoot` carries that async
/// fill; the [`Stub`] runs it in `start`.
///
/// `provider` scopes the hook: a process can hold several `Stub`s (each drains
/// [`BOOT_SLOT`]), so each `Stub` runs ONLY the hooks tagged with its OWN provider —
/// so a hook runs exactly once, in its own provider's stub lifecycle.
#[derive(Clone)]
pub struct RemoteBoot {
    /// The provider this boot belongs to (matches the owning [`Stub::provider`]).
    provider: String,
    /// The async boot action, run once by the `Stub` in `start`.
    boot: Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
}

impl RemoteBoot {
    /// Binds a boot closure to `provider`. The closure is run once, in that provider's
    /// [`Stub`] `start`; an `Err` fails the process startup loudly.
    pub fn new<F>(provider: &str, boot: F) -> RemoteBoot
    where
        F: Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static,
    {
        RemoteBoot {
            provider: provider.to_string(),
            boot: Arc::new(boot),
        }
    }
}

// ---------------------------------------------------------------------------
// The reconnecting caller (Go's edgeConn) — generic over a dial/conn seam so the
// redial-once logic is testable with a fake transport.
// ---------------------------------------------------------------------------

/// One live connection to a peer: makes a single RPC, or is closed. The real impl is
/// [`edge::Client`]; the tests use a fake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureProvenance {
    ConnectionFatal,
    StreamLocal,
    PeerAnswer,
}

#[derive(Debug)]
struct CallFailure {
    mapped: Error,
    provenance: FailureProvenance,
}

#[async_trait]
trait Conn: Send + Sync {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, CallFailure>;
    fn close(&self);
}

/// Dials a fresh [`Conn`] to the peer. Called lazily by [`Reconnecting`] on first use
/// and again after a reset.
#[async_trait]
trait Dialer: Send + Sync {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error>;
}

/// A lazily-dialed, self-healing [`Caller`] over a [`Dialer`]. Holds at most one live
/// connection; only a proven connection-fatal error drops that connection and follows
/// the call's fail-closed [`RetryMode`]. Generic over `D` purely so tests can inject a
/// fake dialer.
struct Reconnecting<D: Dialer> {
    dialer: D,
    /// The cached live connection, or `None` before the first dial / after a reset.
    cur: Mutex<Option<Arc<dyn Conn>>>,
}

impl<D: Dialer> Reconnecting<D> {
    fn new(dialer: D) -> Self {
        Reconnecting {
            dialer,
            cur: Mutex::new(None),
        }
    }

    /// Returns a live connection, dialing if none is cached.
    async fn get(&self) -> Result<Arc<dyn Conn>, Error> {
        let mut g = self.cur.lock().await;
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let c = self.dialer.dial().await?;
        *g = Some(c.clone());
        Ok(c)
    }

    /// Drops the cached connection IF it is the one that just failed, so the next
    /// [`get`](Reconnecting::get) re-dials. Guarding on identity avoids closing a
    /// connection a concurrent caller already replaced (Go's `reset`).
    async fn reset(&self, failed: &Arc<dyn Conn>) {
        let mut g = self.cur.lock().await;
        if let Some(c) = g.as_ref() {
            if Arc::ptr_eq(c, failed) {
                c.close();
                *g = None;
            }
        }
    }

    /// Closes the cached connection (if any) — called from the stub's `stop`.
    async fn close(&self) {
        let mut g = self.cur.lock().await;
        if let Some(c) = g.take() {
            c.close();
        }
    }
}

#[async_trait]
impl<D: Dialer> Caller for Reconnecting<D> {
    /// One RPC. Only [`FailureProvenance::ConnectionFatal`] invalidates the cached
    /// connection. Stream-local failures and peer answers return as-is without reset,
    /// redial, or replay regardless of `retry_mode`. After a proven fatal failure,
    /// [`RetryMode::Never`] returns without replaying, while
    /// [`RetryMode::OnceAfterReconnect`] redials and replays at most once. A fatal
    /// replay failure resets the fresh connection too; a stream-local or peer-answer
    /// replay failure leaves it cached.
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
        retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        let c = self.get().await?;
        match c.call(method, identity, payload).await {
            Ok(v) => Ok(v),
            Err(first) if first.provenance != FailureProvenance::ConnectionFatal => {
                Err(first.mapped)
            }
            Err(first) => {
                self.reset(&c).await;
                if retry_mode == RetryMode::Never {
                    return Err(first.mapped);
                }
                let c2 = self.get().await?;
                match c2.call(method, identity, payload).await {
                    Ok(v) => Ok(v),
                    Err(second) if second.provenance != FailureProvenance::ConnectionFatal => {
                        Err(second.mapped)
                    }
                    Err(second) => {
                        // The replayed connection failed fatally too — invalidate it
                        // so the NEXT request redials instead of reusing a dead c2.
                        self.reset(&c2).await;
                        Err(second.mapped)
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The real edge-backed dial/conn seam.
// ---------------------------------------------------------------------------

/// Dials the peer's QUIC edge with the shared dev CA, producing an [`edge::Client`].
/// The address is RESOLVED and parsed lazily (at dial time) via [`EdgeDialer::resolve`],
/// so an address change is picked up on the next dial and a bad address surfaces as an
/// `Unavailable` error the consumer maps to 503, not a construction-time panic. Because
/// [`Reconnecting::get`] re-dials after a connection-fatal reset, the re-resolution
/// happens for free on every reconnect.
struct EdgeDialer {
    resolve: PeerResolver,
}

#[async_trait]
impl Dialer for EdgeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        // Re-resolve at dial time — the whole point of the resolver seam. A frozen
        // string could not target a moved peer without restarting the consumer.
        let peer = (self.resolve)().await.map_err(|e| {
            Error::unavailable(format!("remote: cannot resolve peer edge addr: {e}"))
        })?;
        let addr: SocketAddr = peer.parse().map_err(|e| {
            Error::unavailable(format!("remote: bad peer edge addr {peer:?}: {e}"))
        })?;
        // Mutual TLS: present this process's CA-signed client leaf and verify the peer
        // against the shared CA (no InsecureSkipVerify). `shared_dev_ca` resolves the
        // same process-wide anchor the peer's edge server trusts.
        let ca = edge::shared_dev_ca().map_err(Error::from)?;
        let client = edge::Client::dial(addr, &ca).await.map_err(Error::from)?;
        Ok(Arc::new(client))
    }
}

fn map_edge_call_failure(failure: edge::Error) -> CallFailure {
    let provenance = match &failure {
        edge::Error::Connection(_) => FailureProvenance::ConnectionFatal,
        edge::Error::Remote(_) | edge::Error::UnknownMethod(_) => {
            FailureProvenance::PeerAnswer
        }
        _ => FailureProvenance::StreamLocal,
    };
    CallFailure {
        mapped: Error::from(failure),
        provenance,
    }
}

#[async_trait]
impl Conn for edge::Client {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, CallFailure> {
        // Classify while the concrete edge cause is still available. Mapping to
        // opsapi erases this distinction (`Remote` and stream failures both become
        // Unavailable), so mapped status must never drive reset/replay decisions.
        self.call_raw_id(method, identity, payload)
            .await
            .map_err(map_edge_call_failure)
    }

    fn close(&self) {
        edge::Client::close(self);
    }
}

/// TEST-ONLY seam (B1 Step 1 abrupt-kill repro, `tests/abrupt_kill_redial.rs`): builds
/// the crate-private `Reconnecting<EdgeDialer>` — the EXACT caller a production
/// [`Stub`] shares with every generated client — and returns it as `Arc<dyn Caller>`.
/// The abrupt-kill repro must run in its OWN process (an integration test): the peer
/// is a killed-and-respawned CHILD process, so both sides must resolve the same
/// on-disk CA via `EDGE_CA_CERT`/`EDGE_CA_KEY`, and `edge::shared_dev_ca()` memoizes
/// on first use — inside the unit-test binary other tests already resolve a GENERATED
/// anchor first. Never call this from production code; it exists only so that
/// integration test can reach the private connection cache under test.
#[doc(hidden)]
pub fn test_only_reconnecting_edge_caller(peer: &str) -> Arc<dyn Caller> {
    Arc::new(Reconnecting::new(EdgeDialer {
        resolve: constant_resolver(peer.to_string()),
    }))
}

// ---------------------------------------------------------------------------
// The per-stub readiness probe (the `/readyz` contribution).
// ---------------------------------------------------------------------------

/// Probe cadence while the peer is reachable — gentle re-check so a healthy fleet
/// pays almost nothing.
const PROBE_INTERVAL_READY: Duration = Duration::from_secs(5);
/// Probe cadence while the peer is unreachable — fast so a recovery (or an initial
/// come-up) is detected quickly.
const PROBE_INTERVAL_UNREADY: Duration = Duration::from_secs(1);
/// Staleness bound (3x [`PROBE_INTERVAL_READY`]): if no probe has COMPLETED in this
/// long the readyz check reports unready even if the last cached verdict was `Ok`,
/// so a dead/stuck probe task cannot freeze a stale-green verdict.
const PROBE_STALL_MAX: Duration = Duration::from_secs(15);
/// Grace given to the probe task to observe the stop signal before it is aborted —
/// larger than `probe_peer`'s 1s inner dial timeout so a probe mid-dial finishes
/// cleanly rather than being force-aborted in the common case.
const PROBE_STOP_GRACE: Duration = Duration::from_secs(2);

/// Coarse monotonic seconds since first call, for the probe staleness stamp (mirrors
/// `asyncevents::coarse_now_secs`). Cheap, wall-clock-independent, never negative.
fn coarse_now_secs() -> u64 {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(std::time::Instant::now).elapsed().as_secs()
}

/// The stub's `/readyz` verdict as a PURE function of the cached probe result and the
/// staleness clock — NO I/O, no dialer, no async, so zero-I/O is guaranteed by
/// construction (not inferred from timing). `last_probe_at`/`now_secs` are coarse
/// seconds (`0` = never probed). A nonzero stamp older than [`PROBE_STALL_MAX`] flips
/// unready even if `cached` is `Ok`, so a dead probe task can't freeze a stale-green
/// verdict; a `0` stamp falls through to `cached` (the fail-closed `Err("probe pending")`
/// seed until the first probe completes).
fn readiness_verdict(
    cached: &Result<(), String>,
    last_probe_at: u64,
    now_secs: u64,
) -> Result<(), String> {
    if last_probe_at != 0 && now_secs.saturating_sub(last_probe_at) > PROBE_STALL_MAX.as_secs() {
        return Err(format!(
            "stub probe stalled: no completed peer probe in >{}s (probe task may have died)",
            PROBE_STALL_MAX.as_secs()
        ));
    }
    cached.clone()
}

/// The background reachability probe loop owned by each [`Stub`]. It is the ONLY
/// runtime caller of [`probe_peer`]: it dials the peer on a two-rate cadence (fast
/// while unready, gentle while ready), stamps the shared cached verdict + the
/// `last_probe_at` coarse timestamp, and never holds the std guard across the dial
/// (`probe_peer` is awaited BEFORE the lock). The `/readyz` [`httpmw::ReadyCheck`]
/// only READS that cache, so probe cost is decoupled from request rate.
async fn probe_loop(
    resolve: PeerResolver,
    verdict: Arc<StdMutex<Result<(), String>>>,
    last_probe_at: Arc<AtomicU64>,
    ready: Duration,
    unready: Duration,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        // Dial OUTSIDE any lock — the std guard must never cross an `.await`. Re-resolve
        // each pass through the SAME resolver the dialer uses, so a moved peer flips the
        // readyz verdict too; an unresolvable peer is itself an unready verdict.
        let v = probe_via_resolver(&resolve).await;
        let is_err = v.is_err();
        *verdict.lock().unwrap_or_else(|e| e.into_inner()) = v;
        // `.max(1)` so a completed probe at t=0 is distinguishable from "never probed".
        last_probe_at.store(coarse_now_secs().max(1), Ordering::SeqCst);
        let wait = if is_err { unready } else { ready };
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// Resolves the peer address through the SAME [`PeerResolver`] the dialer uses, then
/// runs the bounded connectivity [`probe_peer`] against it. A resolver failure is itself
/// an unready verdict — a peer whose address cannot be resolved is not reachable.
async fn probe_via_resolver(resolve: &PeerResolver) -> Result<(), String> {
    let addr = resolve()
        .await
        .map_err(|e| format!("cannot resolve peer edge addr: {e}"))?;
    probe_peer(addr).await
}

/// The bounded connectivity probe backing each stub's `/readyz` [`httpmw::ReadyCheck`].
/// Parses `peer_addr`, resolves the shared dev CA, and dials the peer's QUIC edge —
/// returning `Ok(())` iff a fresh mTLS connection completes, or an `Err(String)`
/// naming the failure (bad addr / unavailable CA / timeout / dial error). The dial is
/// wrapped in a HARD 1s inner timeout, deliberately embedded here rather than relying
/// solely on `core/app`'s outer `READY_CHECK_TIMEOUT`: a hung QUIC handshake must not
/// outlive the probe and leak a pending dial even if the outer bound were ever removed.
/// The dial mirrors [`EdgeDialer::dial`] (same shared-anchor mutual-TLS path).
async fn probe_peer(peer_addr: String) -> Result<(), String> {
    let addr: SocketAddr = peer_addr
        .parse()
        .map_err(|e| format!("bad peer edge addr {peer_addr:?}: {e}"))?;
    let ca = edge::shared_dev_ca().map_err(|e| format!("shared dev CA unavailable: {e}"))?;
    match tokio::time::timeout(Duration::from_secs(1), edge::Client::dial(addr, &ca)).await {
        Err(_elapsed) => Err(format!("dial to {addr} timed out after 1s")),
        Ok(Err(e)) => Err(format!("dial to {addr} failed: {e}")),
        Ok(Ok(client)) => {
            // A completed handshake is the readiness signal; drop the probe connection
            // immediately (the real capability calls hold their own reconnecting conn).
            client.close();
            Ok(())
        }
    }
}

// ===========================================================================
// The client-side connection POOL (C1) — round-robin across a provider's live
// instances. It sits BESIDE `Reconnecting`: where `Reconnecting` holds ONE
// self-healing connection to ONE peer, a `Pool` holds ONE `Reconnecting` PER
// instance and spreads calls across the healthy ones. `Pool` is itself an
// `opsapi::Caller`, so it is a drop-in for a capability EXACTLY the way
// `Reconnecting` is — the gateway/consumer code that `require`s the capability is
// unchanged and unaware it got a pool.
//
// The seam that makes it work: each instance's edge address is FIXED (an instance
// does not move — it is the LIST of instances that changes as the orchestrator scales
// up/down), so each per-instance `Reconnecting<EdgeDialer>` dials a CONSTANT address
// (a `constant_resolver`, re-resolving nothing). It is the POOL that re-resolves the
// LIST — via a [`PeerListResolver`], the multi-address generalization of the single
// [`PeerResolver`] — and reconciles its instance set: a new address gets a fresh
// per-instance connection + probe, a removed address is dropped and its probe torn
// down. Per-instance health comes from a probe fan-out (one [`probe_loop`] per
// instance, one verdict each) so selection skips a known-dead instance.
// ===========================================================================

/// Resolves ALL live instance addresses of a provider — the load-balancing
/// generalization of [`PeerResolver`]. In managed mode this wraps `resolve_peer`
/// WITHOUT the gateway's old `exactly_one` collapse (C2), so it answers the whole
/// `Vec<String>`; standalone it is a constant one-element list. Invoked by
/// [`Pool::refresh`] to reconcile the instance set; an `Err` (agent unreachable) leaves
/// the existing set in place for a later retry rather than tearing every instance down.
pub type PeerListResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<String>, String>> + Send + Sync>;

/// How often [`Pool::call`] will RE-RESOLVE the instance list. The per-instance dialers
/// re-dial on their own reconnect and the per-instance probes flip health on their own
/// (1–5s) cadence; the LIST (which instances exist) changes only when the orchestrator
/// scales, so re-resolving it every request would hammer the agent for no benefit. The
/// throttle keeps `call` cheap in the common case (a claimed-slot compare-exchange, no
/// network) while still picking up a scale event within this bound. C2 may additionally
/// drive a background refresh; the throttle makes the two idempotent.
const POOL_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Per-instance health, stamped by that instance's own [`probe_loop`] and READ (zero
/// I/O) by pool selection + the pool's `/readyz`. One of these PER instance — the
/// fan-out of the single-`Stub` verdict cache. Reuses [`readiness_verdict`] so the
/// dead-probe-task staleness guard applies per instance too.
struct InstanceHealth {
    verdict: Arc<StdMutex<Result<(), String>>>,
    last_probe_at: Arc<AtomicU64>,
}

impl InstanceHealth {
    /// The fail-closed seed: unknown reachability = not healthy until the first probe
    /// completes (mirrors the single `Stub`'s verdict seed).
    fn seed() -> InstanceHealth {
        InstanceHealth {
            verdict: Arc::new(StdMutex::new(Err("probe pending".to_string()))),
            last_probe_at: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A COMPLETED probe verdict of `Ok`, fresh (not stale-flipped). Drives the pool's
    /// `/readyz` some-down-vs-all-down decision.
    fn healthy(&self, now: u64) -> bool {
        let cached = self.verdict.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let stamp = self.last_probe_at.load(Ordering::SeqCst);
        readiness_verdict(&cached, stamp, now).is_ok()
    }

    /// Whether pool SELECTION may route to this instance. Selectable = never-probed
    /// (`stamp == 0`, the optimistic cold-start — the single-conn caller likewise always
    /// ATTEMPTS a dial rather than gating on the probe) OR currently [`healthy`]. A
    /// completed-and-failed (or stale) probe makes an instance non-selectable, so
    /// selection skips a known corpse instead of routing part of the traffic nowhere.
    fn is_selectable(&self, now: u64) -> bool {
        let stamp = self.last_probe_at.load(Ordering::SeqCst);
        stamp == 0 || self.healthy(now)
    }
}

/// The teardown handle for one instance's background probe task (present only for a
/// real edge instance; a test/fake instance has `None`).
struct ProbeHandle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Closes an instance's underlying edge connection on teardown. Boxed because [`Pool`]
/// holds each instance's caller as `Arc<dyn Caller>` (for fake-injectability) and
/// `Caller` has no `close` — the edge factory captures the concrete `Reconnecting` in
/// this closure so the pool can still drain a dropped instance's connection.
type InstanceCloser = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// One pooled instance: a FIXED edge address, its own self-healing caller (a
/// `Reconnecting<EdgeDialer>` on a constant resolver in production), its own probe-fed
/// [`InstanceHealth`], and the teardown handles for the probe + connection.
struct Instance {
    addr: String,
    caller: Arc<dyn Caller>,
    health: Arc<InstanceHealth>,
    probe: Option<ProbeHandle>,
    close: InstanceCloser,
}

impl Instance {
    /// Grace-then-abort the probe task (mirrors [`Stub::stop`]) — called when this
    /// instance is dropped from the set (scaled away) or the whole pool stops.
    async fn stop_probe(&mut self) {
        if let Some(p) = self.probe.take() {
            let _ = p.stop.send(true);
            let mut task = p.task;
            match tokio::time::timeout(PROBE_STOP_GRACE, &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }
}

/// Builds a per-instance [`Instance`] factory over the REAL edge transport: a fixed
/// address ⇒ a `constant_resolver` ⇒ a `Reconnecting<EdgeDialer>` caller + a spawned
/// [`probe_loop`] stamping that instance's health. Injected into [`Pool::with_factory`]
/// so tests can swap a fake transport for it.
type InstanceFactory = Arc<dyn Fn(&str) -> Instance + Send + Sync>;

fn edge_instance_factory(ready: Duration, unready: Duration) -> InstanceFactory {
    Arc::new(move |addr: &str| {
        let addr = addr.to_string();
        let resolver = constant_resolver(addr.clone());
        let recon = Arc::new(Reconnecting::new(EdgeDialer {
            resolve: resolver.clone(),
        }));
        let caller: Arc<dyn Caller> = recon.clone();
        let health = Arc::new(InstanceHealth::seed());
        // One probe loop PER instance (the fan-out), dialing this instance's fixed addr.
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(probe_loop(
            resolver,
            health.verdict.clone(),
            health.last_probe_at.clone(),
            ready,
            unready,
            stop_rx,
        ));
        let close: InstanceCloser = Arc::new(move || {
            let recon = recon.clone();
            Box::pin(async move { recon.close().await })
        });
        Instance {
            addr,
            caller,
            health,
            probe: Some(ProbeHandle { stop, task }),
            close,
        }
    })
}

/// A round-robin [`Caller`] over N per-instance connections. Holds a [`PeerListResolver`]
/// (the LIST source), the current instance set, an atomic round-robin cursor, and (via
/// each instance) probe-fed per-instance health. `call` refreshes the set (throttled),
/// picks the next SELECTABLE instance round-robin, and delegates to that instance's own
/// self-healing caller.
pub struct Pool {
    list: PeerListResolver,
    factory: InstanceFactory,
    /// The current instance set, reconciled by [`refresh`](Pool::refresh). A `std` mutex
    /// (never held across an `.await`): the delegated call happens AFTER the guard drops.
    instances: StdMutex<Vec<Instance>>,
    /// The round-robin cursor — advanced once per `call`, `% len` selects the start slot.
    cursor: AtomicU64,
    /// Coarse seconds of the last list re-resolution (`0` = never), backing the
    /// [`POOL_REFRESH_INTERVAL`] throttle so `call` does not re-resolve every request.
    last_refresh: AtomicU64,
    /// Monotonic per-resolve generation, assigned at the START of each resolve so a SLOW
    /// resolve (window N) cannot clobber a NEWER one (window N+1) that finished first: a
    /// resolve applies its reconcile ONLY if its generation is newer than the last APPLIED
    /// generation. Without this, a resolve taking longer than [`POOL_REFRESH_INTERVAL`]
    /// could land its stale list over a fresher set (transient staleness up to one interval).
    resolve_seq: AtomicU64,
    /// The generation of the resolve whose reconcile last WON (latest-resolve-wins). Read +
    /// written under the `instances` lock, so the generation check and the reconcile are
    /// atomic against a concurrent resolve.
    applied_gen: AtomicU64,
}

impl Pool {
    /// Builds a pool over the REAL edge transport (production). The instance set starts
    /// EMPTY and is populated on the first [`call`](Pool::call) refresh; until a probe
    /// completes the pool reports `/readyz` down (fail-closed cold start).
    pub fn new(list: PeerListResolver) -> Pool {
        Pool::with_factory(
            list,
            edge_instance_factory(PROBE_INTERVAL_READY, PROBE_INTERVAL_UNREADY),
        )
    }

    /// The factory-injectable constructor — production passes [`edge_instance_factory`];
    /// tests pass a fake-transport factory.
    fn with_factory(list: PeerListResolver, factory: InstanceFactory) -> Pool {
        Pool {
            list,
            factory,
            instances: StdMutex::new(Vec::new()),
            cursor: AtomicU64::new(0),
            last_refresh: AtomicU64::new(0),
            resolve_seq: AtomicU64::new(0),
            applied_gen: AtomicU64::new(0),
        }
    }

    /// Re-resolves the instance LIST (throttled by [`POOL_REFRESH_INTERVAL`]) and
    /// reconciles the set: add addresses that appeared, drop (and tear down) addresses
    /// that vanished, keep the rest untouched (so an existing instance's connection +
    /// cursor position + probe survive a refresh). A single concurrent caller wins the
    /// refresh (compare-exchange on `last_refresh`); the resolve runs OUTSIDE the set
    /// lock. A resolver error leaves the existing set in place.
    async fn refresh(&self) {
        let now = coarse_now_secs();
        let last = self.last_refresh.load(Ordering::SeqCst);
        if last != 0 && now.saturating_sub(last) < POOL_REFRESH_INTERVAL.as_secs() {
            return;
        }
        // Claim the refresh slot so two concurrent calls do not both resolve + rebuild.
        if self
            .last_refresh
            .compare_exchange(last, now.max(1), Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.refresh_once().await;
    }

    /// One resolve + reconcile, unthrottled (the [`refresh`](Pool::refresh) throttle gates
    /// how OFTEN this runs; the generation guard here handles ORDER when two do overlap).
    /// Assigns a generation at the start, resolves OUTSIDE the set lock, then applies the
    /// reconcile only if this is still the newest resolve — a slower older resolve is
    /// DROPPED (latest-resolve-wins). Vanished-instance teardown is DETACHED so it never
    /// sits on the request path (a scale-down removing K instances must not add
    /// K × [`PROBE_STOP_GRACE`] to the one request that won the refresh).
    async fn refresh_once(&self) {
        // Generation assigned BEFORE the resolve await, so claim order = generation order.
        let my_gen = self.resolve_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let addrs = match (self.list)().await {
            Ok(a) => a,
            // Keep the existing set; the next refresh (>= one interval later) retries.
            Err(_e) => return,
        };
        let removed = {
            let mut g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
            // Latest-resolve-wins: if a NEWER resolve already applied, drop this stale
            // result rather than clobbering the fresher set. The check + store + reconcile
            // are atomic under the set lock. (finding-5: a `call` may already hold a clone
            // of an instance's caller that reconcile drops here; a concurrent close then
            // yields a fail-closed 503 in a tiny window — self-limited, no corruption.)
            if my_gen <= self.applied_gen.load(Ordering::SeqCst) {
                Vec::new()
            } else {
                self.applied_gen.store(my_gen, Ordering::SeqCst);
                self.reconcile(&mut g, addrs)
            }
        };
        // Tear down vanished instances OFF the request path: detach so a slow
        // stop_probe/close (up to PROBE_STOP_GRACE each) never delays the caller. Each
        // task is bounded per instance and self-completes.
        if !removed.is_empty() {
            tokio::spawn(async move {
                for mut inst in removed {
                    inst.stop_probe().await;
                    (inst.close)().await;
                }
            });
        }
    }

    /// Reconciles `current` toward `addrs` (deduped, order-preserving): keep an existing
    /// instance whose addr is still wanted, build a fresh one for a new addr, and RETURN
    /// the instances whose addr vanished (the caller tears them down). Pure set algebra —
    /// spawning happens inside the factory, no `.await` here, so it runs under the lock.
    fn reconcile(&self, current: &mut Vec<Instance>, addrs: Vec<String>) -> Vec<Instance> {
        let mut seen = std::collections::HashSet::new();
        let wanted: Vec<String> = addrs.into_iter().filter(|a| seen.insert(a.clone())).collect();
        let mut removed = Vec::new();
        let mut kept: Vec<Instance> = Vec::new();
        for inst in current.drain(..) {
            if wanted.contains(&inst.addr) {
                kept.push(inst);
            } else {
                removed.push(inst);
            }
        }
        let mut result: Vec<Instance> = Vec::with_capacity(wanted.len());
        for addr in &wanted {
            if let Some(pos) = kept.iter().position(|i| &i.addr == addr) {
                result.push(kept.remove(pos));
            } else {
                result.push((self.factory)(addr));
            }
        }
        *current = result;
        removed
    }

    /// Picks the next SELECTABLE instance round-robin. Advances the cursor ONCE (so calls
    /// spread even across a stable set), then scans from that slot, skipping non-selectable
    /// (known-dead / stale) instances. `None` = empty set OR every instance known-down,
    /// which [`call`](Pool::call) maps to `Unavailable`.
    fn select(&self, instances: &[Instance], now: u64) -> Option<usize> {
        self.select_excluding(instances, now, None)
    }

    /// [`select`](Pool::select) with an OPTIONAL excluded address — the cursor half of the
    /// C3 cross-instance failover. On a permitted retry the pool must land on a DIFFERENT
    /// instance than the one that just died, so `exclude` names the dead instance's addr
    /// (matched by addr, not index, so a concurrent refresh reconciling the set cannot make
    /// the exclusion target the wrong slot). Advances the cursor exactly as `select` does —
    /// the cursor is the WHERE-a-retry-lands input, orthogonal to `RetryMode`'s WHETHER.
    /// `None` = no selectable instance OTHER than `exclude` exists.
    fn select_excluding(
        &self,
        instances: &[Instance],
        now: u64,
        exclude: Option<&str>,
    ) -> Option<usize> {
        let n = instances.len();
        if n == 0 {
            return None;
        }
        let start = (self.cursor.fetch_add(1, Ordering::SeqCst) % n as u64) as usize;
        for off in 0..n {
            let i = (start + off) % n;
            if exclude == Some(instances[i].addr.as_str()) {
                continue;
            }
            if instances[i].health.is_selectable(now) {
                return Some(i);
            }
        }
        None
    }

    /// The pool's `/readyz` verdict — some-down vs all-down. Zero I/O: reads the current
    /// instance set + each instance's cached probe verdict, resolving nothing. Ready iff
    /// at least one instance is [`healthy`](InstanceHealth::healthy); Down only when EVERY resolved
    /// instance is down (or none resolved yet — the fail-closed cold start). This is the
    /// rethink C1 requires: a pool with one dead instance out of three is still Ready.
    fn readiness_at(&self, now: u64) -> Result<(), String> {
        let g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_empty() {
            return Err("remote pool: no resolved instances yet".to_string());
        }
        let total = g.len();
        let healthy = g.iter().filter(|i| i.health.healthy(now)).count();
        if healthy == 0 {
            return Err(format!("remote pool: all {total} instance(s) down"));
        }
        Ok(())
    }

    /// [`readiness_at`](Pool::readiness_at) at the current coarse time — the entry point a
    /// C2 `httpmw::ReadyCheck` will call (zero I/O, reads the cached verdicts only).
    pub fn readyz(&self) -> Result<(), String> {
        self.readiness_at(coarse_now_secs())
    }

    /// Tears down every instance (probe task + connection) — the pool's `stop`, wired by
    /// the owning `Stub` in C2.
    pub async fn stop(&self) {
        let taken: Vec<Instance> = {
            let mut g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *g)
        };
        for mut inst in taken {
            inst.stop_probe().await;
            (inst.close)().await;
        }
    }
}

#[async_trait]
impl Caller for Pool {
    /// Refresh (throttled) ⇒ select a healthy instance round-robin ⇒ delegate to that
    /// instance's own self-healing [`Reconnecting::call`] ⇒ on a non-peer-answered failure of
    /// a `#[retry_safe]` read, fail over ONCE to a DIFFERENT instance.
    ///
    /// **Two orthogonal inputs — `retry_mode` is the WHETHER authority; the cursor is WHERE:**
    /// * `retry_mode` decides WHETHER a failure may be retried at all. It stays the sole
    ///   WHETHER authority — `opsapi::RetryMode`, never a new pool knob. `RetryMode::Never`
    ///   (a mutation) is NEVER retried onto another instance: a mid-call instance death may
    ///   have executed the mutation, so re-sending it to a peer risks a double-execute. The
    ///   error is returned verbatim (the side effect ran AT MOST once). This gate is checked
    ///   FIRST, so a mutation can never reach the failover regardless of the failure class.
    /// * the round-robin `cursor` decides WHERE a PERMITTED (`OnceAfterReconnect`) retry
    ///   lands. It is the legitimate SECOND input the cross-instance retry needs — it must
    ///   advance OFF the dead instance (`select_excluding`) or the replay hits the same
    ///   corpse. It is NOT a smuggled "retry" knob; it only chooses the target of a retry
    ///   `retry_mode` already permitted.
    ///
    /// The cross-instance retry fires ONLY when ALL of:
    /// 1. `retry_mode == RetryMode::OnceAfterReconnect` (a read / `#[retry_safe]` op); AND
    /// 2. the failure is a NON-PEER-ANSWERED error — `!status.is_definitive_answer()`, i.e.
    ///    any status OTHER than `NotFound`. A definitive answer (`NotFound` = `UnknownMethod`,
    ///    the peer demonstrably received and answered) means the op RAN on a reachable
    ///    instance; re-running it elsewhere would double-execute, so it is returned verbatim.
    ///    (A domain error rides INSIDE the response envelope as `Ok(bytes)` at this boundary —
    ///    it never reaches this gate; the only `Err` statuses here are `NotFound` from
    ///    `UnknownMethod` and `Unavailable` from every other edge fault — see
    ///    `From<edge::Error> for opsapi::Error`.) AND
    /// 3. a DIFFERENT selectable instance exists (`select_excluding` off the dead addr).
    ///
    /// **This gate is BROADER than [`Reconnecting`]'s `ConnectionFatal` reset class — NOT the
    /// same authority.** `Reconnecting` resets only on `FailureProvenance::ConnectionFatal`,
    /// computed from the concrete `edge::Error::Connection` while the provenance is still
    /// visible. That provenance is ERASED by `From<edge::Error>`: `edge::Error::Remote` (the
    /// peer ANSWERED `ok:false` — a reached handler/dispatch error on an ALIVE instance) and
    /// every StreamLocal fault (Io/Codec/Stream) BOTH collapse to `Unavailable`, exactly like
    /// a true connection death. At the Pool boundary the type system cannot tell a
    /// peer-answered `Unavailable` from a connection-death `Unavailable`, so this gate fails
    /// over on the broader `!= NotFound` set — including cases where `Reconnecting` would NOT
    /// have reset. That is SAFE, not a bug: the `retry_mode` check runs FIRST, so ONLY
    /// idempotent `#[retry_safe]`/`OnceAfterReconnect` ops ever reach here; a wasted failover
    /// on a deterministic peer-answered `Unavailable` is one extra call (a minor
    /// inefficiency), never a correctness issue. Tightening to `Reconnecting`'s provenance
    /// would require surfacing provenance across the `opsapi::Error` boundary — a larger
    /// change with no correctness payoff, deliberately not done.
    ///
    /// **Version-skew nit:** a `#[retry_safe]` read to an OLD instance that lacks the method
    /// returns `UnknownMethod → NotFound` (definitive) → NO failover, even if a
    /// rolling-deployed instance J DOES serve it. Consistent with edge's documented
    /// "unknown-method is not retryable" stance; mutation-safety is unaffected.
    ///
    /// The retry is BOUNDED to exactly one cross-instance attempt — J's result (success or
    /// error) is returned as-is; there is no third instance, no loop. [`Reconnecting`]'s own
    /// single-conn `RetryMode` semantics are UNCHANGED — this only adds the pool's outer,
    /// cross-instance failover on top of it.
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
        retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        self.refresh().await;
        let now = coarse_now_secs();
        // Clone the selected instance's caller + addr OUT and drop the set lock before
        // delegating (a std guard must never cross an `.await`). The addr is the exclusion
        // target for a permitted cross-instance retry (matched by addr, not slot, so a
        // concurrent reconcile cannot make the exclusion target the wrong instance).
        // finding-5: a concurrent refresh may reconcile this instance away and close it
        // between this clone and the delegated call, yielding a fail-closed
        // `ConnectionFatal`/503 in that tiny window — self-limited (the next request
        // re-selects a live instance), no state corruption.
        let first = {
            let g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
            self.select(&g, now).map(|i| (g[i].caller.clone(), g[i].addr.clone()))
        };
        let Some((first_caller, first_addr)) = first else {
            return Err(Error::unavailable(
                "remote pool: no reachable instance (all resolved instances down, or none resolved yet)",
            ));
        };
        let first_err = match first_caller.call(method, identity, payload, retry_mode).await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };

        // C3 cross-instance failover gate. WHETHER (retry_mode), checked FIRST — a mutation
        // (`Never`) is returned verbatim, never re-sent (double-execute hazard), so no
        // failure class can carry it past here. Then fail over on any NON-peer-answered error
        // (`!is_definitive_answer`, i.e. any status other than `NotFound`); a peer that
        // answered (`UnknownMethod → NotFound`) ran the op, so return it verbatim rather than
        // re-run it. NOTE: this `!= NotFound` set is BROADER than `Reconnecting`'s
        // `ConnectionFatal` reset class — `Remote`/StreamLocal faults also map to
        // `Unavailable` and are indistinguishable at this boundary — but it is SAFE because
        // the `retry_mode` check above already excluded mutations; the extra failover on a
        // peer-answered `Unavailable` is a minor inefficiency, not a correctness issue.
        if retry_mode != RetryMode::OnceAfterReconnect || first_err.status.is_definitive_answer()
        {
            return Err(first_err);
        }

        // WHERE (cursor): advance OFF the dead instance to a DIFFERENT one. If none exists,
        // return the first instance's error verbatim — the failover had nowhere to land.
        let second_caller = {
            let g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
            self.select_excluding(&g, now, Some(&first_addr))
                .map(|i| g[i].caller.clone())
        };
        let Some(second_caller) = second_caller else {
            return Err(first_err);
        };
        // Bounded to ONE cross-instance attempt: J's result (success or error) is final.
        second_caller.call(method, identity, payload, retry_mode).await
    }
}

/// Safety net for the leak class: a `Pool` dropped WITHOUT [`stop`](Pool::stop) (the
/// graceful path C2 wires) would otherwise leave every per-instance probe `JoinHandle`
/// running detached. Drop ABORTS them synchronously — abort is sync-safe and Drop must
/// never block/await, so this only aborts the tasks (connections close as their `Arc`s
/// drop); [`stop`](Pool::stop) stays the graceful grace-then-abort path.
impl Drop for Pool {
    fn drop(&mut self) {
        if let Ok(g) = self.instances.get_mut() {
            for inst in g.iter() {
                if let Some(p) = inst.probe.as_ref() {
                    p.task.abort();
                }
            }
        }
    }
}

#[cfg(test)]
impl Pool {
    /// Directly reconcile the set (bypassing the throttle/resolver) so a unit test can
    /// prove the add/keep/drop structure without driving `call`.
    fn reconcile_for_test(&self, addrs: Vec<String>) -> Vec<Instance> {
        let mut g = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        self.reconcile(&mut g, addrs)
    }
}

// ---------------------------------------------------------------------------
// The Stub module — the swap.
// ---------------------------------------------------------------------------

/// Stands in for a provider hosted in a PEER process. Its [`Module::name`] reports the
/// PROVIDER name (`"characters"`) so `app::validate_requires` sees a co-hosted
/// consumer's requirement satisfied; its phase-1 `register` `provide`s edge-backed
/// clients under the SAME capability keys the local impl would. It migrates no schema
/// and mounts no routes; its `stop` closes the underlying edge connection on
/// shutdown.
pub struct Stub {
    /// The provider name — also the [`Module::name`], so `validate_requires` matches.
    provider: String,
    /// The peer's edge address SET as UNPARSED strings — the BOOT SNAPSHOT (one element
    /// for a [`Backing::Single`] stub, ALL live instances for a [`Backing::Pooled`] one).
    /// Contributed to [`opsapi::PEER_SLOT`] in `init` so a co-hosted gateway front door
    /// dials this provider Remote without reading env — the topology this composition root
    /// injected via [`Stub::new`]. This slot is filled in `init` (before any I/O), so it
    /// cannot itself re-resolve; the LIVE re-resolution is the resolver the [`EdgeDialer`]
    /// (single) or [`Pool`] (pooled) holds.
    peer_addrs: Vec<String>,
    /// The capability caller + its liveness machinery — a single self-healing
    /// [`Reconnecting`] conn ([`Backing::Single`]) or a round-robin [`Pool`] over N
    /// instances ([`Backing::Pooled`]). The variant is decided by the [`PeerSource`] at
    /// [`Stub::new`].
    backing: Backing,
    /// The provider-swap closures this stub applies in `register`. Injected by the
    /// composition root from the provider's `<name>rpc::remote_factories()` — `remote`
    /// never names the provider itself.
    factories: Vec<RemoteFactory>,
}

/// The stub's capability caller + liveness machinery. Single vs pooled is the whole
/// topology difference between the standalone/monolith path and a managed multi-instance
/// front door — every other Stub method (`name`, `requires`, boot hooks, the PEER_SLOT
/// contribution) is backing-agnostic.
enum Backing {
    /// One self-healing connection (STANDALONE / managed single instance).
    Single(SingleBacking),
    /// A round-robin [`Pool`] across the provider's live instances (MANAGED, C2).
    Pooled(PooledBacking),
}

/// The single-connection backing: a [`Reconnecting`] conn plus the background reachability
/// probe that feeds a cached `/readyz` verdict (unchanged from the pre-pool Stub).
struct SingleBacking {
    /// The dial-time peer resolver — the SAME one the [`EdgeDialer`] inside `conn` and the
    /// background probe loop use, so a moved peer is picked up on the next reconnect.
    resolver: PeerResolver,
    /// The lazily-dialed, self-healing caller shared by every generated client.
    conn: Arc<Reconnecting<EdgeDialer>>,
    /// The cached peer-reachability verdict, stamped by [`probe_loop`] and READ (zero I/O)
    /// by the `/readyz` [`httpmw::ReadyCheck`]. Seeded fail-closed (`Err("probe pending")`).
    verdict: Arc<StdMutex<Result<(), String>>>,
    /// Coarse seconds of the last COMPLETED probe (`0` = never) — the readyz staleness guard.
    last_probe_at: Arc<AtomicU64>,
    /// Stop signal for the background probe task (`None` until `start`).
    probe_stop: StdMutex<Option<watch::Sender<bool>>>,
    /// The background probe task handle, torn down in `stop` (`None` until `start`).
    probe_task: StdMutex<Option<JoinHandle<()>>>,
}

/// The pooled backing (C2): a [`Pool`] whose per-instance probes + `/readyz` it owns, plus
/// a background loop that drives [`Pool::refresh`] on the same cadence the single probe
/// runs — so the pool populates its instance set + probes even with no capability calls yet
/// (the readyz cold-start otherwise stays down until the first request).
struct PooledBacking {
    pool: Arc<Pool>,
    /// Stop signal for the background refresh loop (`None` until `start`).
    refresh_stop: StdMutex<Option<watch::Sender<bool>>>,
    /// The background refresh task handle, torn down in `stop` (`None` until `start`).
    refresh_task: StdMutex<Option<JoinHandle<()>>>,
}

/// Drives [`Pool::refresh`] on the [`POOL_REFRESH_INTERVAL`] cadence so a pooled stub's
/// instance set + per-instance probes come up (and stay reconciled) independently of
/// request traffic — the pooled mirror of [`probe_loop`]. `refresh` is internally
/// throttled, so the first pass populates immediately and later passes pick up scale
/// events; teardown is the same grace-then-abort `stop` uses.
async fn pool_refresh_loop(pool: Arc<Pool>, mut stop: watch::Receiver<bool>) {
    loop {
        pool.refresh().await;
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(POOL_REFRESH_INTERVAL) => {}
        }
    }
}

impl Stub {
    /// Builds a stub for `provider` from a [`PeerSource`]: a fixed `host:port` string
    /// (`"127.0.0.1:9000"`, via `impl Into<PeerSource>`) or [`PeerSource::resolving`] →
    /// a [`Backing::Single`] self-healing [`Reconnecting`] conn (standalone / managed
    /// single instance); [`PeerSource::pooled`] → a [`Backing::Pooled`] round-robin
    /// [`Pool`] over the provider's live instance SET (managed multi-instance, C2). The
    /// address(es) are RESOLVED lazily (re-resolving on reconnect / refresh for free), and
    /// `factories` (the provider's `<name>rpc::remote_factories()`) are applied at
    /// `register`. An EMPTY `factories` vec is a wiring bug — the stub would provide
    /// nothing — and fails loudly at `register`.
    pub fn new(
        provider: &str,
        peer: impl Into<PeerSource>,
        factories: Vec<RemoteFactory>,
    ) -> Stub {
        let (peer_addrs, backing) = match peer.into() {
            PeerSource::Single { boot_addr, resolver } => {
                let backing = Backing::Single(SingleBacking {
                    resolver: resolver.clone(),
                    conn: Arc::new(Reconnecting::new(EdgeDialer { resolve: resolver })),
                    // Fail-closed seed: unknown reachability = not ready until the first
                    // probe completes.
                    verdict: Arc::new(StdMutex::new(Err("probe pending".to_string()))),
                    last_probe_at: Arc::new(AtomicU64::new(0)),
                    probe_stop: StdMutex::new(None),
                    probe_task: StdMutex::new(None),
                });
                (vec![boot_addr], backing)
            }
            PeerSource::Pooled { boot_addrs, list } => {
                let backing = Backing::Pooled(PooledBacking {
                    pool: Arc::new(Pool::new(list)),
                    refresh_stop: StdMutex::new(None),
                    refresh_task: StdMutex::new(None),
                });
                (boot_addrs, backing)
            }
        };
        Stub {
            provider: provider.to_string(),
            peer_addrs,
            backing,
            factories,
        }
    }

    /// Spawns the background reachability probe loop for a [`Backing::Single`] stub
    /// (idempotent-callee contract: call exactly once, from `start`). Splits the spawn
    /// out of `start` so tests can drive it with short intervals without threading them
    /// through [`Stub::new`]. Production calls it with the const cadence; the loop is the
    /// sole runtime caller of [`probe_peer`]. A no-op on a pooled stub (whose per-instance
    /// probes the [`Pool`] owns) — `start` routes a pooled stub to [`Stub::spawn_pool_refresh`].
    pub(crate) fn spawn_probe(&self, ready: Duration, unready: Duration) {
        let Backing::Single(s) = &self.backing else {
            return;
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = tokio::spawn(probe_loop(
            s.resolver.clone(),
            s.verdict.clone(),
            s.last_probe_at.clone(),
            ready,
            unready,
            stop_rx,
        ));
        *s.probe_stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        *s.probe_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Spawns the background [`Pool::refresh`] loop for a [`Backing::Pooled`] stub — the
    /// pooled mirror of [`Stub::spawn_probe`]. A no-op on a single stub.
    fn spawn_pool_refresh(&self) {
        let Backing::Pooled(p) = &self.backing else {
            return;
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = tokio::spawn(pool_refresh_loop(p.pool.clone(), stop_rx));
        *p.refresh_stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        *p.refresh_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// TEST-ONLY: the cached single-conn probe verdict (a [`Backing::Single`] stub).
    /// Lets the readyz-wiring and background-probe tests seed/read the verdict without
    /// exposing the backing enum's shape. Panics on a pooled stub (those tests use
    /// [`Pool::readyz`]).
    #[cfg(test)]
    pub(crate) fn single_verdict(&self) -> &Arc<StdMutex<Result<(), String>>> {
        match &self.backing {
            Backing::Single(s) => &s.verdict,
            Backing::Pooled(_) => panic!("single_verdict on a pooled stub"),
        }
    }

    /// TEST-ONLY: the single-conn probe timestamp (see [`Stub::single_verdict`]).
    #[cfg(test)]
    pub(crate) fn single_last_probe_at(&self) -> &Arc<AtomicU64> {
        match &self.backing {
            Backing::Single(s) => &s.last_probe_at,
            Backing::Pooled(_) => panic!("single_last_probe_at on a pooled stub"),
        }
    }

    /// Runs every [`RemoteBoot`] tagged with THIS stub's provider, each bounded by
    /// `boot_timeout`. Production always calls this via [`Module::start`] with
    /// [`BOOT_TIMEOUT`]; tests inject a short bound to prove the timeout branch
    /// without sleeping 10s. The bound is PER HOOK: a provider contributing N boot
    /// hooks bounds this stub's start at N x [`BOOT_TIMEOUT`] total (today the only
    /// registrant, configrpc, contributes exactly one).
    async fn start_with_boot_timeout(
        &self,
        ctx: &Context,
        boot_timeout: Duration,
    ) -> anyhow::Result<()> {
        for b in ctx.contributions::<RemoteBoot>(BOOT_SLOT) {
            if b.provider == self.provider {
                tokio::time::timeout(boot_timeout, (b.boot)())
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "remote boot for provider {:?} did not complete within {:?} — \
                             peer accepted the connection but is not answering; startup \
                             fails rather than hangs",
                            self.provider,
                            boot_timeout,
                        )
                    })?
                    .with_context(|| format!("remote boot for provider {:?}", self.provider))?;
                tracing::info!(provider = %self.provider, "remote stub boot hook ran");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Module for Stub {
    /// The PROVIDER name, so `validate_requires` treats the stub as the provider a
    /// co-hosted consumer requires.
    fn name(&self) -> &str {
        &self.provider
    }

    /// None — a peer's foundations live in the peer; the stub only bridges the sync
    /// capability over the edge.
    fn requires(&self) -> Vec<String> {
        Vec::new()
    }

    /// Phase 1, BEFORE any consumer's `init`: `provide` the edge-backed clients under
    /// the provider's capability keys, so a co-hosted dependent's `require` resolves to
    /// a real QUIC caller, AND contribute the provider's front-door `route_bindings()`
    /// so any stub-holding process can front the provider's `#[http]` ops remotely.
    ///
    /// For `"characters"` the capability clients are `characters.ownership` (inventory
    /// resolves it for `list_character`'s authz) and `characters.player` from the
    /// generated player client — both over the SAME reconnecting caller — plus the
    /// player route bindings. `"inventory"` is a LEAF (no peer `require`s an inventory
    /// capability), so it contributes route bindings ONLY: a dead capability provide
    /// would be noise, add one only when a consumer appears.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        // A stub with no factories provides nothing — a wiring bug (the composition
        // root forgot to pass the provider's `remote_factories()`). Fail loudly rather
        // than silently registering an inert module (this preserves the fail-loud
        // guarantee the old per-provider `match`'s unknown-provider arm gave).
        if self.factories.is_empty() {
            anyhow::bail!(
                "remote: Stub for provider {:?} was constructed with zero factories — \
                 pass `<name>rpc::remote_factories()` into `Stub::new`",
                self.provider
            );
        }
        // Hand each injected factory the capability caller AS an `opsapi::Caller`, so
        // the glue depends on the transport seam, never remote's concrete type. A single
        // stub hands its self-healing [`Reconnecting`] conn; a pooled stub hands its
        // round-robin [`Pool`] — the factory (and the consumer that `require`s the
        // capability) is unchanged and unaware which it got. Each factory `provide`s a
        // generated capability `Client` under the provider's capability key and/or
        // contributes the provider's front-door route bindings (Operation+OpBinding, no
        // LocalOp — `select_kind` resolves them Remote).
        let caller: Arc<dyn Caller> = match &self.backing {
            Backing::Single(s) => s.conn.clone(),
            Backing::Pooled(p) => p.pool.clone(),
        };
        for f in &self.factories {
            f(ctx, caller.clone());
        }
        tracing::info!(
            provider = %self.provider,
            factories = self.factories.len(),
            "remote stub registered — capability clients + front-door routes via injected factories"
        );
        Ok(())
    }

    /// The capability swap is entirely in `register`; the one wiring `init` does is
    /// contribute this provider's peer edge address to [`opsapi::PEER_SLOT`], so a
    /// co-hosted gateway front door can dial the provider Remote WITHOUT reading env
    /// itself — the module stays topology-blind, the composition root owns the address
    /// (via [`Stub::new`]). In a process with no gateway the contribution sits inert
    /// (unread) — harmless. The address stays an UNPARSED string: the gateway parses it
    /// lazily, preserving the Unavailable-not-panic taxonomy [`EdgeDialer`] relies on.
    ///
    /// The admin fan-out (Go's `Stub.adminFetcher`) is a `register`-time factory — a
    /// caller passes `adminrpc::admin_remote_factory(provider)` into [`Stub::new`],
    /// which contributes the REMOTE `adminapi::Item` there. `remote` stays `api/`-free:
    /// the admin closure arrives boxed, this crate never names `adminapi`.
    ///
    /// `init` ALSO contributes a per-stub `httpmw::ReadyCheck` (`stub:<provider>`) to
    /// [`httpmw::READINESS_SLOT`], so a stub-holding process's `/readyz` reflects its
    /// peers' reachability instead of answering 200 with the whole fleet dead. A stub is
    /// a HARD synchronous dependency — a process cannot serve the provider's ops with the
    /// peer down — so an unreachable peer flipping this process unready is the intended
    /// semantics, and it fans out fleet-wide (every stub-holder reports its own peers).
    ///
    /// The reachability I/O is NOT done here: a background probe loop owned by the stub
    /// (spawned in `start`, torn down in `stop` — see [`Stub::spawn_probe`]/[`probe_loop`])
    /// dials the peer on a two-rate cadence ([`PROBE_INTERVAL_UNREADY`] 1s while unready,
    /// [`PROBE_INTERVAL_READY`] 5s while ready) and stamps a CACHED verdict plus a
    /// `last_probe_at` coarse timestamp. The `/readyz` check here reads ONLY that cache
    /// (zero I/O), so probe cost is fully decoupled from request rate — a flood of unauth,
    /// rate-limit-exempt `/readyz` requests can no longer amplify into per-request QUIC/mTLS
    /// handshakes (a 6-stub front like gateway-svc used to pay up to 6 fresh dials PER
    /// request; it now pays none). The seed is fail-closed (`Err("probe pending")` until
    /// the first probe completes), and a stalled/dead probe loop (no completed probe within
    /// [`PROBE_STALL_MAX`]) flips the check unready rather than freezing a stale-green
    /// verdict. The monolith hosts no stubs and is unaffected.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        ctx.contribute(
            opsapi::PEER_SLOT,
            opsapi::PeerAddr {
                provider: self.provider.clone(),
                addrs: self.peer_addrs.clone(),
            },
        );
        // Zero-I/O readyz. A single stub reads the cached probe verdict (staleness-guarded
        // so a dead probe task can't freeze a stale-green verdict); a pooled stub delegates
        // to `Pool::readyz` (some-down-vs-all-down over the per-instance verdicts, also zero
        // I/O). Same slot, same `stub:<provider>` name in both — a stub-holding process's
        // `/readyz` reflects its peers either way.
        let name = format!("stub:{}", self.provider);
        match &self.backing {
            Backing::Single(s) => {
                let verdict = s.verdict.clone();
                let last_probe_at = s.last_probe_at.clone();
                ctx.contribute(
                    httpmw::READINESS_SLOT,
                    httpmw::ReadyCheck::new(name, move || {
                        let verdict = verdict.clone();
                        let last_probe_at = last_probe_at.clone();
                        async move {
                            let now = coarse_now_secs();
                            let stamp = last_probe_at.load(Ordering::SeqCst);
                            // Clone the cached verdict OUT and drop the std guard before
                            // computing the decision — the guard must never cross an
                            // `.await` (and this closure has none). The verdict is then a
                            // PURE function of the cached result + staleness clock.
                            let cached = verdict.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            readiness_verdict(&cached, stamp, now)
                        }
                    }),
                );
            }
            Backing::Pooled(p) => {
                let pool = p.pool.clone();
                ctx.contribute(
                    httpmw::READINESS_SLOT,
                    httpmw::ReadyCheck::new(name, move || {
                        let pool = pool.clone();
                        // `Pool::readyz` is a pure read of the cached per-instance verdicts
                        // (no dial), so this closure has no `.await` of its own.
                        async move { pool.readyz() }
                    }),
                );
            }
        }
        Ok(())
    }

    /// Runs every [`RemoteBoot`] tagged with THIS stub's provider (Step 5). A factory
    /// registers a boot hook in `register` for a start-time async action its pure swap
    /// cannot do — e.g. `configrpc`'s `CachedConfig` boot-fill (one `snapshot()`, fail
    /// loud if config-svc is down). Filtering by provider keeps a hook to its own
    /// provider's stub, so it runs exactly once even when a process holds several
    /// stubs. A boot error fails process startup loudly (config is a hard dependency).
    /// Each hook is bounded by [`BOOT_TIMEOUT`] — see its doc for why a live-but-slow
    /// peer now fails startup instead of hanging it (and every module start after it,
    /// since `App::start` awaits module starts sequentially and unbounded).
    async fn start(&self, ctx: &Context) -> anyhow::Result<()> {
        // Boot hooks first (ordered, and they fail-loud on a dead hard dependency)…
        self.start_with_boot_timeout(ctx, BOOT_TIMEOUT).await?;
        // …then arm the liveness machinery: a single stub's background reachability probe,
        // or a pooled stub's background refresh loop (which brings the pool's instance set +
        // per-instance probes up independently of request traffic).
        match &self.backing {
            Backing::Single(_) => self.spawn_probe(PROBE_INTERVAL_READY, PROBE_INTERVAL_UNREADY),
            Backing::Pooled(_) => self.spawn_pool_refresh(),
        }
        Ok(())
    }

    /// Tears down the background probe task (grace-then-abort, mirroring the scheduler)
    /// and closes the persistent edge connection (if one was ever dialed). Safe when the
    /// probe was never spawned (start-unwind before this stub started): the `Option::take`
    /// guards leave `None`.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        match &self.backing {
            Backing::Single(s) => {
                if let Some(tx) = s.probe_stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = tx.send(true);
                }
                // Take the handle out into a local so the std guard is dropped BEFORE the
                // await below (a `MutexGuard` is not `Send` and must never cross `.await`).
                let task = s.probe_task.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(mut task) = task {
                    match tokio::time::timeout(PROBE_STOP_GRACE, &mut task).await {
                        Ok(_) => {}
                        Err(_) => {
                            task.abort();
                            let _ = task.await; // await the abort so we don't leak the task
                        }
                    }
                }
                s.conn.close().await;
            }
            Backing::Pooled(p) => {
                if let Some(tx) = p.refresh_stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = tx.send(true);
                }
                let task = p.refresh_task.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(mut task) = task {
                    match tokio::time::timeout(PROBE_STOP_GRACE, &mut task).await {
                        Ok(_) => {}
                        Err(_) => {
                            task.abort();
                            let _ = task.await;
                        }
                    }
                }
                // Grace-then-abort the per-instance probes + close every instance conn.
                p.pool.stop().await;
            }
        }
        Ok(())
    }
}

// ===========================================================================
// Tests. The reconnecting caller's redial-once logic is exercised with a fake
// dial/conn seam (no QUIC); the injected-factory swap is proven with LOCAL fake
// factories (no `api/` crate — the core-leaf rule), asserting `register` runs every
// factory and that a zero-factory stub fails loudly. The REAL glue factories
// (`<name>rpc::remote_factories()`) are covered by their own crates + split-proof.
// ===========================================================================
#[cfg(test)]
mod tests;

// Step 1 (B1 repro): a real-edge redial test that boots an `edge::Server`, primes the
// crate-private `Reconnecting<EdgeDialer>` cache with a live call, tears the peer down,
// and re-boots it on the SAME port — then loops until the reconnecting caller recovers
// (or a bounded hang-guard fires with the full observed error sequence). Separate file
// per the tests-in-separate-files rule; same crate so it reaches the private seam.
#[cfg(test)]
#[path = "redial_tests.rs"]
mod redial_tests;

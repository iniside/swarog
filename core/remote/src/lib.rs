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
/// different value, thread it through [`Stub::new`] the way `peer_addr` already is,
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
/// The address is parsed lazily (at dial time) so a bad `*_EDGE_ADDR` surfaces as an
/// `Unavailable` error the consumer maps to 503, not a construction-time panic.
struct EdgeDialer {
    peer: String,
}

#[async_trait]
impl Dialer for EdgeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let addr: SocketAddr = self.peer.parse().map_err(|e| {
            Error::unavailable(format!("remote: bad peer edge addr {:?}: {e}", self.peer))
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
    peer_addr: String,
    verdict: Arc<StdMutex<Result<(), String>>>,
    last_probe_at: Arc<AtomicU64>,
    ready: Duration,
    unready: Duration,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        // Dial OUTSIDE any lock — the std guard must never cross an `.await`.
        let v = probe_peer(peer_addr.clone()).await;
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
    /// The peer's edge address as an UNPARSED string (the one [`EdgeDialer`] holds).
    /// Contributed to [`opsapi::PEER_SLOT`] in `init` so a co-hosted gateway front door
    /// dials this provider Remote without reading env — the topology this composition
    /// root injected via [`Stub::new`].
    peer_addr: String,
    /// The lazily-dialed, self-healing caller shared by every generated client below.
    conn: Arc<Reconnecting<EdgeDialer>>,
    /// The provider-swap closures this stub applies in `register`. Injected by the
    /// composition root from the provider's `<name>rpc::remote_factories()` — `remote`
    /// never names the provider itself.
    factories: Vec<RemoteFactory>,
    /// The cached peer-reachability verdict, stamped by [`probe_loop`] and READ (zero
    /// I/O) by the `/readyz` [`httpmw::ReadyCheck`]. Seeded fail-closed
    /// (`Err("probe pending")`) so an unknown-reachability cold start reports unready.
    verdict: Arc<StdMutex<Result<(), String>>>,
    /// Coarse seconds of the last COMPLETED probe (`0` = never). Backs the readyz
    /// staleness guard: a frozen verdict from a dead probe task flips unready.
    last_probe_at: Arc<AtomicU64>,
    /// Stop signal for the background probe task (`None` until `start`).
    probe_stop: StdMutex<Option<watch::Sender<bool>>>,
    /// The background probe task handle, torn down in `stop` (`None` until `start`).
    probe_task: StdMutex<Option<JoinHandle<()>>>,
}

impl Stub {
    /// Builds a stub for `provider`, dialing `peer_addr` (a numeric `host:port`, e.g.
    /// `127.0.0.1:9000`) lazily on first use, applying `factories` (the provider's
    /// `<name>rpc::remote_factories()`) at `register`. An EMPTY `factories` vec is a
    /// wiring bug — the stub would provide nothing — and fails loudly at `register`.
    pub fn new(provider: &str, peer_addr: &str, factories: Vec<RemoteFactory>) -> Stub {
        Stub {
            provider: provider.to_string(),
            peer_addr: peer_addr.to_string(),
            conn: Arc::new(Reconnecting::new(EdgeDialer {
                peer: peer_addr.to_string(),
            })),
            factories,
            // Fail-closed seed: unknown reachability = not ready until the first probe
            // completes.
            verdict: Arc::new(StdMutex::new(Err("probe pending".to_string()))),
            last_probe_at: Arc::new(AtomicU64::new(0)),
            probe_stop: StdMutex::new(None),
            probe_task: StdMutex::new(None),
        }
    }

    /// Spawns the background reachability probe loop (idempotent-callee contract: call
    /// exactly once, from `start`). Splits the spawn out of `start` so tests can drive
    /// it with short intervals via [`Stub::spawn_probe`] without threading them through
    /// [`Stub::new`]. Production calls it with the const cadence; the loop is the sole
    /// runtime caller of [`probe_peer`].
    pub(crate) fn spawn_probe(&self, ready: Duration, unready: Duration) {
        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = tokio::spawn(probe_loop(
            self.peer_addr.clone(),
            self.verdict.clone(),
            self.last_probe_at.clone(),
            ready,
            unready,
            stop_rx,
        ));
        *self.probe_stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        *self.probe_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
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
        // Hand each injected factory the reconnecting conn AS an `opsapi::Caller`, so
        // the glue depends on the transport seam, never remote's concrete type. Each
        // factory `provide`s a generated capability `Client` under the provider's
        // capability key and/or contributes the provider's front-door route bindings
        // (Operation+OpBinding, no LocalOp — `select_kind` resolves them Remote).
        let caller: Arc<dyn Caller> = self.conn.clone();
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
                addr: self.peer_addr.clone(),
            },
        );
        // Zero-I/O readyz: read the cached verdict stamped by the background probe loop,
        // with a staleness guard so a dead probe task can't freeze a stale-green verdict.
        let verdict = self.verdict.clone();
        let last_probe_at = self.last_probe_at.clone();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new(format!("stub:{}", self.provider), move || {
                let verdict = verdict.clone();
                let last_probe_at = last_probe_at.clone();
                async move {
                    let now = coarse_now_secs();
                    let stamp = last_probe_at.load(Ordering::SeqCst);
                    // Clone the cached verdict OUT and drop the std guard before computing
                    // the decision — the guard must never cross an `.await` (and this
                    // closure has none). The verdict is then a PURE function of the cached
                    // result + the staleness clock (see `readiness_verdict`).
                    let cached = verdict.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    readiness_verdict(&cached, stamp, now)
                }
            }),
        );
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
        // …then arm the background reachability probe that feeds the cached readyz verdict.
        self.spawn_probe(PROBE_INTERVAL_READY, PROBE_INTERVAL_UNREADY);
        Ok(())
    }

    /// Tears down the background probe task (grace-then-abort, mirroring the scheduler)
    /// and closes the persistent edge connection (if one was ever dialed). Safe when the
    /// probe was never spawned (start-unwind before this stub started): the `Option::take`
    /// guards leave `None`.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self
            .probe_stop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(true);
        }
        // Take the handle out into a local so the std guard is dropped BEFORE the await
        // below (a `MutexGuard` is not `Send` and must never cross an `.await`).
        let task = self
            .probe_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut task) = task {
            match tokio::time::timeout(PROBE_STOP_GRACE, &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    task.abort();
                    let _ = task.await; // await the abort so we don't leak the task
                }
            }
        }
        self.conn.close().await;
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

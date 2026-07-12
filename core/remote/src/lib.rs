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
//! on a *transport* error always drops the connection (a definitive peer answer —
//! `opsapi::Status::is_definitive_answer` — keeps it), but replays only methods
//! explicitly marked retry-safe; mutations return the first error and the next
//! request redials.
//! A dial failure — the peer is down — propagates to
//! the consumer, which maps it to a 503. The retry logic is generic over a private
//! [`Dialer`]/[`Conn`] seam so it is unit-testable with a fake transport (no QUIC).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use opsapi::{Caller, Error, RetryMode};
use tokio::sync::Mutex;

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
pub const BOOT_SLOT: &str = "remote.boot";

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
#[async_trait]
trait Conn: Send + Sync {
    async fn call(&self, method: &str, identity: Option<&str>, payload: &[u8]) -> Result<Vec<u8>, Error>;
    fn close(&self);
}

/// Dials a fresh [`Conn`] to the peer. Called lazily by [`Reconnecting`] on first use
/// and again after a reset.
#[async_trait]
trait Dialer: Send + Sync {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error>;
}

/// A lazily-dialed, self-healing [`Caller`] over a [`Dialer`]. Holds at most one live
/// connection; on a *transport* error it drops that connection and follows the call's
/// fail-closed [`RetryMode`] (a definitive peer answer keeps the connection — see
/// [`Caller::call`] below). Generic over `D` purely so tests can inject a fake dialer.
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
    /// One RPC. A *transport* failure always invalidates the cached connection; a
    /// DEFINITIVE peer answer ([`opsapi::Status::is_definitive_answer`], e.g. the
    /// typed unknown-method → `NotFound`) proves the connection is healthy and is
    /// returned as-is — no reset, no redial, regardless of `retry_mode`. Only an
    /// explicit [`RetryMode::OnceAfterReconnect`] redials and replays once; the
    /// default [`RetryMode::Never`] returns the first error without replaying a
    /// mutation. The SAME definitive-answer guard applies to the replay on the
    /// fresh connection: if it too answers definitively, that connection is proven
    /// healthy and is kept cached, unreset. Only a non-definitive replay failure
    /// resets the fresh connection too — a dead c2 must not stay cached.
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
            // A definitive peer answer: the peer received and answered the request,
            // so the connection is healthy — keep it cached. Reset stays the DEFAULT
            // for everything else (Unavailable, Internal, any future status).
            Err(first) if first.status.is_definitive_answer() => Err(first),
            Err(first) => {
                self.reset(&c).await;
                if retry_mode == RetryMode::Never {
                    return Err(first);
                }
                let c2 = self.get().await?;
                match c2.call(method, identity, payload).await {
                    Ok(v) => Ok(v),
                    // Mirrors the first-attempt guard above: a definitive answer on
                    // the replay proves c2 is healthy too — keep it cached.
                    Err(second) if second.status.is_definitive_answer() => Err(second),
                    Err(second) => {
                        // The replayed connection failed (non-definitively) too —
                        // invalidate it so the NEXT request redials instead of
                        // reusing a dead c2.
                        self.reset(&c2).await;
                        Err(second)
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

#[async_trait]
impl Conn for edge::Client {
    async fn call(&self, method: &str, identity: Option<&str>, payload: &[u8]) -> Result<Vec<u8>, Error> {
        // The edge client stamps `identity` into the request envelope; a transport
        // failure (an `edge::Error`) becomes an `opsapi::Error::Unavailable`. A domain
        // status rides INSIDE the payload the generated `Client` decodes — not here.
        self.call_raw_id(method, identity, payload).await.map_err(Error::from)
    }

    fn close(&self) {
        edge::Client::close(self);
    }
}

// ---------------------------------------------------------------------------
// The per-stub readiness probe (the `/readyz` contribution).
// ---------------------------------------------------------------------------

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
        }
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
    /// The deliberate cost: each probe is a FRESH QUIC/mTLS dial (see [`probe_peer`]),
    /// run per stub, sequentially by `/readyz`; a dead peer therefore costs ~1s per stub
    /// per probe (a 6-stub front like gateway-svc pays up to ~6s on a fully-dead fleet) —
    /// acceptable at this local/dev deploy scale. The check reads lazily per request, so
    /// contributing it in `init` (no I/O) is correct. The monolith hosts no stubs and is
    /// unaffected.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        ctx.contribute(
            opsapi::PEER_SLOT,
            opsapi::PeerAddr {
                provider: self.provider.clone(),
                addr: self.peer_addr.clone(),
            },
        );
        // Fresh dial per probe; the probe body owns its 1s inner bound (see `probe_peer`).
        let peer_addr = self.peer_addr.clone();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new(format!("stub:{}", self.provider), move || {
                probe_peer(peer_addr.clone())
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
    async fn start(&self, ctx: &Context) -> anyhow::Result<()> {
        for b in ctx.contributions::<RemoteBoot>(BOOT_SLOT) {
            if b.provider == self.provider {
                (b.boot)()
                    .await
                    .with_context(|| format!("remote boot for provider {:?}", self.provider))?;
                tracing::info!(provider = %self.provider, "remote stub boot hook ran");
            }
        }
        Ok(())
    }

    /// Closes the persistent edge connection (if one was ever dialed).
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
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

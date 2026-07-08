//! `remote` — the registry SWAP that makes the split topology-blind (port of Go's
//! `modules/remote`). When a process hosts a consumer whose provider lives in ANOTHER
//! process, `main` adds a [`Stub`] for that provider. In phase-1 `register` the stub
//! `provide`s a generated edge CLIENT under the SAME capability key(s) the local impl
//! would, so the consumer's `require::<dyn Trait>(key)` resolves to a real QUIC caller
//! across the process boundary — the consumer code is unchanged, unaware which it got.
//!
//! It imports only the foundations + `edge` + each provider's `<name>rpc` GLUE crate
//! (whose `remote_factories()` owns the swap actions — capability `Client` provides +
//! front-door route bindings). It NEVER imports the provider's impl crate (CLAUDE.md #2):
//! the generated `Client` implements the capability trait over an [`opsapi::Caller`],
//! and the wire shape + method names are OWNED by that generated glue, so wire drift
//! between the two sides is impossible.
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
//! on a call error drops the connection and retries EXACTLY once with a fresh dial
//! (the port of Go's `edgeConn`). A dial failure — the peer is down — propagates to
//! the consumer, which maps it to a 503. The retry logic is generic over a private
//! [`Dialer`]/[`Conn`] seam so it is unit-testable with a fake transport (no QUIC).

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use lifecycle::{Caps, Context, Module};
use opsapi::{Caller, Error};
use tokio::sync::Mutex;

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
/// connection; on a call error it drops that connection and retries once with a fresh
/// dial. Generic over `D` purely so the tests can inject a fake dialer.
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
    /// One RPC with a single reconnect-and-retry on failure. The first error may be a
    /// stale/dead connection (the peer restarted); we drop it, re-dial, and retry
    /// once. If the re-dial fails or the retry also errors, the error propagates so
    /// the consumer answers 503.
    async fn call(&self, method: &str, identity: Option<&str>, payload: &[u8]) -> Result<Vec<u8>, Error> {
        let c = self.get().await?;
        match c.call(method, identity, payload).await {
            Ok(v) => Ok(v),
            Err(_first) => {
                // Possible transport failure — reconnect once and retry.
                self.reset(&c).await;
                let c2 = self.get().await?;
                c2.call(method, identity, payload).await
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
// The Stub module — the swap.
// ---------------------------------------------------------------------------

/// Stands in for a provider hosted in a PEER process. Its [`Module::name`] reports the
/// PROVIDER name (`"characters"`) so `app::validate_requires` sees a co-hosted
/// consumer's requirement satisfied; its phase-1 `register` `provide`s edge-backed
/// clients under the SAME capability keys the local impl would. It migrates no schema
/// and mounts no routes; as a [`Module`] with [`Caps::STOP`] it closes the underlying
/// edge connection on shutdown.
pub struct Stub {
    /// The provider name — also the [`Module::name`], so `validate_requires` matches.
    provider: String,
    /// The lazily-dialed, self-healing caller shared by every generated client below.
    conn: Arc<Reconnecting<EdgeDialer>>,
}

impl Stub {
    /// Builds a stub for `name`, dialing `peer_addr` (a numeric `host:port`, e.g.
    /// `127.0.0.1:9000`) lazily on first use. Only `"characters"` is edge-exposed in
    /// M1; any other name fails loudly at `register` rather than providing a dead
    /// client.
    pub fn new(name: &str, peer_addr: &str) -> Stub {
        Stub {
            provider: name.to_string(),
            conn: Arc::new(Reconnecting::new(EdgeDialer {
                peer: peer_addr.to_string(),
            })),
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

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::STOP
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
        // Hand each generated client the reconnecting conn AS an `opsapi::Caller`, so
        // the glue depends on the transport seam, never remote's concrete type.
        //
        // Each glue crate owns its provider-swap actions (`remote_factories()`): for
        // "characters" they provide the ownership + player edge clients under the
        // provider's capability keys AND contribute the player ops' route bindings
        // (Operation+OpBinding, no LocalOp — `select_kind` resolves them Remote); for
        // "inventory" (a leaf — nothing `require`s an inventory capability) route
        // bindings ONLY. Step 4 moves this match into `cmd/*`, which will pass the
        // factories straight to a generic `Stub::new`.
        let caller: Arc<dyn Caller> = self.conn.clone();
        let factories = match self.provider.as_str() {
            "characters" => charactersrpc::remote_factories(),
            "inventory" => inventoryrpc::remote_factories(),
            other => anyhow::bail!("remote: no edge client for provider {other:?}"),
        };
        for f in &factories {
            f(ctx, caller.clone());
        }
        tracing::info!(
            provider = %self.provider,
            "remote stub registered — capability clients + front-door routes via the provider's rpc glue"
        );
        Ok(())
    }

    /// Nothing to wire in M1: the swap is entirely in `register`. (Go's stub also
    /// contributes a remote admin item; the admin edge fan-out is Milestone 2.)
    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
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
// dial/conn seam (no QUIC); the stub's swap is proven by asserting `register`
// provides the right capability keys with the right trait types.
// ===========================================================================
#[cfg(test)]
mod tests;

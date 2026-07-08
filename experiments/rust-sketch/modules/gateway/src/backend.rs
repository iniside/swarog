//! The topology-swappable invoker at the heart of the gateway (port of Go's
//! `modules/gateway/backend.go`). Given an [`Operation`], the caller [`Identity`],
//! and the already-decoded WIRE REQUEST bytes, an [`OperationBackend`] produces the
//! WIRE RESPONSE bytes. It abstracts ONLY the invoke — the gateway decodes the HTTP
//! body into the wire request and encodes the wire response back to HTTP around this
//! call, so the SAME decode/encode path drives both topologies and only the hop in
//! the middle differs (D3 marshal-count honesty: Local = +0, Remote = +1 over the
//! edge).
//!
//! Two impls, selected per operation by whether the provider is in THIS process
//! (see `select_kind` in `lib.rs`):
//!   - [`LocalBackend`] — a direct in-process call to the generated [`LocalInvoker`]
//!     contributed to `opsapi::LOCAL_SLOT`. Zero extra marshal.
//!   - [`RemoteBackend`] — relays the wire request over the QUIC edge to the owning
//!     peer via an [`opsapi::Caller`] (an `edge::Client`), stamping the verified
//!     identity into the envelope so the peer never re-verifies.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use opsapi::{Caller, Error, Identity, LocalInvoker, Operation};

/// The invoke seam: decoded wire request bytes in, wire response bytes out. The
/// gateway owns the HTTP decode/encode on either side of this call, so a
/// `LocalBackend` and a `RemoteBackend` are interchangeable by construction.
#[async_trait]
pub trait OperationBackend: Send + Sync {
    async fn invoke(
        &self,
        op: &Operation,
        identity: Identity,
        req: Vec<u8>,
    ) -> Result<Vec<u8>, Error>;
}

/// Dispatches an operation to its provider IN-PROCESS. Holds the method-name →
/// [`LocalInvoker`] map the gateway builds from `ctx.contributions(opsapi::LOCAL_SLOT)`.
/// Behind an `Arc` so a per-request backend is a cheap handle clone, never a map copy.
pub struct LocalBackend {
    invokers: Arc<HashMap<String, LocalInvoker>>,
}

impl LocalBackend {
    pub fn new(invokers: Arc<HashMap<String, LocalInvoker>>) -> Self {
        LocalBackend { invokers }
    }
}

#[async_trait]
impl OperationBackend for LocalBackend {
    async fn invoke(
        &self,
        op: &Operation,
        identity: Identity,
        req: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        // A missing invoker is a wiring bug (a route was bound with no provider
        // registered in this process) surfaced as an error rather than a silent nil.
        let inv = self.invokers.get(&op.method).ok_or_else(|| {
            Error::internal(format!("gateway: no local invoker for operation {:?}", op.method))
        })?;
        inv(identity, req).await
    }
}

/// Dispatches an operation to a provider hosted in a PEER process over the QUIC edge.
/// Generic over any [`opsapi::Caller`] — production wires an `edge::Client`; a test
/// wires a fake caller (or a loopback edge server). The verified player_id rides the
/// envelope's identity field so the peer's generated adapter reads it there instead
/// of re-verifying — the auth-once boundary extends across the wire.
pub struct RemoteBackend {
    caller: Arc<dyn Caller>,
}

impl RemoteBackend {
    pub fn new(caller: Arc<dyn Caller>) -> Self {
        RemoteBackend { caller }
    }
}

#[async_trait]
impl OperationBackend for RemoteBackend {
    async fn invoke(
        &self,
        op: &Operation,
        identity: Identity,
        req: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        // `Caller::call` maps every edge-transport failure to Status::Unavailable; a
        // completed op's DOMAIN status (404/403/…) rides inside the returned bytes and
        // is decoded by the gateway's `encode`, exactly as on the Local path.
        self.caller.call(&op.method, identity.player_id(), &req).await
    }
}

//! The QUIC RPC server (port of Go's `edge/server.go`). Accepts connections, then
//! bidirectional streams, and dispatches one framed request per stream to a handler
//! registered by method name. It knows nothing about the application domain — pure
//! transport.
//!
//! Dispatch precedence (exactly Go's): an exact [`Server::handle`] wins over an
//! exact [`Server::handle_identity`], which wins over the longest-matching
//! [`Server::handle_prefix`]; an unmatched method is an "unknown method" error.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use quinn::crypto::rustls::QuicServerConfig;
use serde_json::value::RawValue;
use tokio::sync::{watch, Notify};

use crate::frame::{read_frame, write_frame};
use crate::tls::DevCA;
use crate::wire::{Request, Response};
use crate::Error;

/// Steady-state bound on the two PEER-controlled waits in [`serve_stream`]: the
/// request read (peer opened a bidi stream but never sent a full frame) and the
/// response delivery (peer never grants flow-control credit / never acknowledges).
/// Both hold an [`InFlightGuard`] and a stream slot, and the internal client's 5s
/// keepalive ([`crate::client`]) resets the connection idle timeout — so without
/// this bound an application-level hang with a live transport leaks the stream task
/// forever. Distinct from `EDGE_DRAIN_GRACE_MS` (shutdown-only); this one runs in
/// steady state. The handler dispatch between the two waits is deliberately
/// UNBOUNDED — a domain call may legitimately be long.
const EDGE_STREAM_GRACE: Duration = Duration::from_secs(30);

/// Explicit idle timeout for internal-edge connections. This mirrors quinn's
/// current default (30s) — it is pinned here for auditability against a future
/// quinn default change, not to rescue anything. MUST stay comfortably above the
/// internal client's [`crate::client::KEEPALIVE_INTERVAL`] (5s) so the keepalive
/// keeps a quiet-but-live connection open.
const EDGE_IDLE_TIMEOUT_MS: u32 = 30_000;

/// Concurrent bidi streams one internal-edge peer may hold open, mirroring the
/// player plane's cap (quinn's default is 100 — the internal stream-per-call
/// pattern never needs that many at once per connection).
const MAX_EDGE_BIDI_STREAMS: u32 = 16;

/// The result a handler returns: response payload bytes, or any error (whose
/// `Display` becomes the wire error string — edge carries only a bare string, so
/// the operation `Status` rides INSIDE the payload envelope the `#[rpc]` layer emits).
pub type HandlerResult = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>;

/// A transport-agnostic RPC handler: raw request payload in, raw response payload
/// out. Async because the domain impls it fronts are async.
pub type Handler = Arc<dyn Fn(Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync>;

/// Like [`Handler`] but also receives the caller identity the sender stamped into
/// the request envelope (`None` when none was set). The generated RPC server
/// adapters (Step 5) register through this so a remote operation's impl can see the
/// gateway-verified player_id — edge forwards the raw string; the adapter turns it
/// into an [`opsapi::Identity`].
pub type IdentityHandler =
    Arc<dyn Fn(Option<String>, Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync>;

/// Like [`Handler`] but also receives the method name, so one prefix registration
/// serves a whole family of methods under their original names — the natural shape
/// for a gateway that byte-relays to a backend.
pub type ForwardHandler =
    Arc<dyn Fn(String, Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync>;

/// The registration builder. Register all handlers, then [`Server::listen`].
pub struct Server {
    handlers: HashMap<String, Handler>,
    id_handlers: HashMap<String, IdentityHandler>,
    prefixes: Vec<(String, ForwardHandler)>,
    stream_grace: Duration,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            handlers: HashMap::new(),
            id_handlers: HashMap::new(),
            prefixes: Vec::new(),
            stream_grace: EDGE_STREAM_GRACE,
        }
    }
}

impl Server {
    pub fn new() -> Self {
        Server::default()
    }

    /// Test seam: shrink the per-stream grace so a reap test does not sleep 30s.
    /// Production always runs [`EDGE_STREAM_GRACE`].
    #[cfg(test)]
    pub(crate) fn set_stream_grace(&mut self, grace: Duration) {
        self.stream_grace = grace;
    }

    /// Registers a [`Handler`] under a method name.
    pub fn handle(&mut self, method: impl Into<String>, h: Handler) {
        self.handlers.insert(method.into(), h);
    }

    /// Registers an [`IdentityHandler`] under a method name — like [`Server::handle`],
    /// but the handler also receives the request envelope's identity string.
    pub fn handle_identity(&mut self, method: impl Into<String>, h: IdentityHandler) {
        self.id_handlers.insert(method.into(), h);
    }

    /// Registers a [`ForwardHandler`] for every method whose name starts with
    /// `prefix`. An exact registration always wins over any prefix; among competing
    /// prefixes the longest match wins.
    pub fn handle_prefix(&mut self, prefix: impl Into<String>, fwd: ForwardHandler) {
        self.prefixes.push((prefix.into(), fwd));
    }

    /// Binds a QUIC listener on `addr` (e.g. `127.0.0.1:0` for an ephemeral port),
    /// builds the mutual-TLS server config from `ca`, and starts the accept loop in
    /// the background. Returns once the socket is bound; [`RunningServer::local_addr`]
    /// is valid immediately.
    pub fn listen(self, addr: SocketAddr, ca: &DevCA) -> Result<RunningServer, Error> {
        let server_cfg = ca.server_tls()?;
        let qsc = QuicServerConfig::try_from(server_cfg)
            .map_err(|e| Error::Tls(format!("quic server config: {e}")))?;
        let mut quinn_cfg = quinn::ServerConfig::with_crypto(Arc::new(qsc));

        // Explicit TransportConfig (same template as the player plane): the idle
        // timeout pins quinn's current default so a future default change cannot
        // silently unbound the plane, and the stream cap tightens 100 → 16. The
        // internal peer is mTLS-authenticated, so this is auditability, not defense.
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(MAX_EDGE_BIDI_STREAMS));
        transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            EDGE_IDLE_TIMEOUT_MS,
        ))));
        quinn_cfg.transport_config(Arc::new(transport));

        let endpoint = quinn::Endpoint::server(quinn_cfg, addr).map_err(Error::Io)?;
        let local_addr = endpoint.local_addr().map_err(Error::Io)?;

        let dispatch = Arc::new(Dispatch {
            handlers: self.handlers,
            id_handlers: self.id_handlers,
            prefixes: self.prefixes,
        });
        let stream_grace = self.stream_grace;

        let shutdown = ShutdownState::new();
        let accept_endpoint = endpoint.clone();
        let accept_state = shutdown.clone();
        tokio::spawn(async move {
            let mut closing = accept_state.subscribe();
            loop {
                tokio::select! {
                    incoming = accept_endpoint.accept() => {
                        // `Endpoint::accept()` is cancel-safe: the incoming queue
                        // lives in the endpoint, so losing a select race drops nothing.
                        let Some(incoming) = incoming else { break };
                        let dispatch = dispatch.clone();
                        let conn_state = accept_state.clone();
                        // The guard is created HERE (the accept arm) and moved into the
                        // task, so an accepted-but-unstarted connection is never
                        // invisible to the drain.
                        let guard = accept_state.enter();
                        tokio::spawn(async move {
                            let _guard = guard;
                            match incoming.await {
                                Ok(conn) => serve_conn(conn, dispatch, conn_state, stream_grace).await,
                                // Handshake failure (e.g. an un-certed client rejected by the
                                // WebPkiClientVerifier) — nothing to serve.
                                Err(e) => tracing::debug!(error = %e, "edge: connection handshake failed"),
                            }
                        });
                    }
                    // Graceful shutdown: stop admitting NEW connections.
                    _ = closing.wait_for(|c| *c) => break,
                }
            }
        });

        Ok(RunningServer { endpoint, local_addr, shutdown })
    }
}

/// Shared drain state for one running QUIC plane — the internal [`Server`] and the
/// player-facing [`crate::PlayerServer`] each build one and thread it through their
/// accept loops. `closing` is a `tokio::sync::watch` channel, NOT a `Notify`:
/// `notify_waiters()` stores no permit, so a loop that is not currently parked would
/// miss the signal, while a watch receiver observes the flipped value whenever it
/// polls. `in_flight` counts RAII [`InFlightGuard`]s — one per
/// accepted-but-not-yet-handshaken connection and one per accepted stream, each
/// created at its ACCEPT site and moved into the spawned task.
pub(crate) struct ShutdownState {
    closing: watch::Sender<bool>,
    in_flight: AtomicUsize,
    idle_notify: Notify,
}

impl ShutdownState {
    pub(crate) fn new() -> Arc<ShutdownState> {
        Arc::new(ShutdownState {
            closing: watch::Sender::new(false),
            in_flight: AtomicUsize::new(0),
            idle_notify: Notify::new(),
        })
    }

    /// A receiver for the closing flag. Use `rx.wait_for(|c| *c)` in a `select!` arm:
    /// unlike `changed()`, it resolves immediately when shutdown already began.
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.closing.subscribe()
    }

    /// Registers one unit of in-flight work. Create the guard at the accept site and
    /// MOVE it into the task performing the work — creating it inside the task body
    /// leaves a window where accepted-but-unstarted work is invisible to the drain
    /// and gets aborted by `endpoint.close()`.
    pub(crate) fn enter(self: &Arc<Self>) -> InFlightGuard {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        InFlightGuard(self.clone())
    }

    fn begin_closing(&self) {
        self.closing.send_replace(true);
    }

    fn in_flight_count(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Resolves once `in_flight` reaches 0. Subscribe-THEN-check: the `Notify` future
    /// is registered BEFORE the counter is read, closing the race where the last
    /// guard decrements (and notifies) between our check and our await. When already
    /// idle, the first check returns without ever awaiting — the short-circuit an
    /// idle teardown relies on.
    async fn idle(&self) {
        loop {
            let notified = self.idle_notify.notified();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// RAII in-flight marker; dropping the last one wakes [`ShutdownState::idle`] waiters.
pub(crate) struct InFlightGuard(Arc<ShutdownState>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle_notify.notify_waiters();
        }
    }
}

/// A running edge server (either plane — the internal mTLS [`Server`] and the
/// player-facing [`crate::PlayerServer`] both return one). Stop it gracefully with
/// [`RunningServer::shutdown`] (drains in-flight work) or hard with
/// [`RunningServer::close`] / drop (aborts everything immediately).
pub struct RunningServer {
    pub(crate) endpoint: quinn::Endpoint,
    pub(crate) local_addr: SocketAddr,
    pub(crate) shutdown: Arc<ShutdownState>,
}

impl RunningServer {
    /// The listener's bound address (valid immediately after [`Server::listen`]).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Hard stop: aborts the accept loop and every live connection immediately,
    /// in-flight work included. The graceful superset is [`RunningServer::shutdown`];
    /// this stays as the abort path (and the tests' quick teardown).
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"server shutting down");
    }

    /// Graceful shutdown: (1) flip the closing flag — the endpoint-accept loop and
    /// every per-connection stream-accept loop stop admitting new work; (2) wait up
    /// to `grace` for in-flight work (handshakes + stream handlers) to drain —
    /// already-idle returns immediately, a timeout warns with the straggler count;
    /// (3) close the endpoint (aborting any stragglers) and wait — bounded by
    /// `min(grace, 3s)` so a dead peer cannot hang teardown — for the close
    /// notifications to flush.
    pub async fn shutdown(&self, grace: Duration) {
        self.shutdown.begin_closing();
        if tokio::time::timeout(grace, self.shutdown.idle()).await.is_err() {
            tracing::warn!(
                in_flight = self.shutdown.in_flight_count(),
                grace_ms = grace.as_millis() as u64,
                "edge: drain grace expired; aborting in-flight work"
            );
        }
        self.endpoint.close(0u32.into(), b"server shutting down");
        let bound = grace.min(Duration::from_secs(3));
        let _ = tokio::time::timeout(bound, self.endpoint.wait_idle()).await;
    }
}

/// The frozen dispatch table shared (via `Arc`) into every accept/serve task.
struct Dispatch {
    handlers: HashMap<String, Handler>,
    id_handlers: HashMap<String, IdentityHandler>,
    prefixes: Vec<(String, ForwardHandler)>,
}

impl Dispatch {
    /// Decodes the request envelope, invokes the handler by precedence, and builds
    /// the response envelope. Unknown methods and handler errors/panics become
    /// `ok:false`.
    async fn dispatch(&self, req_bytes: Vec<u8>) -> Response {
        let req: Request = match serde_json::from_slice(&req_bytes) {
            Ok(r) => r,
            Err(_) => return err_response("edge: malformed request envelope"),
        };
        let payload = req.payload.get().as_bytes().to_vec();

        // Exact `handle` wins; then exact `handle_identity`; then longest prefix.
        let result: HandlerResult = if let Some(h) = self.handlers.get(&req.method) {
            run_caught(h(payload)).await
        } else if let Some(h) = self.id_handlers.get(&req.method) {
            run_caught(h(req.identity.clone(), payload)).await
        } else if let Some(fwd) = self.longest_prefix(&req.method) {
            run_caught(fwd(req.method.clone(), payload)).await
        } else {
            // The shared sentinel (`crate::UNKNOWN_METHOD_PREFIX`) — the internal
            // Client detects this prefix and types it as `Error::UnknownMethod`.
            return err_response(&format!("{} {:?}", crate::UNKNOWN_METHOD_PREFIX, req.method));
        };

        match result {
            Ok(bytes) => ok_response(bytes),
            Err(e) => err_response(&e.to_string()),
        }
    }

    /// The [`ForwardHandler`] whose prefix is the longest one `method` starts with.
    fn longest_prefix(&self, method: &str) -> Option<&ForwardHandler> {
        let mut best: Option<&ForwardHandler> = None;
        let mut best_len: isize = -1;
        for (prefix, fwd) in &self.prefixes {
            if method.starts_with(prefix) && prefix.len() as isize > best_len {
                best = Some(fwd);
                best_len = prefix.len() as isize;
            }
        }
        best
    }
}

/// Accepts streams on a single connection, one task per stream. Each accepted
/// stream's in-flight guard is created HERE (where `accept_bi()` yields) and moved
/// into the stream task. On graceful shutdown the loop stops accepting NEW streams
/// and returns WITHOUT closing the connection — in-flight stream tasks hold their
/// own guards (and their streams keep the quinn connection refcount alive), so they
/// finish under the drain.
async fn serve_conn(
    conn: quinn::Connection,
    dispatch: Arc<Dispatch>,
    state: Arc<ShutdownState>,
    stream_grace: Duration,
) {
    let mut closing = state.subscribe();
    loop {
        tokio::select! {
            res = conn.accept_bi() => match res {
                Ok((send, recv)) => {
                    let dispatch = dispatch.clone();
                    let guard = state.enter();
                    tokio::spawn(async move {
                        let _guard = guard;
                        serve_stream(send, recv, dispatch, stream_grace).await;
                    });
                }
                // Peer closed, idle timeout, or shutdown.
                Err(_) => return,
            },
            _ = closing.wait_for(|c| *c) => return,
        }
    }
}

/// Reads one framed request, dispatches it, and writes one framed response.
///
/// Both PEER-controlled waits are bounded by `grace` ([`EDGE_STREAM_GRACE`] in
/// production): the request read and the response delivery. A peer whose 5s
/// keepalive keeps the connection alive but that never completes a frame (or never
/// drains the reply) would otherwise pin this task — and its [`InFlightGuard`] +
/// stream slot — forever. The handler dispatch in the middle stays unbounded.
async fn serve_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    dispatch: Arc<Dispatch>,
    grace: Duration,
) {
    let req_bytes = match tokio::time::timeout(grace, read_frame(&mut recv)).await {
        Ok(Ok(b)) => b,
        // Malformed / truncated request: nothing to reply to reliably.
        Ok(Err(_)) => return,
        // Peer opened the stream but never sent a full frame within the grace:
        // drop the stream (returning resets both halves) and free the guard.
        Err(_) => {
            tracing::debug!("edge: stream request not received within grace; dropping stream");
            return;
        }
    };

    let resp = dispatch.dispatch(req_bytes).await;
    let resp_bytes = serde_json::to_vec(&resp)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"edge: response encode failed"}"#.to_vec());
    // Hold the stream task (and thus its in-flight guard) open until the peer
    // acknowledges receipt of the response, or the stream/connection dies. `finish`
    // only queues the data — without the `stopped()` wait, a graceful shutdown could
    // observe in-flight == 0 and reach `endpoint.close()` while the reply is still
    // buffered, aborting its delivery. The WHOLE output half is bounded by `grace`:
    // `write_frame` can stall on withheld flow-control credit and `stopped()` on a
    // never-acknowledging peer — the same keepalive-pinned pathology as the read.
    let deliver = async {
        let _ = write_frame(&mut send, &resp_bytes).await;
        let _ = send.finish();
        let _ = send.stopped().await;
    };
    if tokio::time::timeout(grace, deliver).await.is_err() {
        tracing::debug!("edge: peer did not drain response within grace; dropping stream");
    }
}

/// Runs a handler future, containing a panic (or a panicking codec) into an error
/// result so one bad call cannot take down the stream task silently (Go's `recover`).
/// Shared with the player plane (`player.rs`).
pub(crate) async fn run_caught(fut: BoxFuture<'static, HandlerResult>) -> HandlerResult {
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(r) => r,
        Err(p) => Err(format!("edge: handler panic: {}", panic_message(&p)).into()),
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

pub(crate) fn ok_response(bytes: Vec<u8>) -> Response {
    // An empty handler response (a no-return op) omits the payload; otherwise the
    // bytes are the domain response envelope, preserved verbatim as raw JSON.
    let payload = if bytes.is_empty() {
        None
    } else {
        match RawValue::from_string(String::from_utf8_lossy(&bytes).into_owned()) {
            Ok(raw) => Some(raw),
            Err(_) => return err_response("edge: handler produced non-JSON response"),
        }
    };
    Response { ok: true, payload, error: None }
}

pub(crate) fn err_response(msg: &str) -> Response {
    Response { ok: false, payload: None, error: Some(msg.to_string()) }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

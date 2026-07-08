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
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use quinn::crypto::rustls::QuicServerConfig;
use serde_json::value::RawValue;

use crate::frame::{read_frame, write_frame};
use crate::tls::DevCA;
use crate::wire::{Request, Response};
use crate::Error;

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
#[derive(Default)]
pub struct Server {
    handlers: HashMap<String, Handler>,
    id_handlers: HashMap<String, IdentityHandler>,
    prefixes: Vec<(String, ForwardHandler)>,
}

impl Server {
    pub fn new() -> Self {
        Server::default()
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
        let endpoint = quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(qsc)), addr)
            .map_err(Error::Io)?;
        let local_addr = endpoint.local_addr().map_err(Error::Io)?;

        let dispatch = Arc::new(Dispatch {
            handlers: self.handlers,
            id_handlers: self.id_handlers,
            prefixes: self.prefixes,
        });

        let accept_endpoint = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let dispatch = dispatch.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(conn) => serve_conn(conn, dispatch).await,
                        // Handshake failure (e.g. an un-certed client rejected by the
                        // WebPkiClientVerifier) — nothing to serve.
                        Err(e) => tracing::debug!(error = %e, "edge: connection handshake failed"),
                    }
                });
            }
        });

        Ok(RunningServer { endpoint, local_addr })
    }
}

/// A running edge server (either plane — the internal mTLS [`Server`] and the
/// player-facing [`crate::PlayerServer`] both return one). Drop or
/// [`RunningServer::close`] to stop.
pub struct RunningServer {
    pub(crate) endpoint: quinn::Endpoint,
    pub(crate) local_addr: SocketAddr,
}

impl RunningServer {
    /// The listener's bound address (valid immediately after [`Server::listen`]).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops the accept loop and closes all live connections.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"server shutting down");
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
            return err_response(&format!("edge: unknown method {:?}", req.method));
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

/// Accepts streams on a single connection, one task per stream.
async fn serve_conn(conn: quinn::Connection, dispatch: Arc<Dispatch>) {
    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let dispatch = dispatch.clone();
                tokio::spawn(async move { serve_stream(send, recv, dispatch).await });
            }
            // Peer closed, idle timeout, or shutdown.
            Err(_) => return,
        }
    }
}

/// Reads one framed request, dispatches it, and writes one framed response.
async fn serve_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream, dispatch: Arc<Dispatch>) {
    let req_bytes = match read_frame(&mut recv).await {
        Ok(b) => b,
        // Malformed / truncated request: nothing to reply to reliably.
        Err(_) => return,
    };

    let resp = dispatch.dispatch(req_bytes).await;
    let resp_bytes = serde_json::to_vec(&resp)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"edge: response encode failed"}"#.to_vec());
    let _ = write_frame(&mut send, &resp_bytes).await;
    let _ = send.finish();
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

//! The PLAYER-facing QUIC plane — the public front door's transport. Same bones as
//! the internal plane (persistent conn, stream-per-call, 4-byte frames, JSON
//! envelopes) but a DIFFERENT trust model, and therefore a different envelope:
//!
//! - TLS is server-cert-only ([`DevCA::server_tls_public`]): a real player cannot
//!   hold a CA-signed client leaf, so the transport authenticates only the server.
//!   The CLIENT is authenticated per-call by the bearer `token` in
//!   [`PlayerRequest`] — verified by the front (gateway), never trusted here.
//! - The internal `wire::Request` is deliberately NOT accepted on this plane: its
//!   `identity` field is trusted-by-mTLS, and on a public port it would be
//!   attacker-controlled. The player envelope carries a `token` (a CLAIM to be
//!   verified), never an identity (a VERIFIED fact).
//! - ALPN is [`crate::PLAYER_ALPN`], not [`crate::ALPN`], so the planes cannot
//!   cross even before certs are checked.
//! - Frames are capped at [`MAX_PLAYER_FRAME`] (1 MiB, mirroring the gateway's HTTP
//!   body cap) instead of the internal 16 MiB, and the endpoint carries an explicit
//!   [`quinn::TransportConfig`] (stream/idle/window caps) because the peer is
//!   untrusted — a certless attacker's per-connection cost must be bounded.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};

use futures::future::BoxFuture;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::client::raw_from_bytes;
use crate::frame::{read_frame_max, write_frame};
use crate::server::{err_response, ok_response, run_caught, HandlerResult, RunningServer, ShutdownState};
use crate::tls::{client_bind_addr, DevCA, TrustAnchor};
use crate::wire::Response;
use crate::Error;

/// Caps a single player-plane frame. 1 MiB — mirrors the gateway's HTTP
/// `MAX_BODY_BYTES` so the two public fronts admit the same payload sizes; the
/// internal plane keeps its 16 MiB [`crate::MAX_FRAME`]. Enforced SERVER-side on
/// read (an attacker controls their client, so the client-side cap is not the
/// security boundary).
pub const MAX_PLAYER_FRAME: usize = 1 << 20; // 1 MiB

/// Max concurrent bidirectional streams one player connection may hold open —
/// bounds the per-connection dispatch fan-out an untrusted peer can force.
const MAX_PLAYER_BIDI_STREAMS: u32 = 16;

/// Idle timeout for a player connection — an abandoned handshake or silent peer is
/// reaped instead of pinning server state indefinitely.
const PLAYER_IDLE_TIMEOUT_MS: u32 = 30_000;

/// Default ceiling on the number of player connections admitted at once, across ALL
/// peers — bounds the accept-loop's fan-out so a flood of certless dials cannot spawn
/// unbounded per-connection tasks. This is the fallback baked into `core/edge`; the
/// live value is threaded down from `core/app` (`PLAYER_MAX_CONNS`, same default),
/// which owns the env surface — the edge crate stays topology- and env-blind. `0`
/// means "no global cap" (a deliberate opt-out, never the default).
pub const DEFAULT_PLAYER_MAX_CONNS: usize = 1024;

/// Default ceiling on concurrent player connections from a SINGLE source IP — a much
/// tighter bound than the global one, so one abusive peer cannot consume the whole
/// global budget. Threaded from `core/app` (`PLAYER_MAX_CONNS_PER_IP`, same default).
/// `0` means "no per-IP cap" (opt-out only). Counted BEFORE the handshake, keyed by
/// the raw UDP source address' IP, so the check costs nothing an attacker can inflate.
pub const DEFAULT_PLAYER_MAX_CONNS_PER_IP: usize = 32;

/// The on-wire envelope for a single player request. Unlike the internal
/// `wire::Request` there is NO identity field: `token` is ATTACKER-CONTROLLED input
/// — a bearer CLAIM the front verifies against a `SessionVerifier` — never a
/// verified identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct PlayerRequest {
    pub method: String,
    /// The caller's bearer token, absent for an unauthenticated call (an `AuthNone`
    /// operation). `#[serde(default)]` is load-bearing: an omitted token must still
    /// parse, or every unauthenticated call dies as a malformed envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The caller's API key — the CLIENT-CLASS credential the front checks against a
    /// key policy, orthogonal to `token` (which identifies the *player*). Like the
    /// token it is an attacker-controlled CLAIM the front verifies; `#[serde(default)]`
    /// keeps a pre-key envelope parsing (it then fails the front's key check as a
    /// clean domain 401, never a malformed-envelope transport error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// The already-encoded request payload, preserved verbatim as raw JSON — the
    /// transport never re-parses the domain body.
    pub payload: Box<RawValue>,
}

/// The player-plane dispatch seam: (method, token, api_key, payload) in, response
/// payload bytes out. ONE handler serves every method — routing, token verification,
/// the API-key policy check and the per-op auth requirement all live in the front
/// (gateway) behind this seam, so the transport stays domain-blind.
pub type PlayerHandler = Arc<
    dyn Fn(String, Option<String>, Option<String>, Vec<u8>) -> BoxFuture<'static, HandlerResult>
        + Send
        + Sync,
>;

/// The player-facing QUIC listener. Construct, [`PlayerServer::set_handler`] (the
/// gateway does this in its `Init`), then [`PlayerServer::listen`]. A server left
/// without a handler still answers — every call gets a transport `ok:false`
/// "front not wired" rather than a hang.
pub struct PlayerServer {
    handler: OnceLock<PlayerHandler>,
    /// Global concurrent-connection cap; `0` = unlimited. Defaults to
    /// [`DEFAULT_PLAYER_MAX_CONNS`] unless [`PlayerServer::with_conn_limits`] overrides
    /// it (which `core/app` always does from its env-owned config).
    max_conns: usize,
    /// Per-source-IP concurrent-connection cap; `0` = unlimited. Defaults to
    /// [`DEFAULT_PLAYER_MAX_CONNS_PER_IP`].
    max_conns_per_ip: usize,
}

impl Default for PlayerServer {
    fn default() -> Self {
        PlayerServer {
            handler: OnceLock::new(),
            max_conns: DEFAULT_PLAYER_MAX_CONNS,
            max_conns_per_ip: DEFAULT_PLAYER_MAX_CONNS_PER_IP,
        }
    }
}

impl PlayerServer {
    pub fn new() -> Self {
        PlayerServer::default()
    }

    /// Installs the single dispatch handler. First set wins (`OnceLock`) — the
    /// front is wired exactly once, at module init.
    pub fn set_handler(&self, h: PlayerHandler) {
        let _ = self.handler.set(h);
    }

    /// Sets the connection admission caps (`global`, `per_ip`) before [`listen`], each
    /// `0` = unlimited. The ENV surface (`PLAYER_MAX_CONNS`/`PLAYER_MAX_CONNS_PER_IP`)
    /// lives in `core/app`, which calls this once on the fully-wired server it took from
    /// the shared handle — the edge crate never reads env, keeping modules topology-blind.
    ///
    /// [`listen`]: PlayerServer::listen
    pub fn with_conn_limits(mut self, global: usize, per_ip: usize) -> Self {
        self.max_conns = global;
        self.max_conns_per_ip = per_ip;
        self
    }

    /// Binds the public QUIC listener on `addr` with server-cert-only TLS
    /// ([`DevCA::server_tls_public`]) and an EXPLICIT transport config — the
    /// internal plane keeps quinn defaults, but a public port faces untrusted,
    /// certless peers, so per-connection cost is capped: [`MAX_PLAYER_BIDI_STREAMS`]
    /// concurrent streams, [`PLAYER_IDLE_TIMEOUT_MS`] idle reap, and the stream
    /// receive window clamped to [`MAX_PLAYER_FRAME`] so a peer cannot make the
    /// server buffer more than one max frame per stream. (Transport knobs, not a
    /// rate limiter — full rate limiting is out of scope.)
    ///
    /// ADMISSION CONTROL: on top of the per-connection transport caps, the accept loop
    /// bounds the NUMBER of concurrent connections — [`PlayerServer::with_conn_limits`]'s
    /// global and per-IP ceilings — so a certless attacker cannot spawn an unbounded
    /// task-per-connection fleet. The peer's source IP is read from the raw UDP datagram
    /// BEFORE the TLS handshake is awaited; over either limit ⇒ [`quinn::Incoming::refuse`]
    /// (no handshake CPU spent, no task spawned). An admitted connection holds a
    /// [`ConnGuard`] moved into its task, so both counters decrement when the connection
    /// (or a failed handshake) ends — no separate evictor.
    pub fn listen(self, addr: SocketAddr, ca: &DevCA) -> Result<RunningServer, Error> {
        let server_cfg = ca.server_tls_public()?;
        let qsc = QuicServerConfig::try_from(server_cfg)
            .map_err(|e| Error::Tls(format!("quic player server config: {e}")))?;
        let mut quinn_cfg = quinn::ServerConfig::with_crypto(Arc::new(qsc));

        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(MAX_PLAYER_BIDI_STREAMS));
        transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            PLAYER_IDLE_TIMEOUT_MS,
        ))));
        transport.stream_receive_window(quinn::VarInt::from_u32(MAX_PLAYER_FRAME as u32));
        quinn_cfg.transport_config(Arc::new(transport));

        let endpoint = quinn::Endpoint::server(quinn_cfg, addr).map_err(Error::Io)?;
        let local_addr = endpoint.local_addr().map_err(Error::Io)?;

        let handler = Arc::new(self.handler);
        let limiter = ConnLimiter::new(self.max_conns, self.max_conns_per_ip);
        let shutdown = ShutdownState::new();
        let accept_endpoint = endpoint.clone();
        let accept_state = shutdown.clone();
        tokio::spawn(async move {
            let mut closing = accept_state.subscribe();
            loop {
                tokio::select! {
                    incoming = accept_endpoint.accept() => {
                        // Cancel-safe: the incoming queue lives in the endpoint.
                        let Some(incoming) = incoming else { break };
                        // Admission BEFORE the handshake: the source IP is a property of
                        // the raw UDP datagram, so refusing here spends no crypto and
                        // spawns no task. `refuse()` sends a CONNECTION_REFUSED close.
                        let ip = incoming.remote_address().ip();
                        let Some(conn_guard) = limiter.try_admit(ip) else {
                            tracing::warn!(%ip, "edge: player connection refused (over conn limit)");
                            incoming.refuse();
                            continue;
                        };
                        let handler = handler.clone();
                        let conn_state = accept_state.clone();
                        // Guard created at the ACCEPT arm and moved into the task —
                        // see [`ShutdownState::enter`].
                        let guard = accept_state.enter();
                        tokio::spawn(async move {
                            let _guard = guard;
                            // Held for the connection's whole life; dropping it frees the
                            // global + per-IP slot (also on a failed handshake below).
                            let _conn_guard = conn_guard;
                            match incoming.await {
                                Ok(conn) => serve_conn(conn, handler, conn_state).await,
                                Err(e) => tracing::debug!(error = %e, "edge: player handshake failed"),
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

/// The mutable admission state, guarded by one lock so `global` and `per_ip` never
/// disagree: `global` is the total live connection count and `per_ip[ip]` its per-source
/// breakdown (an IP is present iff its count is > 0 — a count is removed when it hits 0,
/// so the map never grows past the set of currently-connected IPs).
struct LimiterState {
    global: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// Concurrent-connection admission control for the player plane. Both caps are checked
/// and both counters incremented under ONE lock in [`ConnLimiter::try_admit`], so a burst
/// of simultaneous dials cannot slip past either ceiling. `0` on a cap disables it.
struct ConnLimiter {
    max_conns: usize,
    max_conns_per_ip: usize,
    state: Mutex<LimiterState>,
}

impl ConnLimiter {
    fn new(max_conns: usize, max_conns_per_ip: usize) -> Arc<ConnLimiter> {
        Arc::new(ConnLimiter {
            max_conns,
            max_conns_per_ip,
            state: Mutex::new(LimiterState { global: 0, per_ip: HashMap::new() }),
        })
    }

    /// Admits a connection from `ip`, returning an RAII [`ConnGuard`] whose drop frees the
    /// slot, or `None` if either cap is already at its ceiling. On the `None` paths NOTHING
    /// is mutated (the global check returns before touching the map; the per-IP ceiling is
    /// only reachable when the entry already exists with count ≥ 1), so a refusal leaves no
    /// stray zero entry behind.
    fn try_admit(self: &Arc<Self>, ip: IpAddr) -> Option<ConnGuard> {
        let mut st = self.state.lock().unwrap();
        if self.max_conns != 0 && st.global >= self.max_conns {
            return None;
        }
        let per = st.per_ip.entry(ip).or_insert(0);
        if self.max_conns_per_ip != 0 && *per >= self.max_conns_per_ip {
            return None;
        }
        *per += 1;
        st.global += 1;
        Some(ConnGuard { limiter: self.clone(), ip })
    }
}

/// RAII slot marker: dropping it decrements the global count and the source IP's count,
/// pruning the map entry when it reaches 0. One is held for each admitted connection's
/// whole lifetime (moved into the connection task).
struct ConnGuard {
    limiter: Arc<ConnLimiter>,
    ip: IpAddr,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut st = self.limiter.state.lock().unwrap();
        st.global = st.global.saturating_sub(1);
        if let Some(count) = st.per_ip.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                st.per_ip.remove(&self.ip);
            }
        }
    }
}

/// Accepts streams on a single player connection, one task per stream. Same drain
/// contract as the internal plane's `serve_conn`: the in-flight guard is created
/// where `accept_bi()` yields and moved into the stream task; on graceful shutdown
/// the loop stops accepting NEW streams and returns without closing the connection,
/// letting in-flight stream tasks finish under their own guards.
async fn serve_conn(
    conn: quinn::Connection,
    handler: Arc<OnceLock<PlayerHandler>>,
    state: Arc<ShutdownState>,
) {
    let mut closing = state.subscribe();
    loop {
        tokio::select! {
            res = conn.accept_bi() => match res {
                Ok((send, recv)) => {
                    let handler = handler.clone();
                    let guard = state.enter();
                    tokio::spawn(async move {
                        let _guard = guard;
                        serve_stream(send, recv, handler).await;
                    });
                }
                // Peer closed, idle timeout, or shutdown.
                Err(_) => return,
            },
            _ = closing.wait_for(|c| *c) => return,
        }
    }
}

/// Reads one framed player request (capped at [`MAX_PLAYER_FRAME`]), dispatches it,
/// and writes one framed response. Transport `ok:false` is emitted ONLY for
/// transport faults — oversize frame, malformed envelope, unwired front; a handler
/// `Ok(bytes)` passes through verbatim as `ok:true` (domain outcomes, auth failures
/// included, ride INSIDE those bytes — the pinned error grammar).
async fn serve_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    handler: Arc<OnceLock<PlayerHandler>>,
) {
    let req_bytes = match read_frame_max(&mut recv, MAX_PLAYER_FRAME).await {
        Ok(b) => b,
        Err(Error::FrameTooLarge { size, max }) => {
            // The sender may still be blocked pushing the oversized body (the
            // receive window is one max frame) — stop the receive side so the peer's
            // write unblocks with an error instead of deadlocking, then reply.
            let _ = recv.stop(quinn::VarInt::from_u32(0));
            respond(&mut send, err_response(&format!("edge: player frame too large: {size} > {max}"))).await;
            return;
        }
        // Malformed / truncated request: nothing to reply to reliably.
        Err(_) => return,
    };

    let resp = dispatch(&handler, req_bytes).await;
    respond(&mut send, resp).await;
}

/// Decodes the player envelope and runs the front handler (panic-contained). No
/// method table here: routing is the front's job, behind the single handler.
async fn dispatch(handler: &OnceLock<PlayerHandler>, req_bytes: Vec<u8>) -> Response {
    let req: PlayerRequest = match serde_json::from_slice(&req_bytes) {
        Ok(r) => r,
        Err(_) => return err_response("edge: malformed player request envelope"),
    };
    let Some(h) = handler.get() else {
        return err_response("edge: player front not wired");
    };
    let payload = req.payload.get().as_bytes().to_vec();
    match run_caught(h(req.method, req.token, req.api_key, payload)).await {
        Ok(bytes) => ok_response(bytes),
        Err(e) => err_response(&e.to_string()),
    }
}

/// Serializes and writes one framed response envelope, then finishes the stream and
/// waits for the peer to acknowledge receipt (or the stream/connection to die) —
/// `finish` only queues the data, and the caller's in-flight guard must not release
/// before the reply actually left, or a graceful shutdown could abort its delivery.
async fn respond(send: &mut quinn::SendStream, resp: Response) {
    let resp_bytes = serde_json::to_vec(&resp)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"edge: response encode failed"}"#.to_vec());
    let _ = write_frame(send, &resp_bytes).await;
    let _ = send.finish();
    let _ = send.stopped().await;
}

/// The player-side QUIC client: one persistent connection, stream-per-call —
/// exactly the internal [`crate::Client`]'s shape, but it dials with a key-less
/// [`TrustAnchor`] (server-cert-only, [`crate::PLAYER_ALPN`]) and speaks the
/// [`PlayerRequest`] envelope (token, not identity).
pub struct PlayerClient {
    // The endpoint must outlive the connection (dropping it tears the conn down).
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
}

impl PlayerClient {
    /// Establishes the persistent QUIC connection to `addr`, verifying the server
    /// against `trust` and presenting NO client certificate. Dials with
    /// `ServerName = "localhost"` for the same rustls IP-SNI reason as
    /// [`crate::Client::dial`].
    pub async fn dial(addr: SocketAddr, trust: &TrustAnchor) -> Result<PlayerClient, Error> {
        let qcc = QuicClientConfig::try_from(trust.client_tls_public()?)
            .map_err(|e| Error::Tls(format!("quic player client config: {e}")))?;
        let mut endpoint = quinn::Endpoint::client(client_bind_addr(addr)).map_err(Error::Io)?;
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(qcc)));
        let conn = endpoint
            .connect(addr, "localhost")
            .map_err(|e| Error::Connect(e.to_string()))?
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(PlayerClient { _endpoint: endpoint, conn })
    }

    /// One RPC over a fresh stream: stamps `token` and `api_key` (if any) into the
    /// player envelope and relays `payload` verbatim. `Err(Error::Remote)` is a
    /// TRANSPORT fault at the peer; a completed operation returns `Ok(bytes)` whose
    /// domain status rides inside the payload envelope — callers must check it (the
    /// pinned error grammar: an auth failure is `Ok` here).
    pub async fn call(
        &self,
        method: &str,
        token: Option<&str>,
        api_key: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let req = PlayerRequest {
            method: method.to_string(),
            token: token.map(str::to_string),
            api_key: api_key.map(str::to_string),
            payload: raw_from_bytes(payload)?,
        };
        let env_bytes = serde_json::to_vec(&req).map_err(Error::Codec)?;

        let (mut send, mut recv) =
            self.conn.open_bi().await.map_err(|e| Error::Connection(e.to_string()))?;
        write_frame(&mut send, &env_bytes).await?;
        send.finish().map_err(|e| Error::Stream(e.to_string()))?;

        let resp_bytes = read_frame_max(&mut recv, MAX_PLAYER_FRAME).await?;
        let resp: Response = serde_json::from_slice(&resp_bytes).map_err(Error::Codec)?;
        if !resp.ok {
            return Err(Error::Remote(resp.error.unwrap_or_default()));
        }
        Ok(resp.payload.map(|p| p.get().as_bytes().to_vec()).unwrap_or_default())
    }

    /// Tears down the persistent connection.
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;

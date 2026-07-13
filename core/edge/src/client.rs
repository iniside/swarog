//! The QUIC RPC client (port of Go's `edge/client.go`). Holds a single persistent
//! connection; each call opens a fresh, cheap bidirectional stream over that reused
//! connection (persistent conn, stream-per-call — never a re-dial per call).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::QuicClientConfig;
use serde_json::value::RawValue;

use crate::frame::{frame_bytes, MAX_FRAME};
use crate::tls::{client_bind_addr, DevCA};
use crate::wire::{Request, Response};
use crate::Error;

/// Transport keepalive on the persistent internal connection. MUST stay well below
/// the server's `EDGE_IDLE_TIMEOUT_MS` (30s, `server.rs`) so a quiet-but-live
/// connection is never idle-reaped between two domain calls.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Explicit idle timeout on the client half of the persistent connection — the
/// mirror of the server plane's `EDGE_IDLE_TIMEOUT_MS` (`server.rs`): it pins
/// quinn's current default (30s) for auditability against a future default change.
/// MUST stay well above [`KEEPALIVE_INTERVAL`] so the keepalive keeps a
/// quiet-but-live connection open.
pub(crate) const CLIENT_IDLE_TIMEOUT_MS: u32 = 30_000;

/// Upper bound on the QUIC handshake when dialing a peer. Without it, a dial to a
/// bound-but-dead address (e.g. a crashed svc whose port is still held, or a
/// blackholed route) waits out the full transport idle machinery; a caller holding
/// a lock across the dial (the gateway's route table) would stall everyone. Elapse
/// maps to [`Error::Connect`].
pub(crate) const DIAL_DEADLINE: Duration = Duration::from_secs(5);

/// A QUIC RPC client over one persistent connection. Implements [`opsapi::Caller`],
/// so the generated RPC client (Step 5) composes over it transport-agnostically.
pub struct Client {
    // The endpoint must outlive the connection (dropping it tears the conn down), so
    // it is held even though never touched again after dial.
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
}

impl Client {
    /// Establishes the persistent QUIC connection to `addr`, presenting a CA-signed
    /// client leaf and verifying the server against `ca`. The client dials with
    /// `ServerName = "localhost"` — the rustls IP-SNI gotcha means dialing a bare IP
    /// fails name verification, so the loopback server leaf carries a `localhost`
    /// SAN and the client always names it `localhost` (mTLS point 3).
    pub async fn dial(addr: SocketAddr, ca: &DevCA) -> Result<Client, Error> {
        Self::dial_with_config(addr, ca.client_tls()?).await
    }

    /// Dials with an explicit rustls [`ClientConfig`] — the seam the negative mTLS
    /// tests use to present a mismatched or absent client certificate. Production
    /// callers use [`Client::dial`].
    pub async fn dial_with_config(
        addr: SocketAddr,
        client_cfg: rustls::ClientConfig,
    ) -> Result<Client, Error> {
        let qcc = QuicClientConfig::try_from(client_cfg)
            .map_err(|e| Error::Tls(format!("quic client config: {e}")))?;
        let mut endpoint = quinn::Endpoint::client(client_bind_addr(addr)).map_err(Error::Io)?;
        // Internal stubs cache this connection for the process lifetime. Quinn's
        // default idle timeout can otherwise retire a quiet connection between two
        // domain calls; the next non-retry-safe mutation would then fail on a stale
        // cached connection. Keepalive is transport liveness, not RPC replay.
        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(KEEPALIVE_INTERVAL));
        transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            CLIENT_IDLE_TIMEOUT_MS,
        ))));
        let mut quinn_cfg = quinn::ClientConfig::new(Arc::new(qcc));
        quinn_cfg.transport_config(Arc::new(transport));
        endpoint.set_default_client_config(quinn_cfg);
        let connecting = endpoint
            .connect(addr, "localhost")
            .map_err(|e| Error::Connect(e.to_string()))?;
        // Bound the handshake: a bound-but-dead peer must fail the dial fast, not
        // after the transport idle machinery gives up.
        let conn = tokio::time::timeout(DIAL_DEADLINE, connecting)
            .await
            .map_err(|_| Error::Connect("dial timed out after 5s".into()))?
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Client {
            _endpoint: endpoint,
            conn,
        })
    }

    /// One RPC relaying raw payload bytes verbatim: `payload` is already-encoded
    /// request bytes (assigned straight into the envelope, never re-encoded) and the
    /// returned bytes are the response payload exactly as the server sent them. This
    /// is the gateway relay path — it neither knows nor cares about the payload shape.
    pub async fn call_raw(&self, method: &str, payload: &[u8]) -> Result<Vec<u8>, Error> {
        self.call_raw_id(method, None, payload).await
    }

    /// [`Client::call_raw`] plus a caller identity stamped into the request
    /// envelope's identity field. The gateway's RemoteBackend uses this to carry the
    /// verified player_id to a backend over the (mutually authenticated) edge, so the
    /// peer's generated adapter can read it from the envelope instead of re-verifying.
    pub async fn call_raw_id(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let req = Request {
            method: method.to_string(),
            identity: identity.map(str::to_string),
            payload: raw_from_bytes(payload)?,
        };
        let env_bytes = serde_json::to_vec(&req).map_err(Error::Codec)?;

        // Fresh stream on the persistent connection (stream-per-call).
        let (mut send, mut recv) = self.conn.open_bi().await.map_err(|e| Error::Connection(e.to_string()))?;
        write_call_frame(&mut send, &env_bytes).await?;
        // Finish the write side: signals the stream is complete so the server reads
        // the full frame then EOF.
        send.finish().map_err(|e| Error::Stream(e.to_string()))?;

        let resp_bytes = read_call_frame(&mut recv).await?;
        let resp: Response = serde_json::from_slice(&resp_bytes).map_err(Error::Codec)?;
        if !resp.ok {
            let msg = resp.error.unwrap_or_default();
            // Classify off the TYPED envelope code, never the error text. The internal
            // dispatch sets `code: Some(UnknownMethod)` at the one no-handler branch;
            // a handler that propagates an inner peer's unknown-method MESSAGE (which
            // carries the same `UNKNOWN_METHOD_PREFIX` text) leaves the OUTER reply's
            // code `None`, so it stays `Remote` — the false positive the text sniff
            // used to produce. Internal plane only: `player.rs` never sets the code.
            if resp.code == Some(crate::ResponseCode::UnknownMethod) {
                return Err(Error::UnknownMethod(msg));
            }
            return Err(Error::Remote(msg));
        }
        Ok(resp.payload.map(|p| p.get().as_bytes().to_vec()).unwrap_or_default())
    }

    /// Test seam: the raw quinn connection, for edge tests that need to hold a
    /// stream open below the frame layer (e.g. the stream-grace reap tests).
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    /// Tears down the persistent connection.
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

#[async_trait::async_trait]
impl opsapi::Caller for Client {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
        _retry_mode: opsapi::RetryMode,
    ) -> Result<Vec<u8>, opsapi::Error> {
        self.call_raw_id(method, identity, payload)
            .await
            .map_err(opsapi::Error::from)
    }
}

/// Writes one call frame without erasing Quinn's stream-vs-connection failure.
/// Only an explicit `ConnectionLost` proves the shared connection is unusable;
/// STOPPED, closed/cancelled streams, and 0-RTT rejection belong to this call.
async fn write_call_frame(send: &mut quinn::SendStream, bytes: &[u8]) -> Result<(), Error> {
    let framed = frame_bytes(bytes)?;
    send.write_all(&framed).await.map_err(map_write_error)
}

fn map_write_error(error: quinn::WriteError) -> Error {
    let message = error.to_string();
    match error {
        quinn::WriteError::ConnectionLost(_) => Error::Connection(message),
        _ => Error::Stream(message),
    }
}

/// Reads one call frame through Quinn's inherent API so `ConnectionLost` remains
/// typed. A reset, closed/cancelled stream, or early finish is stream-local; an
/// oversized frame retains the existing typed `FrameTooLarge` error.
async fn read_call_frame(recv: &mut quinn::RecvStream) -> Result<Vec<u8>, Error> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(map_read_error)?;
    let size = u32::from_be_bytes(len) as usize;
    if size > MAX_FRAME {
        return Err(Error::FrameTooLarge { size, max: MAX_FRAME });
    }

    let mut bytes = vec![0u8; size];
    recv.read_exact(&mut bytes).await.map_err(map_read_error)?;
    Ok(bytes)
}

fn map_read_error(error: quinn::ReadExactError) -> Error {
    let message = error.to_string();
    match error {
        quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(_)) => {
            Error::Connection(message)
        }
        _ => Error::Stream(message),
    }
}

/// Wraps already-encoded JSON payload bytes into a `RawValue` for the envelope. An
/// empty payload is treated as JSON `null` (a call with no request body). An invalid
/// payload (non-UTF-8 or non-JSON) is a programmer error surfaced as
/// [`Error::Codec`] — serde's own parse error, never mislabelled as TLS. Shared with
/// the player plane (`player.rs`).
pub(crate) fn raw_from_bytes(payload: &[u8]) -> Result<Box<RawValue>, Error> {
    if payload.is_empty() {
        return RawValue::from_string("null".to_string()).map_err(Error::Codec);
    }
    serde_json::from_slice::<Box<RawValue>>(payload).map_err(Error::Codec)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;

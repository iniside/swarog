//! The QUIC RPC client (port of Go's `edge/client.go`). Holds a single persistent
//! connection; each call opens a fresh, cheap bidirectional stream over that reused
//! connection (persistent conn, stream-per-call — never a re-dial per call).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::QuicClientConfig;
use serde_json::value::RawValue;

use crate::frame::{read_frame, write_frame};
use crate::tls::{client_bind_addr, DevCA};
use crate::wire::{Request, Response};
use crate::Error;

/// Transport keepalive on the persistent internal connection. MUST stay well below
/// the server's `EDGE_IDLE_TIMEOUT_MS` (30s, `server.rs`) so a quiet-but-live
/// connection is never idle-reaped between two domain calls.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

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
        let mut quinn_cfg = quinn::ClientConfig::new(Arc::new(qcc));
        quinn_cfg.transport_config(Arc::new(transport));
        endpoint.set_default_client_config(quinn_cfg);
        let conn = endpoint
            .connect(addr, "localhost")
            .map_err(|e| Error::Connect(e.to_string()))?
            .await
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
        write_frame(&mut send, &env_bytes).await?;
        // Finish the write side: signals the stream is complete so the server reads
        // the full frame then EOF.
        send.finish().map_err(|e| Error::Stream(e.to_string()))?;

        let resp_bytes = read_frame(&mut recv).await?;
        let resp: Response = serde_json::from_slice(&resp_bytes).map_err(Error::Codec)?;
        if !resp.ok {
            let msg = resp.error.unwrap_or_default();
            // The internal server's unknown-method sentinel (shared
            // `crate::UNKNOWN_METHOD_PREFIX` — producer and detector cannot drift)
            // becomes the typed variant; every other peer error stays `Remote`.
            // Internal plane only: `player.rs` must NOT mirror this — the player
            // server has no method table and never produces the sentinel.
            if msg.starts_with(crate::UNKNOWN_METHOD_PREFIX) {
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

//! Fleet health checks: a raw-TCP `/readyz` probe (copied from
//! `tools/devctl/src/supervisor.rs::ready` — std `TcpStream`, no HTTP client)
//! and the pre-first-spawn stale-listener check (copied from
//! `tools/splitproof/src/main.rs::ensure_no_stale_listener`).

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{bail, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const STALE_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

/// What a single readyz probe observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeResult {
    /// The service answered `HTTP/1.x 200` on `/readyz`.
    Ready,
    /// Something accepted the connection but did not answer 200 (503 while
    /// warming up, a torn response, an I/O error mid-exchange).
    NotReady,
    /// Nothing is listening (connection refused / connect timeout).
    ConnectFailed,
}

/// One bounded `GET /readyz` against `127.0.0.1:port` (300ms connect, 500ms
/// read): `Ready` iff the response opens with an `HTTP/1.x 200` status line.
pub fn probe(port: u16) -> ProbeResult {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
        return ProbeResult::ConnectFailed;
    };
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    if stream
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return ProbeResult::NotReady;
    }
    let mut response = [0u8; 64];
    let Ok(read) = stream.read(&mut response) else {
        return ProbeResult::NotReady;
    };
    let response = &response[..read];
    // "HTTP/1.1 200 ..." / "HTTP/1.0 200 ..." — the status code sits at a
    // fixed offset (bytes 8..13 are " 200 "), so nothing later in the payload
    // can fake a match.
    if response.len() >= 13 && &response[..7] == b"HTTP/1." && &response[8..13] == b" 200 " {
        ProbeResult::Ready
    } else {
        ProbeResult::NotReady
    }
}

/// Pre-spawn stale-listener check: before a service's FIRST spawn, its readyz
/// port must have no listener — anything accepting a TCP connect is a stale
/// process from a previous hung run (or an unrelated port conflict), and the
/// health gate would then probe the OLD listener while the new child dies on
/// bind. Connection refused is the good case.
///
/// Deliberately NOT used on a crash respawn: the just-killed process's
/// TIME_WAIT/lingering socket state could false-positive there.
pub fn ensure_no_stale_listener(svc_name: &str, port: u16) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&addr, STALE_CONNECT_TIMEOUT).is_ok() {
        bail!(
            "port :{port} already has a listener before spawn ({svc_name}) — stale process \
             from a previous run or port conflict; clean up and retry"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;

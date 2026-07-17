//! A minimal, stage-owned HTTP/1.1 server on a loopback port.
//!
//! It exists for ONE reason, and it is not convenience: a verify stage that must
//! prove a resolved address was *used* has to be able to answer with an address
//! that is **not** the one the process would have guessed. Every address weles's
//! agent serves is `127.0.0.1:{port from the manifest}`, and
//! `cmd/gateway-svc`'s standalone defaults are the same bytes — so on the real
//! fleet, "resolved" and "defaulted" are indistinguishable by construction. A
//! fake agent under the stage's control is what breaks that tie, because the
//! stage picks the ports.
//!
//! Deliberately NOT hyper/axum: verifyctl's tokio pin carries no `net` (root
//! `Cargo.toml`), and widening it would reach every crate in the build graph to
//! serve one stage's fixture. Blocking `std::net` on its own thread costs no
//! dependency and no feature.
//!
//! Deliberately NOT a general HTTP server: it reads a bounded request, hands the
//! path + body to a closure, and writes the closure's answer with
//! `Connection: close`. Anything it cannot parse is a 400. It faces one client —
//! a `cmd/*-svc` on loopback, under the trusted-local-operator model (CLAUDE.md,
//! "Dev tooling scope") — so it is bounded and correct, not hardened.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};

/// Cap on a request line + headers + body. The bodies here are a short provider
/// name and a tiny enum; a fixture that buffered whatever arrived would be a
/// worse citizen than the thing it is standing in for.
const MAX_REQUEST_BYTES: u64 = 8 * 1024;

/// Bound on one client's request, so a half-open connection cannot park the
/// accept loop's thread forever.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the accept loop looks at its stop flag. Non-blocking accept +
/// poll-sleep is the shape `weles::control::ControlServer` uses; there is no
/// `.await` here for a flag to fail to reach.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// What a handler answers: an HTTP status and a JSON body.
pub(crate) type Answer = (u16, Vec<u8>);

/// A running fixture. Dropping it stops and JOINS the thread — a stage that
/// leaked one would leave a listener behind for the next rollout to trip over.
pub(crate) struct FakeHttp {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeHttp {
    /// Binds an ephemeral loopback port and serves `handler` until dropped.
    ///
    /// `handler` receives `(path, body)` and answers. It runs on the fixture's
    /// thread, so it must not touch anything the stage's thread owns — every
    /// handler here answers from values moved into it.
    pub(crate) fn start(
        handler: impl Fn(&str, &[u8]) -> Answer + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).context("bind a fake HTTP fixture port")?;
        let port = listener.local_addr()?.port();
        listener
            .set_nonblocking(true)
            .context("make the fixture accept loop pollable")?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(format!("verify-fake-http-{port}"))
            .spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = serve_one(stream, &handler) {
                                eprintln!("verifyctl: fake HTTP fixture :{port} connection: {error:#}");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(ACCEPT_POLL);
                        }
                        Err(error) => {
                            eprintln!("verifyctl: fake HTTP fixture :{port} accept: {error}");
                            std::thread::sleep(ACCEPT_POLL);
                        }
                    }
                }
            })
            .context("spawn a fake HTTP fixture")?;
        Ok(Self { port, stop, thread: Some(thread) })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// `127.0.0.1:<port>` — the form every address on weles's wire takes.
    pub(crate) fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // Bounded by construction: the loop polls the flag every
            // ACCEPT_POLL, and a connection in flight is bounded by READ_TIMEOUT.
            let _ = thread.join();
        }
    }
}

fn serve_one(stream: TcpStream, handler: &impl Fn(&str, &[u8]) -> Answer) -> Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // "POST /resolve HTTP/1.1" — the path is what a handler routes on. The
    // METHOD is deliberately handed over too: a client that switched to GET is a
    // real drift, and a fixture that ignored the method could not see it.
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header.trim().is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().to_string())
        {
            length = value.parse().unwrap_or(0);
        }
    }

    let mut body = Vec::new();
    if length > 0 {
        reader
            .get_mut()
            .set_read_timeout(Some(READ_TIMEOUT))?;
        (&mut reader)
            .take((length as u64).min(MAX_REQUEST_BYTES))
            .read_to_end(&mut body)?;
    }

    let (status, payload) = handler(&format!("{method} {path}"), &body);
    let mut stream = reader.into_inner();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        payload.len()
    )?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Unknown",
    }
}

#[cfg(test)]
#[path = "fake_http_tests.rs"]
mod fake_http_tests;

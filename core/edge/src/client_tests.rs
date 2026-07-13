use super::*;
use crate::tls::DevCA;
use crate::{Handler, HandlerResult, Server};
use futures::future::BoxFuture;
use std::net::SocketAddr;
use std::sync::Arc;

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn handler<F>(f: F) -> Handler
where
    F: Fn(Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// A dial to a bound-but-silent UDP socket (nothing speaks QUIC there) must fail
/// within [`DIAL_DEADLINE`], not wait out the transport idle machinery — the seam
/// the gateway's route table depends on so one dead svc can't stall its callers.
#[tokio::test]
async fn dial_to_silent_socket_fails_within_deadline() {
    // Bind a real UDP socket so the packets aren't ICMP-rejected; never answer.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();

    let ca = DevCA::generate().unwrap();
    let started = std::time::Instant::now();
    let res = Client::dial(addr, &ca).await;
    let elapsed = started.elapsed();

    let err = res.err().expect("dial to a silent socket must fail");
    assert!(
        matches!(err, Error::Connect(_) | Error::Connection(_)),
        "expected a connect/connection error, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "dial must fail within the deadline (took {elapsed:?})"
    );
}

/// Pin the client-side timing invariants: the pinned idle timeout must exceed the
/// keepalive (twin of `server_tests::edge_timing_invariants`), and the dial
/// deadline stays at the documented 5s.
#[test]
fn client_timing_invariants() {
    assert!(
        Duration::from_millis(CLIENT_IDLE_TIMEOUT_MS as u64) > KEEPALIVE_INTERVAL,
        "idle timeout must exceed the keepalive or live connections get reaped"
    );
    assert_eq!(DIAL_DEADLINE, Duration::from_secs(5));
}

#[test]
fn concrete_quinn_call_errors_preserve_only_connection_loss_as_fatal() {
    let write_cases = [
        (
            quinn::WriteError::ConnectionLost(quinn::ConnectionError::LocallyClosed),
            true,
            "write connection lost",
        ),
        (
            quinn::WriteError::Stopped(quinn::VarInt::from_u32(7)),
            false,
            "peer STOPPED",
        ),
        (quinn::WriteError::ClosedStream, false, "closed send stream"),
        (quinn::WriteError::ZeroRttRejected, false, "0-RTT write"),
    ];
    for (error, fatal, name) in write_cases {
        assert_eq!(
            matches!(map_write_error(error), Error::Connection(_)),
            fatal,
            "{name}"
        );
    }

    let read_cases = [
        (
            quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(
                quinn::ConnectionError::LocallyClosed,
            )),
            true,
            "read connection lost",
        ),
        (
            quinn::ReadExactError::ReadError(quinn::ReadError::Reset(
                quinn::VarInt::from_u32(9),
            )),
            false,
            "peer reset",
        ),
        (
            quinn::ReadExactError::ReadError(quinn::ReadError::ClosedStream),
            false,
            "closed receive stream",
        ),
        (
            quinn::ReadExactError::ReadError(quinn::ReadError::IllegalOrderedRead),
            false,
            "unprovenanced read failure",
        ),
        (quinn::ReadExactError::FinishedEarly(2), false, "finished early"),
        (
            quinn::ReadExactError::ReadError(quinn::ReadError::ZeroRttRejected),
            false,
            "0-RTT read",
        ),
    ];
    for (error, fatal, name) in read_cases {
        assert_eq!(
            matches!(map_read_error(error), Error::Connection(_)),
            fatal,
            "{name}"
        );
    }
}

/// One oversized response fails only its stream while an echo runs concurrently
/// and remains usable afterwards on the same persistent client connection.
#[tokio::test]
async fn oversized_response_is_stream_local_while_concurrent_echo_survives() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let oversized = Arc::new(format!("\"{}\"", "a".repeat(MAX_FRAME)).into_bytes());

    let mut server = Server::new();
    server.handle("echo", handler(|payload| Box::pin(async move { Ok(payload) })));
    server.handle(
        "oversized",
        handler({
            let entered = entered.clone();
            let release = release.clone();
            let oversized = oversized.clone();
            move |_| {
                let entered = entered.clone();
                let release = release.clone();
                let oversized = oversized.clone();
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok((*oversized).clone())
                })
            }
        }),
    );
    let running = server.listen(loopback(), &ca).unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let failing = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("oversized", b"null").await }
    });
    entered.notified().await;

    let echo = client.call_raw("echo", br#"{"during":true}"#).await.unwrap();
    assert_eq!(echo, br#"{"during":true}"#);

    release.notify_one();
    let error = failing.await.unwrap().unwrap_err();
    assert!(matches!(error, Error::Stream(_)), "got {error:?}");

    let echo = client.call_raw("echo", br#"{"after":true}"#).await.unwrap();
    assert_eq!(echo, br#"{"after":true}"#);

    client.close();
    running.close();
}

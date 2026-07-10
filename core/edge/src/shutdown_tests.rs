//! Graceful-shutdown (drain) tests for both QUIC planes: `RunningServer::shutdown`
//! must wait for in-flight handlers (up to the grace), stop admitting new work the
//! moment it begins, and abort stragglers once the grace expires. Live QUIC over
//! loopback, same style as the `e2e_tests`/`player_e2e_tests` modules in `lib.rs`.

use super::*;
use futures::future::BoxFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn handler<F>(f: F) -> Handler
where
    F: Fn(Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// An internal-plane server with one handler that sleeps `delay` then echoes.
fn slow_echo_server(delay: Duration) -> Server {
    let mut srv = Server::new();
    srv.handle(
        "slow",
        handler(move |payload| {
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(payload)
            })
        }),
    );
    srv
}

// (i) The drain waits: a handler is mid-flight when `shutdown` is called; the client
// must still receive the FULL response (not an aborted stream), and `shutdown` must
// return only after the handler finished.
#[tokio::test]
async fn shutdown_waits_for_inflight_handler() {
    let ca = DevCA::generate().unwrap();
    let running = slow_echo_server(Duration::from_millis(200))
        .listen(loopback(), &ca)
        .unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", br#"{"n":1}"#).await }
    });
    // Let the request reach the handler before shutting down.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    running.shutdown(Duration::from_secs(2)).await;
    let waited = started.elapsed();

    // The in-flight call completed with the full response — the drain waited.
    let resp = call.await.unwrap().unwrap();
    assert_eq!(resp, br#"{"n":1}"#);
    // And shutdown actually blocked on the handler (~150ms left of its sleep).
    assert!(
        waited >= Duration::from_millis(100),
        "shutdown returned before the in-flight handler finished: {waited:?}"
    );

    client.close();
}

// (ii) Once shutdown begins (closing flipped, drain still in progress), a NEW client
// connection is never served — the endpoint-accept loop stopped admitting.
#[tokio::test]
async fn shutdown_stops_accepting_new_connections() {
    let ca = DevCA::generate().unwrap();
    let running = Arc::new(
        slow_echo_server(Duration::from_millis(300))
            .listen(loopback(), &ca)
            .unwrap(),
    );
    let addr = running.local_addr();
    let client = Arc::new(Client::dial(addr, &ca).await.unwrap());

    // Keep one call in flight so shutdown is mid-drain when we dial. (Non-null
    // payload: an echoed JSON `null` rides as `"payload":null`, which deserializes
    // to an ABSENT payload — empty bytes — so it can't prove the echo.)
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", br#"{"n":2}"#).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let shut = tokio::spawn({
        let running = running.clone();
        async move { running.shutdown(Duration::from_secs(2)).await }
    });
    // Give the closing flag time to break the accept loop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A new connection attempt must not be served: the unaccepted handshake either
    // errors once the endpoint closes, or (worst case) never completes — bound it.
    let dial = tokio::time::timeout(Duration::from_secs(3), Client::dial(addr, &ca)).await;
    match dial {
        Err(_elapsed) => { /* never accepted — not served */ }
        Ok(Err(_)) => { /* rejected — not served */ }
        Ok(Ok(new_client)) => {
            // Defensive: even if a connection object materialized, it must not serve.
            let r = new_client.call_raw("slow", b"null").await;
            assert!(r.is_err(), "a post-shutdown connection must not be served, got {r:?}");
        }
    }

    // The pre-existing in-flight call still drained fine.
    let resp = call.await.unwrap().unwrap();
    assert_eq!(resp, br#"{"n":2}"#);
    shut.await.unwrap();
    client.close();
}

// (iii) The grace bounds the drain: a 5s straggler cannot hold teardown hostage —
// shutdown(200ms) aborts it and returns promptly (grace + the wait_idle bound).
#[tokio::test]
async fn shutdown_grace_timeout_aborts_stragglers() {
    let ca = DevCA::generate().unwrap();
    let running = slow_echo_server(Duration::from_secs(5))
        .listen(loopback(), &ca)
        .unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", b"null").await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    running.shutdown(Duration::from_millis(200)).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown must return at ~grace, not wait out a 5s straggler: {elapsed:?}"
    );

    // The straggler was aborted, not served.
    let r = call.await.unwrap();
    assert!(r.is_err(), "the aborted straggler must not receive a response, got {r:?}");
}

// Player-plane smoke test: the tracking struct is shared, so one drain proof
// suffices — a single in-flight player call survives `shutdown`.
#[tokio::test]
async fn player_shutdown_drains_inflight_call() {
    let ca = DevCA::generate().unwrap();
    let srv = PlayerServer::new();
    srv.set_handler(Arc::new(|_method, _token, _key, payload| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(payload)
        })
    }));
    let running = srv.listen(loopback(), &ca).unwrap();
    let client =
        Arc::new(PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call("slow", None, None, br#"{"p":1}"#).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    running.shutdown(Duration::from_secs(2)).await;

    let resp = call.await.unwrap().unwrap();
    assert_eq!(resp, br#"{"p":1}"#);
    client.close();
}

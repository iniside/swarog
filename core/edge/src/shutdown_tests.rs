//! Graceful-shutdown (drain) tests for both QUIC planes: `RunningServer::shutdown`
//! must wait for in-flight handlers (up to the grace), stop admitting new work the
//! moment it begins, and abort stragglers once the grace expires. Live QUIC over
//! loopback, same style as the `e2e_tests`/`player_e2e_tests` modules in `lib.rs`.

use super::*;
use futures::future::BoxFuture;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// An internal-plane server with one handler that pings `entered` on entry (BEFORE its
/// delay), sleeps `delay`, sets `resumed` (AFTER the sleep resolves but BEFORE the echo
/// write leaves the handler), then echoes. The entry ping lets a test wait for the
/// happens-before fact "the request reached the handler" instead of guessing a sleep
/// window — the tokio `Notify` is the ordering seam. `resumed` is the drop-vs-abort
/// distinction: it flips ONLY if the handler future ran past its sleep, so a straggler
/// whose future was dropped mid-sleep leaves it false while a merely stream-aborted
/// (but still-executing) handler flips it when the sleep elapses.
fn signaling_slow_echo(
    delay: Duration,
    entered: Arc<tokio::sync::Notify>,
    resumed: Arc<AtomicBool>,
) -> Server {
    let mut srv = Server::new();
    srv.handle(
        "slow",
        handler(move |payload| {
            let entered = entered.clone();
            let resumed = resumed.clone();
            Box::pin(async move {
                entered.notify_one();
                tokio::time::sleep(delay).await;
                resumed.store(true, Ordering::SeqCst);
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
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    let running = signaling_slow_echo(Duration::from_millis(200), entered.clone(), resumed)
        .listen(loopback(), &ca)
        .unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", br#"{"n":1}"#).await }
    });
    // Happens-before: wait until the handler has actually entered (5s hang-guard),
    // not a guessed sleep window, before shutting down.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");

    running.shutdown(Duration::from_secs(2)).await;

    // Ordering fact (no wall-clock margin): shutdown was initiated only AFTER the
    // handler entered, yet the in-flight call still completed with its FULL response —
    // so the drain waited for the mid-flight handler.
    let resp = call.await.unwrap().unwrap();
    assert_eq!(resp, br#"{"n":1}"#);

    client.close();
}

// (ii) Once shutdown begins (closing flipped, drain still in progress), a NEW client
// connection is never served — the endpoint-accept loop stopped admitting.
#[tokio::test]
async fn shutdown_stops_accepting_new_connections() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    let running = Arc::new(
        signaling_slow_echo(Duration::from_millis(300), entered.clone(), resumed)
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
    // Happens-before: the handler has entered (5s hang-guard), so a call is genuinely
    // in flight when we begin shutdown.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");

    let shut = tokio::spawn({
        let running = running.clone();
        async move { running.shutdown(Duration::from_secs(2)).await }
    });

    // Poll-until: a NEW connection must end up not served. Rather than sleeping a
    // guessed window then asserting once (racing the accept loop's observation of the
    // closing flag), retry dials until one is rejected — the endpoint stops admitting
    // as shutdown progresses. Overall 5s deadline is the hang-guard.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut not_served = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), Client::dial(addr, &ca)).await {
            Err(_elapsed) => {
                not_served = true; // handshake never accepted
                break;
            }
            Ok(Err(_)) => {
                not_served = true; // handshake rejected
                break;
            }
            Ok(Ok(new_client)) => {
                // A connection object materialized — it must not actually serve a call.
                // The call itself is timeout-bounded too: a call that never completes
                // mid-shutdown is "not served" (and must not hang this loop past the
                // deadline). The healthy-path handler answers in ~200ms, so 1s is a
                // clean split, and the outer deadline stays the overall hang-guard.
                match tokio::time::timeout(
                    Duration::from_secs(1),
                    new_client.call_raw("slow", b"null"),
                )
                .await
                {
                    Err(_) | Ok(Err(_)) => {
                        not_served = true; // hung or rejected — either way, not served
                        break;
                    }
                    Ok(Ok(_)) => {
                        // Still served — the accept loop hasn't observed the close yet.
                        new_client.close();
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
    assert!(not_served, "a post-shutdown connection was still served within the deadline");

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
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    let running = signaling_slow_echo(Duration::from_secs(5), entered.clone(), resumed)
        .listen(loopback(), &ca)
        .unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", b"null").await }
    });
    // Happens-before: the 5s handler has entered (5s hang-guard) before we shut down.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");

    running.shutdown(Duration::from_millis(200)).await;

    // Ordering fact (no wall-clock margin): the straggler received an ERROR, not the
    // echo. Had the 200ms grace NOT bounded the drain, shutdown would have waited out
    // the full 5s handler and the call would have succeeded with `null`. Its failure
    // proves the grace aborted it early.
    let r = call.await.unwrap();
    assert!(r.is_err(), "the aborted straggler must not receive a response, got {r:?}");
}

// Player-plane smoke test: the tracking struct is shared, so one drain proof
// suffices — a single in-flight player call survives `shutdown`.
#[tokio::test]
async fn player_shutdown_drains_inflight_call() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let srv = PlayerServer::new();
    srv.set_handler({
        let entered = entered.clone();
        Arc::new(move |_method, _token, _key, payload| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.notify_one();
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(payload)
            })
        })
    });
    let running = srv.listen(loopback(), &ca).unwrap();
    let client =
        Arc::new(PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call("slow", None, None, br#"{"p":1}"#).await }
    });
    // Happens-before: the player handler has entered (5s hang-guard) before shutdown.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("player handler never entered");

    running.shutdown(Duration::from_secs(2)).await;

    let resp = call.await.unwrap().unwrap();
    assert_eq!(resp, br#"{"p":1}"#);
    client.close();
}

// (iv) The straggler's HANDLER FUTURE is dropped at grace — not merely its stream.
// `endpoint.close()` alone resets the quinn streams but leaves a detached
// `tokio::spawn`'d handler running; the abort registry drops the future itself.
// `resumed` is the proof seam: it flips ONLY when the handler runs PAST its 5s sleep.
#[tokio::test]
async fn shutdown_grace_drops_straggler_future_which_never_resumes() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    let running = signaling_slow_echo(Duration::from_secs(5), entered.clone(), resumed.clone())
        .listen(loopback(), &ca)
        .unwrap();
    let client = Arc::new(Client::dial(running.local_addr(), &ca).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", b"null").await }
    });
    // Happens-before: the 5s handler has entered before we shut down (5s hang-guard).
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");

    running.shutdown(Duration::from_millis(200)).await;

    // Wait comfortably past the handler's own 5s sleep (measured from entry). On the
    // OLD code `endpoint.close()` only reset the stream while the detached handler
    // kept sleeping, so `resumed` would flip at t≈5s. Post-fix its FUTURE was dropped
    // mid-sleep at the 200ms grace, so it never reaches the post-sleep store.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !resumed.load(Ordering::SeqCst),
        "straggler handler future must be dropped mid-sleep, not run to completion after shutdown"
    );

    let _ = call.await;
}

// (v) The player-plane analog of (iv): the shared `ShutdownState` tracks the player
// stream-spawn site too, so a mid-flight player handler's future is dropped at grace.
// Proven server-side via the same `resumed` seam (CLAUDE.md names BOTH planes).
#[tokio::test]
async fn player_shutdown_grace_drops_straggler_future_which_never_resumes() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    let srv = PlayerServer::new();
    srv.set_handler({
        let entered = entered.clone();
        let resumed = resumed.clone();
        Arc::new(move |_method, _token, _key, payload| {
            let entered = entered.clone();
            let resumed = resumed.clone();
            Box::pin(async move {
                entered.notify_one();
                tokio::time::sleep(Duration::from_secs(5)).await;
                resumed.store(true, Ordering::SeqCst);
                Ok(payload)
            })
        })
    });
    let running = srv.listen(loopback(), &ca).unwrap();
    let client =
        Arc::new(PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap());

    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call("slow", None, None, b"null").await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("player handler never entered");

    running.shutdown(Duration::from_millis(200)).await;

    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !resumed.load(Ordering::SeqCst),
        "player straggler handler future must be dropped mid-sleep, not run to completion after shutdown"
    );

    let _ = call.await;
}

// (vi) No-leak: a burst of normally-completing requests must leave the abort registry
// EMPTY. `enter()` inserts an entry and `track()` fills it; if a fast task's guard
// dropped before `track` ran the fill must be a no-op — otherwise every request leaks
// one AbortHandle, growing the map unboundedly (worse than the bug). After the burst
// drains AND the connection closes, `tracked_len()` must return to 0.
#[tokio::test]
async fn completed_handlers_leave_no_tracked_abort_handles() {
    let ca = DevCA::generate().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    // Zero-delay echo: each call returns immediately, maximizing the track()-after-fast-
    // completion race the no-leak invariant must survive.
    let running = signaling_slow_echo(Duration::from_millis(0), entered.clone(), resumed)
        .listen(loopback(), &ca)
        .unwrap();
    let client = Client::dial(running.local_addr(), &ca).await.unwrap();

    for i in 0..2000u32 {
        let payload = format!(r#"{{"n":{i}}}"#);
        let resp = client.call_raw("slow", payload.as_bytes()).await.unwrap();
        assert_eq!(resp, payload.as_bytes());
    }

    // Close the connection so its connection-level guard also drops, then poll: every
    // stream task's guard AND the connection guard must have removed their entry.
    client.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut len = running.shutdown.tracked_len();
    while len != 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        len = running.shutdown.tracked_len();
    }
    assert_eq!(
        len, 0,
        "completed handlers left {len} tracked abort handles behind (unbounded-growth leak)"
    );
}

use super::*;

use std::time::Duration;

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A trivial player server that echoes its payload — enough to keep an admitted
/// connection live while a test holds it open.
fn echo_server() -> PlayerServer {
    let srv = PlayerServer::new();
    srv.set_handler(Arc::new(|_method, _token, _key, payload| {
        Box::pin(async move { Ok(payload) })
    }));
    srv
}

/// Dials the player plane, bounding the attempt so a REFUSED connection (which quinn
/// surfaces as a connection error) can't hang the test. `Ok(Some)` = admitted,
/// `Ok(None)` = refused/failed, `Err` = the dial didn't resolve within the window.
async fn try_dial(addr: SocketAddr, trust: &TrustAnchor) -> Result<Option<PlayerClient>, ()> {
    match tokio::time::timeout(Duration::from_secs(3), PlayerClient::dial(addr, trust)).await {
        Ok(Ok(client)) => Ok(Some(client)),
        Ok(Err(_)) => Ok(None),
        Err(_) => Err(()),
    }
}

// Admission control: the global cap refuses a connection over the ceiling BEFORE the
// handshake, and releasing an admitted connection (RAII guard drop) frees a slot for a
// subsequent dial. with_conn_limits(2, 2): two live connections fill the global budget,
// a third is refused, and after dropping one a fourth is admitted within a bounded window.
#[tokio::test]
async fn player_global_conn_cap_refuses_over_limit_and_frees_on_drop() {
    let ca = DevCA::generate().unwrap();
    let running = echo_server().with_conn_limits(2, 2).listen(loopback(), &ca).unwrap();
    let addr = running.local_addr();
    let trust = ca.trust_anchor();

    // Two admitted, held open — the global budget (2) is now full.
    let c1 = try_dial(addr, &trust).await.unwrap().expect("first dial admitted");
    let c2 = try_dial(addr, &trust).await.unwrap().expect("second dial admitted");

    // The third is over the global cap → refused (no handshake completes).
    assert!(
        try_dial(addr, &trust).await.unwrap().is_none(),
        "third dial must be refused while the global cap is full"
    );

    // Free one slot; the server-side guard drops once it notices the close.
    c1.close();
    drop(c1);

    // Poll-retry until the freed slot admits a fourth connection (bounded).
    let mut admitted = None;
    for _ in 0..50 {
        if let Some(c) = try_dial(addr, &trust).await.unwrap() {
            admitted = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(admitted.is_some(), "a fourth dial must succeed after a slot is freed");

    drop(c2);
    drop(admitted);
    running.close();
}

// The per-IP cap is a distinct counter from the global one: with a generous global cap
// (10) but a per-IP cap of 1, the SECOND connection from the same loopback IP is refused
// even though the global budget is nowhere near full.
#[tokio::test]
async fn player_per_ip_conn_cap_refuses_second_from_same_ip() {
    let ca = DevCA::generate().unwrap();
    let running = echo_server().with_conn_limits(10, 1).listen(loopback(), &ca).unwrap();
    let addr = running.local_addr();
    let trust = ca.trust_anchor();

    let c1 = try_dial(addr, &trust).await.unwrap().expect("first dial admitted");
    assert!(
        try_dial(addr, &trust).await.unwrap().is_none(),
        "second dial from the same IP must be refused by the per-IP cap"
    );

    drop(c1);
    running.close();
}

// Address-validation gate (anti-spoof admission): every FIRST Incoming from a fresh
// dial is UNVALIDATED, so the accept loop answers it with a stateless Retry and
// reserves NO slot; the dialer echoes the token and re-arrives validated, and only
// then is a slot taken. With BOTH caps at 1, admission of the retried dial proves the
// slot was reserved exactly once (a buggy pre-validation reservation would either trip
// the cap on the validated re-arrival or leak a phantom slot), and the follow-up
// refusal proves the validated connection genuinely holds it. A true off-path spoofed
// source cannot be forged at this level — quinn only surfaces an `Incoming` for a
// well-formed Initial, and a loopback dialer is on-path, so it always completes the
// Retry. That negative (spoof flood never consumes budget) rests on quinn's Retry
// semantics and is deferred to the live split assertions (Step 9a).
#[tokio::test]
async fn player_dial_traverses_retry_gate_and_reserves_slot_exactly_once() {
    let ca = DevCA::generate().unwrap();
    let running = echo_server().with_conn_limits(1, 1).listen(loopback(), &ca).unwrap();
    let addr = running.local_addr();
    let trust = ca.trust_anchor();

    // Admitted despite caps of 1 — the unvalidated first Incoming reserved nothing.
    let client = try_dial(addr, &trust).await.unwrap().expect("retried dial admitted");
    let echoed = client.call("echo", None, None, br#"{"ping":1}"#).await.unwrap();
    assert_eq!(echoed, br#"{"ping":1}"#);

    // The single slot is held by the validated connection, so a second dial is refused.
    assert!(
        try_dial(addr, &trust).await.unwrap().is_none(),
        "second dial must be refused while the only slot is held"
    );

    drop(client);
    running.close();
}

// The serde(default) proof at the envelope level: a request with the token AND the
// api_key OMITTED — the shape every pre-key unauthenticated caller sends — must
// parse (it then fails the FRONT's key check as a domain 401, never as a malformed
// envelope here).
#[test]
fn omitted_token_and_api_key_envelope_parses() {
    let req: PlayerRequest =
        serde_json::from_slice(br#"{"method":"leaderboard.top","payload":{"n":10}}"#).unwrap();
    assert_eq!(req.method, "leaderboard.top");
    assert_eq!(req.token, None);
    assert_eq!(req.api_key, None);
    assert_eq!(req.payload.get(), r#"{"n":10}"#);
}

#[test]
fn token_and_api_key_roundtrip_and_absent_fields_are_not_serialised() {
    let with = PlayerRequest {
        method: "characters.create".into(),
        token: Some("dev-alice".into()),
        api_key: Some("dev-key-client".into()),
        payload: RawValue::from_string(r#"{"name":"hero"}"#.into()).unwrap(),
    };
    let bytes = serde_json::to_vec(&with).unwrap();
    let back: PlayerRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.token.as_deref(), Some("dev-alice"));
    assert_eq!(back.api_key.as_deref(), Some("dev-key-client"));
    assert_eq!(back.payload.get(), r#"{"name":"hero"}"#);

    let without = PlayerRequest {
        method: "leaderboard.top".into(),
        token: None,
        api_key: None,
        payload: RawValue::from_string("null".into()).unwrap(),
    };
    let s = serde_json::to_string(&without).unwrap();
    assert!(!s.contains("token"), "absent token must not be serialised: {s}");
    assert!(!s.contains("api_key"), "absent api_key must not be serialised: {s}");
}

#[test]
fn request_limiter_shares_ip_and_isolates_connections() {
    let limits = PlayerRequestLimits { per_ip_rps: 0.0, per_ip_burst: 2, per_conn_rps: 0.0, per_conn_burst: 1 };
    let disabled = RequestLimiter::new(limits);
    let ip = "127.0.0.1".parse().unwrap();
    let a = disabled.connect(ip);
    assert!(disabled.allow(ip, a.id));
    assert!(disabled.allow(ip, a.id), "zero rate disables that level even with a burst");

    let limited = RequestLimiter::new(PlayerRequestLimits { per_ip_rps: 1.0, per_ip_burst: 2, per_conn_rps: 1.0, per_conn_burst: 1 });
    let a = limited.connect(ip);
    let b = limited.connect(ip);
    assert!(limited.allow(ip, a.id));
    assert!(limited.allow(ip, b.id), "separate connection starts with its own token");
    assert!(!limited.allow(ip, a.id), "shared IP burst and connection burst are exhausted");
}

#[test]
fn accepted_stream_keeps_connection_bucket_alive_after_loop_exits() {
    let limiter = RequestLimiter::new(PlayerRequestLimits {
        per_ip_rps: 0.0,
        per_ip_burst: 0,
        per_conn_rps: 1.0,
        per_conn_burst: 1,
    });
    let ip = "127.0.0.1".parse().unwrap();
    let connection_loop_guard = limiter.connect(ip);

    // This clone is the ownership handed to an accepted stream before its task is
    // scheduled. The connection loop then closes/shuts down while the stream is
    // still gated before admission.
    let accepted_stream_guard = connection_loop_guard.clone();
    let connection_id = accepted_stream_guard.id;
    drop(connection_loop_guard);

    assert!(
        limiter.state.lock().unwrap().conns.contains_key(&connection_id),
        "accepted stream must retain its connection bucket after the loop exits"
    );
    let handler_calls = std::sync::atomic::AtomicUsize::new(0);
    if limiter.allow(ip, connection_id) {
        handler_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    assert_eq!(
        handler_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "released accepted stream must be admitted and run its handler"
    );

    drop(accepted_stream_guard);
    assert!(
        !limiter.state.lock().unwrap().conns.contains_key(&connection_id),
        "the bucket is removed after the final accepted stream finishes"
    );
}

// --- Stream-grace tests (live QUIC over loopback), mirroring server_tests.rs ---
//
// An attacker-chosen keepalive resets the 30s idle timeout, so a peer that opens a
// stream but never completes a frame (or never drains the reply) would pin the
// stream task — its in-flight guard, RequestConnGuard clone and stream slot —
// forever. `serve_stream`/`respond` bound BOTH peer-controlled waits with the
// stream grace; these tests shrink it via the `set_stream_grace` test seam and
// prove the reap by watching `in_flight` — one guard per live connection task plus
// one per live stream task. The codebase never uses tokio paused time (real quinn
// timers don't advance under it) — the shrunk grace is the seam instead.

/// Polls until `in_flight` equals `want` (or fails after 2s) — the accept/reap path
/// runs in background tasks, so the count moves asynchronously.
async fn wait_in_flight(state: &Arc<ShutdownState>, want: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let got = state.in_flight_count();
        if got == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "in_flight never reached {want} (last seen {got})"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A raw quinn client on the player plane (server-cert-only trust): live keepalive
/// (defeats the idle reaper, the attacker's move) + a caller-chosen stream receive
/// window so a test can withhold flow-control credit for the response.
async fn raw_player_conn(
    addr: SocketAddr,
    ca: &DevCA,
    window: u32,
) -> (quinn::Endpoint, quinn::Connection) {
    let qcc = QuicClientConfig::try_from(ca.trust_anchor().client_tls_public().unwrap()).unwrap();
    let mut endpoint = quinn::Endpoint::client(client_bind_addr(addr)).unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_millis(500)));
    transport.stream_receive_window(quinn::VarInt::from_u32(window));
    let mut cfg = quinn::ClientConfig::new(Arc::new(qcc));
    cfg.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(cfg);
    let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    (endpoint, conn)
}

fn player_envelope(method: &str, payload: &str) -> Vec<u8> {
    let req = PlayerRequest {
        method: method.to_string(),
        token: None,
        api_key: None,
        payload: RawValue::from_string(payload.to_string()).unwrap(),
    };
    serde_json::to_vec(&req).unwrap()
}

// The INPUT half is bounded: a peer that opens a bidi stream and sends only a
// partial frame (with a live keepalive holding the connection open) has its stream
// task reaped after the grace — and the CONNECTION survives, so a well-formed call
// on the same connection still succeeds afterwards.
#[tokio::test]
async fn hung_request_read_is_reaped_after_grace() {
    let ca = DevCA::generate().unwrap();
    let mut srv = echo_server();
    srv.set_stream_grace(Duration::from_millis(200));
    let running = srv.listen(loopback(), &ca).unwrap();

    let (_endpoint, conn) = raw_player_conn(running.local_addr(), &ca, 1024).await;
    // One connection task in flight.
    wait_in_flight(&running.shutdown, 1).await;

    // Open a stream and send 2 of the 4 length-prefix bytes, then stall forever.
    let (mut hung_send, _hung_recv) = conn.open_bi().await.unwrap();
    hung_send.write_all(&[0u8, 0]).await.unwrap();
    // The server accepted the stream: conn + stream = 2.
    wait_in_flight(&running.shutdown, 2).await;
    // ...and reaped it after the grace, while the connection stayed up.
    wait_in_flight(&running.shutdown, 1).await;

    // A well-formed call on the SAME connection still works.
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &player_envelope("echo", r#"{"n":1}"#)).await.unwrap();
    send.finish().unwrap();
    let resp_bytes = read_frame_max(&mut recv, MAX_PLAYER_FRAME).await.unwrap();
    let resp: Response = serde_json::from_slice(&resp_bytes).unwrap();
    assert!(resp.ok, "connection must survive a single reaped stream");
    assert_eq!(resp.payload.unwrap().get(), r#"{"n":1}"#);

    conn.close(0u32.into(), b"bye");
    running.close();
}

// The OUTPUT half is bounded: a peer that sends a full request but never grants
// flow-control credit for the response (tiny stream receive window, never reads)
// pins the delivery — with a live keepalive defeating the idle reaper — and is
// reaped after the grace.
#[tokio::test]
async fn undrained_response_is_reaped_after_grace() {
    let ca = DevCA::generate().unwrap();
    // Response far larger than the client's 1 KiB stream receive window below.
    let big = format!("\"{}\"", "a".repeat(256 * 1024)).into_bytes();
    let mut srv = PlayerServer::new();
    srv.set_handler(Arc::new(move |_method, _token, _key, _payload| {
        let big = big.clone();
        Box::pin(async move { Ok(big) })
    }));
    srv.set_stream_grace(Duration::from_millis(200));
    let running = srv.listen(loopback(), &ca).unwrap();

    let (_endpoint, conn) = raw_player_conn(running.local_addr(), &ca, 1024).await;
    wait_in_flight(&running.shutdown, 1).await;

    let (mut send, _recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &player_envelope("big", "null")).await.unwrap();
    send.finish().unwrap();

    // Stream accepted + handler dispatched; the delivery stalls on flow control...
    wait_in_flight(&running.shutdown, 2).await;
    // ...and the grace reaps it while the connection stays alive.
    wait_in_flight(&running.shutdown, 1).await;
    assert!(conn.close_reason().is_none(), "the connection itself must survive the stream reap");

    conn.close(0u32.into(), b"bye");
    running.close();
}

// The rate-denied EARLY-RETURN path is bounded too: its `respond` call site shares
// the one timeout in `respond`, so a denied peer that withholds ALL flow-control
// credit (zero stream receive window — even the tiny denial reply cannot be
// written) is reaped after the grace instead of pinning the stream task on
// `write_frame`/`stopped()`. The handler-call count proves the second stream took
// the denied path, not the dispatch path.
#[tokio::test]
async fn rate_denied_response_is_reaped_after_grace() {
    let ca = DevCA::generate().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    // Per-connection budget of ONE request; refill slow enough (0.01/s) that the
    // second request cannot re-earn a token within the test window.
    let mut srv = PlayerServer::new().with_request_limits(0.0, 0, 0.01, 1);
    srv.set_handler(Arc::new(move |_method, _token, _key, payload| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { Ok(payload) })
    }));
    srv.set_stream_grace(Duration::from_millis(200));
    let running = srv.listen(loopback(), &ca).unwrap();

    // ZERO receive window: the server cannot write even one response byte.
    let (_endpoint, conn) = raw_player_conn(running.local_addr(), &ca, 0).await;
    wait_in_flight(&running.shutdown, 1).await;

    // First request: admitted, dispatched, then its response delivery stalls on the
    // zero window — reaped after the grace (the main-response call site).
    let (mut s1, _r1) = conn.open_bi().await.unwrap();
    write_frame(&mut s1, &player_envelope("echo", r#"{"n":1}"#)).await.unwrap();
    s1.finish().unwrap();
    wait_in_flight(&running.shutdown, 2).await;
    wait_in_flight(&running.shutdown, 1).await;

    // Second request: DENIED by the per-conn bucket; the denial reply also cannot
    // be delivered — the rate-denied call site is reaped after the grace too.
    let (mut s2, _r2) = conn.open_bi().await.unwrap();
    write_frame(&mut s2, &player_envelope("echo", r#"{"n":2}"#)).await.unwrap();
    s2.finish().unwrap();
    wait_in_flight(&running.shutdown, 2).await;
    wait_in_flight(&running.shutdown, 1).await;

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the second stream must have taken the rate-denied path, not dispatch"
    );
    assert!(conn.close_reason().is_none(), "the connection itself must survive both reaps");

    conn.close(0u32.into(), b"bye");
    running.close();
}

// Pin the player plane's timing/size invariants: the production stream grace stays
// at the documented 30s (twin of server_tests::edge_timing_invariants — the two
// planes' consts are deliberately separate, so each plane pins its own).
#[test]
fn player_timing_invariants() {
    assert_eq!(PLAYER_STREAM_GRACE, Duration::from_secs(30));
    assert_eq!(MAX_PLAYER_BIDI_STREAMS, 16);
}

#[tokio::test]
async fn request_denial_is_exact_and_handler_is_not_called() {
    let ca = DevCA::generate().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let srv = PlayerServer::new().with_request_limits(0.0, 0, 1.0, 1);
    srv.set_handler(Arc::new(move |_method, _token, _key, payload| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { Ok(payload) })
    }));
    let running = srv.listen(loopback(), &ca).unwrap();
    let client = PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap();
    assert!(client.call("echo", None, None, br#"{}"#).await.is_ok());
    let err = client.call("echo", None, None, br#"{}"#).await.unwrap_err();
    assert!(err.to_string().contains("player request rate limit exceeded"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    running.close();
}

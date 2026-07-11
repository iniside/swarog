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

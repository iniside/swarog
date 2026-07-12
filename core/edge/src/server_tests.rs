use super::*;
use proptest::prelude::*;

fn tagged_forward(tag: String) -> ForwardHandler {
    Arc::new(move |_method: String, _payload: Vec<u8>| {
        let tag = tag.clone();
        Box::pin(async move { Ok(tag.into_bytes()) }) as BoxFuture<'static, HandlerResult>
    })
}

fn dispatch_with(prefixes: &[String]) -> Dispatch {
    Dispatch {
        handlers: HashMap::new(),
        id_handlers: HashMap::new(),
        prefixes: prefixes.iter().map(|p| (p.clone(), tagged_forward(p.clone()))).collect(),
    }
}

/// `Server::methods()` reports the union of exact + identity registrations, sorted,
/// and excludes prefix registrations (they match by prefix, not method identity).
/// The seam `tools/routecheck` reads a domain module's served set through.
#[test]
fn methods_reports_exact_and_identity_registrations() {
    let mut srv = Server::new();
    assert!(srv.methods().is_empty());
    srv.handle("b.exact", echo_handler());
    srv.handle(
        "a.exact",
        Arc::new(|_payload: Vec<u8>| {
            Box::pin(async { Ok(Vec::new()) }) as BoxFuture<'static, HandlerResult>
        }),
    );
    srv.handle_identity(
        "c.identity",
        Arc::new(|_id: Option<String>, _payload: Vec<u8>| {
            Box::pin(async { Ok(Vec::new()) }) as BoxFuture<'static, HandlerResult>
        }),
    );
    srv.handle_prefix("z.", tagged_forward("z.".to_string()));
    assert_eq!(srv.methods(), vec!["a.exact", "b.exact", "c.identity"]);
}

fn identity_handler() -> IdentityHandler {
    Arc::new(|_id: Option<String>, _payload: Vec<u8>| {
        Box::pin(async { Ok(Vec::new()) }) as BoxFuture<'static, HandlerResult>
    })
}

// --- Uniqueness contract: a duplicate method registration panics at startup ---
// (the same convention as `registry::provide` — never a silent overwrite).

#[test]
#[should_panic(expected = "registered twice")]
fn duplicate_handle_panics() {
    let mut srv = Server::new();
    srv.handle("dup.method", echo_handler());
    srv.handle("dup.method", echo_handler());
}

#[test]
#[should_panic(expected = "registered twice")]
fn duplicate_handle_identity_panics() {
    let mut srv = Server::new();
    srv.handle_identity("dup.method", identity_handler());
    srv.handle_identity("dup.method", identity_handler());
}

// Cross-map collision: a name in `handlers` and `id_handlers` is ALSO a duplicate —
// dispatch precedence would otherwise silently pick the exact handler.
#[test]
#[should_panic(expected = "registered twice")]
fn cross_map_duplicate_panics() {
    let mut srv = Server::new();
    srv.handle("dup.method", echo_handler());
    srv.handle_identity("dup.method", identity_handler());
}

// --- Property test (port of Go's TestPropPrefixLongestMatch in edge/prop_test.go) ---
//
// For any set of distinct registered prefixes and any method string,
// `Dispatch::longest_prefix` matches iff some registered prefix is a
// `str::starts_with` of the method, and when it matches it selects the LONGEST
// such prefix. An oracle loop computes the expected result independently; each
// handler is tagged with its own prefix so the winner is identifiable.
proptest! {
    #[test]
    fn prop_prefix_longest_match(
        prefixes in proptest::collection::hash_set("[a-z]{1,5}\\.", 0..8),
        use_registered in any::<bool>(),
        chosen_idx in 0usize..8,
        suffix in "[a-z.]{0,6}",
        random_method in "[a-z.]{0,12}",
    ) {
        let prefixes: Vec<String> = prefixes.into_iter().collect();
        let dispatch = dispatch_with(&prefixes);

        let method = if !prefixes.is_empty() && use_registered {
            let chosen = &prefixes[chosen_idx % prefixes.len()];
            format!("{chosen}{suffix}")
        } else {
            random_method
        };

        // Oracle: the longest registered prefix that `method` starts with.
        let mut best_len: isize = -1;
        let mut best_tag: Option<&str> = None;
        for p in &prefixes {
            if method.starts_with(p.as_str()) && p.len() as isize > best_len {
                best_len = p.len() as isize;
                best_tag = Some(p.as_str());
            }
        }
        let want_ok = best_len >= 0;

        let got = dispatch.longest_prefix(&method);
        prop_assert_eq!(got.is_some(), want_ok);

        if let Some(fwd) = got {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let got_tag = rt.block_on(fwd(method.clone(), Vec::new())).unwrap();
            prop_assert_eq!(String::from_utf8(got_tag).unwrap(), best_tag.unwrap().to_string());
        }
    }
}

// --- Stream-grace + explicit TransportConfig tests (live QUIC over loopback) ---
//
// The internal client's 5s keepalive keeps a connection's transport alive even when
// the application layer hangs, so the 30s idle timeout never rescues a stuck
// stream. `serve_stream` therefore bounds BOTH peer-controlled waits (request read,
// response delivery) with the stream grace; these tests shrink it via the
// `set_stream_grace` test seam and prove the reap by watching `in_flight` — one
// guard per live connection task plus one per live stream task.

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn echo_handler() -> Handler {
    Arc::new(|payload| Box::pin(async move { Ok(payload) }) as BoxFuture<'static, HandlerResult>)
}

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

// The INPUT half is bounded: a peer that opens a bidi stream and sends only a
// partial frame (with a live keepalive holding the connection open) has its stream
// task reaped after the grace — and the CONNECTION survives, so a well-formed call
// on the same connection still succeeds afterwards.
#[tokio::test]
async fn hung_request_read_is_reaped_after_grace() {
    let ca = DevCA::generate().unwrap();
    let mut srv = Server::new();
    srv.handle("echo", echo_handler());
    srv.set_stream_grace(Duration::from_millis(200));
    let running = srv.listen(loopback(), &ca).unwrap();

    // `Client::dial` configures the production 5s keepalive.
    let client = crate::Client::dial(running.local_addr(), &ca).await.unwrap();
    // One connection task in flight.
    wait_in_flight(&running.shutdown, 1).await;

    // Open a stream and send 2 of the 4 length-prefix bytes, then stall forever.
    let (mut hung_send, _hung_recv) = client.connection().open_bi().await.unwrap();
    hung_send.write_all(&[0u8, 0]).await.unwrap();
    // The server accepted the stream: conn + stream = 2.
    wait_in_flight(&running.shutdown, 2).await;
    // ...and reaped it after the grace, while the connection stayed up.
    wait_in_flight(&running.shutdown, 1).await;

    let resp = client.call_raw("echo", br#"{"n":1}"#).await.unwrap();
    assert_eq!(resp, br#"{"n":1}"#, "connection must survive a single reaped stream");

    client.close();
    running.close();
}

// The OUTPUT half is bounded: a peer that sends a full request but never grants
// flow-control credit for the response (tiny stream receive window, never reads)
// pins the delivery — with a live keepalive — and is reaped after the grace.
#[tokio::test]
async fn undrained_response_is_reaped_after_grace() {
    let ca = DevCA::generate().unwrap();
    let mut srv = Server::new();
    // Response far larger than the client's stream receive window below.
    let big = format!("\"{}\"", "a".repeat(256 * 1024)).into_bytes();
    srv.handle(
        "big",
        Arc::new(move |_payload| {
            let big = big.clone();
            Box::pin(async move { Ok(big) }) as BoxFuture<'static, HandlerResult>
        }),
    );
    srv.set_stream_grace(Duration::from_millis(200));
    let running = srv.listen(loopback(), &ca).unwrap();
    let addr = running.local_addr();

    // A raw quinn client (the frame-layer `Client` always drains responses): live
    // keepalive + a 1 KiB stream receive window, and it NEVER reads the response.
    let qcc =
        quinn::crypto::rustls::QuicClientConfig::try_from(ca.client_tls().unwrap()).unwrap();
    let mut endpoint = quinn::Endpoint::client(crate::tls::client_bind_addr(addr)).unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_millis(500)));
    transport.stream_receive_window(quinn::VarInt::from_u32(1024));
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(qcc));
    client_cfg.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(client_cfg);
    let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    wait_in_flight(&running.shutdown, 1).await;

    let req = Request {
        method: "big".to_string(),
        identity: None,
        payload: serde_json::value::RawValue::from_string("null".to_string()).unwrap(),
    };
    let env = serde_json::to_vec(&req).unwrap();
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &env).await.unwrap();
    send.finish().unwrap();

    // Stream accepted + handler dispatched; the delivery stalls on flow control...
    wait_in_flight(&running.shutdown, 2).await;
    // ...and the grace reaps it while the connection stays alive.
    wait_in_flight(&running.shutdown, 1).await;
    assert!(conn.close_reason().is_none(), "the connection itself must survive the stream reap");

    conn.close(0u32.into(), b"bye");
    running.close();
}

// The explicit TransportConfig caps concurrent bidi streams at MAX_EDGE_BIDI_STREAMS
// (16): with 16 handlers parked mid-dispatch (each holding its stream), a 17th call
// cannot even open a stream until one completes. This also proves the MIDDLE of
// serve_stream (the dispatch) is NOT grace-bounded — the parked handlers survive.
#[tokio::test]
async fn transport_config_caps_concurrent_bidi_streams() {
    let ca = DevCA::generate().unwrap();
    let (release, parked) = tokio::sync::watch::channel(false);
    let mut srv = Server::new();
    srv.handle(
        "park",
        Arc::new(move |payload| {
            let mut parked = parked.clone();
            Box::pin(async move {
                let _ = parked.wait_for(|v| *v).await;
                Ok(payload)
            }) as BoxFuture<'static, HandlerResult>
        }),
    );
    let running = srv.listen(loopback(), &ca).unwrap();
    let client = Arc::new(crate::Client::dial(running.local_addr(), &ca).await.unwrap());

    let calls: Vec<_> = (0..MAX_EDGE_BIDI_STREAMS)
        .map(|i| {
            let client = client.clone();
            tokio::spawn(async move { client.call_raw("park", format!("{i}").as_bytes()).await })
        })
        .collect();
    // Connection task + 16 parked stream tasks.
    wait_in_flight(&running.shutdown, 1 + MAX_EDGE_BIDI_STREAMS as usize).await;

    // The 17th call cannot open a stream while all 16 slots are parked.
    let mut extra = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("park", b"\"extra\"").await }
    });
    let blocked = tokio::time::timeout(Duration::from_millis(300), &mut extra).await;
    assert!(blocked.is_err(), "17th concurrent stream must block on the bidi cap");

    // Release the handlers: every call (the 17th included) completes.
    release.send(true).unwrap();
    for (i, call) in calls.into_iter().enumerate() {
        let resp = call.await.unwrap().unwrap();
        assert_eq!(resp, format!("{i}").into_bytes());
    }
    let resp = extra.await.unwrap().unwrap();
    assert_eq!(resp, b"\"extra\"");

    client.close();
    running.close();
}

// Pin the timing invariants the two planes depend on: the connection idle timeout
// must exceed the client keepalive (so keepalive keeps a live conn open) and the
// production stream grace stays at the documented 30s.
#[test]
fn edge_timing_invariants() {
    assert!(
        Duration::from_millis(EDGE_IDLE_TIMEOUT_MS as u64) > crate::client::KEEPALIVE_INTERVAL,
        "idle timeout must exceed the client keepalive or live connections get reaped"
    );
    assert_eq!(EDGE_STREAM_GRACE, Duration::from_secs(30));
    assert_eq!(MAX_EDGE_BIDI_STREAMS, 16);
}

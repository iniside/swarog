//! `PushClient::invoke` buffering, against a local scripted server (no fleet): the server
//! writes frames in a fixed order, so what the client buffers is decided by the script, not
//! by timing.

use super::*;

/// A hang guard for every client read, not a latency budget.
const BUDGET: Duration = Duration::from_secs(10);

/// Serves ONE `/push`-shaped connection: reads the client's first frame (the invoke), then
/// writes `script(invoke_id)` in order and holds the socket open until the client leaves,
/// so a `None` from the client can only mean the script ended it.
async fn scripted_server(
    script: impl FnOnce(i64) -> Vec<String> + Send + 'static,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("upgrade");
        let first = loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => break text,
                Some(Ok(_)) => continue,
                other => panic!("the client sent no invoke: {other:?}"),
            }
        };
        let invoke: serde_json::Value = serde_json::from_str(first.as_str()).expect("JSON invoke");
        assert_eq!(invoke["type"], "invoke");
        let id = invoke["id"].as_i64().expect("a numeric id");
        for frame in script(id) {
            ws.send(WsMessage::Text(frame)).await.expect("server write");
        }
        while ws.next().await.is_some() {}
    });
    (addr, task)
}

fn result_frame(id: i64, marker: &str) -> String {
    serde_json::json!({"type":"result","id":id,"response":{"status":"Ok","marker":marker}})
        .to_string()
}

/// The branch that makes `invoke` safe next to a push stream: a `message` and a result for a
/// DIFFERENT id read while waiting go to `buffered` (the `Some(frame) => push_back` arm, and
/// the `got == id` guard falling through), the matching result returns, and `next_frame`
/// then replays the buffer in arrival order. A client that dropped non-matching frames, or
/// matched on the first result, fails an assertion below.
#[tokio::test]
async fn invoke_buffers_foreign_frames_in_order_and_returns_the_matching_result() {
    let (addr, server) = scripted_server(|id| {
        vec![
            serde_json::json!({
                "type":"message","hub":"party","topic":"party.ping",
                "payload": base64::engine::general_purpose::STANDARD.encode(b"hi"),
            })
            .to_string(),
            result_frame(id + 1000, "foreign"),
            result_frame(id, "mine"),
        ]
    })
    .await;

    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    let response = client
        .invoke(7, "demo.echo", serde_json::json!({}), BUDGET)
        .await
        .expect("the matching result");
    assert_eq!(response["marker"], "mine", "the result with the invoke's own id");

    assert_eq!(
        client.next_frame(BUDGET).await.expect("buffered frame"),
        Some(Frame::Message {
            hub: "party".into(),
            topic: "party.ping".into(),
            payload: b"hi".to_vec(),
        }),
        "the push message read first is replayed first"
    );
    assert_eq!(
        client.next_frame(BUDGET).await.expect("buffered frame"),
        Some(Frame::Result {
            id: 1007,
            response: serde_json::json!({"status":"Ok","marker":"foreign"}),
        }),
        "the foreign result is buffered, not mistaken for ours and not dropped"
    );

    client.disconnect().await;
    server.await.expect("server task");
}

/// A typed close before any result: `invoke` fails (the `Frame::Close` arm), and the close is
/// kept in the buffer so the caller can still dispatch on its code with `next_frame`. A
/// client that only bailed would lose the code the proof asserts on.
#[tokio::test]
async fn invoke_fails_on_a_typed_close_and_keeps_it_for_next_frame() {
    let (addr, server) = scripted_server(|_id| {
        vec![serde_json::json!({"type":"close","code":"capacity","retryable":true}).to_string()]
    })
    .await;

    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    let err = client
        .invoke(1, "demo.echo", serde_json::json!({}), BUDGET)
        .await
        .expect_err("a close before the result is an error");
    assert!(err.to_string().contains("closed before the result"), "{err}");

    assert_eq!(
        client.next_frame(BUDGET).await.expect("buffered close"),
        Some(Frame::Close { code: "capacity".into(), retryable: true }),
    );

    client.disconnect().await;
    server.await.expect("server task");
}

/// Reads `expect` client frames, writes `script` in order, then either closes the socket
/// (`hold_open == false`, so any further client read ends with no frame) or holds it open
/// until the client leaves. Returns every client frame received, in order.
async fn pipelined_server(
    expect: usize,
    script: Vec<String>,
    hold_open: bool,
) -> (SocketAddr, tokio::task::JoinHandle<Vec<serde_json::Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("upgrade");
        let mut received = Vec::new();
        while received.len() < expect {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    received.push(serde_json::from_str(text.as_str()).expect("JSON frame"))
                }
                Some(Ok(_)) => continue,
                other => panic!("the client sent too few frames: {other:?}"),
            }
        }
        for frame in script {
            ws.send(WsMessage::Text(frame)).await.expect("server write");
        }
        if hold_open {
            while ws.next().await.is_some() {}
        } else {
            let _ = ws.close(None).await;
        }
        received
    });
    (addr, task)
}

fn close_frame() -> String {
    serde_json::json!({"type":"close","code":"capacity","retryable":true}).to_string()
}

fn ok(marker: &str) -> serde_json::Value {
    serde_json::json!({"status":"Ok","marker":marker})
}

/// `await_result` scans `buffered` before reading: with the server gone after its two
/// frames, the second call can only succeed from the buffer.
#[tokio::test]
async fn out_of_order_results_resolve_the_later_id_from_the_buffer() {
    let (addr, server) =
        pipelined_server(2, vec![result_frame(2, "two"), result_frame(1, "one")], false).await;
    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    client.send_invoke(1, "a.one", serde_json::json!({})).await.expect("send 1");
    client.send_invoke(2, "a.two", serde_json::json!({})).await.expect("send 2");

    assert_eq!(client.await_result(1, BUDGET).await.expect("result 1"), ok("one"));
    assert_eq!(client.buffered.len(), 1, "result 2 was read past and buffered");
    server.await.expect("server task");
    assert_eq!(
        client.await_result(2, BUDGET).await.expect("result 2 from the buffer"),
        ok("two")
    );
    assert!(client.buffered.is_empty());
}

/// A result that arrived before a close is still deliverable after the close is buffered;
/// an unrelated id fails on the close without consuming either result.
#[tokio::test]
async fn results_before_a_close_stay_deliverable_after_another_id_fails() {
    let (addr, server) = pipelined_server(
        1,
        vec![result_frame(4, "four"), result_frame(3, "three"), close_frame()],
        true,
    )
    .await;
    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    client.send_invoke(5, "a.five", serde_json::json!({})).await.expect("send");

    let err = client.await_result(5, BUDGET).await.expect_err("the close fails id 5");
    assert!(err.to_string().contains("closed before the result"), "{err}");
    assert!(
        matches!(client.buffered.back(), Some(Frame::Close { .. })),
        "the Close stays buffered after the failure"
    );
    assert_eq!(client.await_result(4, BUDGET).await.expect("result 4"), ok("four"));
    assert_eq!(client.await_result(3, BUDGET).await.expect("result 3"), ok("three"));

    client.disconnect().await;
    server.await.expect("server task");
}

/// A `[close, result]` buffer cannot arise from the client today (nothing is buffered after a
/// Close); this guards the arrival-order scan in case that rule regresses.
#[tokio::test]
async fn a_close_ahead_of_a_result_fails_the_await_and_stays_buffered() {
    let (addr, server) = pipelined_server(1, vec![], true).await;
    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    client.send_invoke(7, "a.seven", serde_json::json!({})).await.expect("send");
    let close = Frame::Close { code: "capacity".into(), retryable: true };
    let result = Frame::Result { id: 7, response: ok("seven") };
    client.buffered.push_back(close.clone());
    client.buffered.push_back(result.clone());

    let err = client.await_result(7, BUDGET).await.expect_err("close first fails");
    assert!(err.to_string().contains("closed before the result"), "{err}");
    assert_eq!(client.next_frame(BUDGET).await.expect("close"), Some(close));
    assert_eq!(client.next_frame(BUDGET).await.expect("result"), Some(result));

    client.disconnect().await;
    server.await.expect("server task");
}

/// Result 99 sits unread on the socket behind the Close: any socket read would return it as
/// `Ok`, so an `Err` with the Close still buffered proves the buffered Close decided.
#[tokio::test]
async fn a_buffered_close_fails_an_unmatched_await_without_reading() {
    let (addr, server) = pipelined_server(1, vec![close_frame(), result_frame(99, "late")], true).await;
    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    client.send_invoke(1, "a.one", serde_json::json!({})).await.expect("send");
    client.await_result(1, BUDGET).await.expect_err("the close fails id 1");
    assert_eq!(client.buffered.len(), 1);

    let err = client.await_result(99, BUDGET).await.expect_err("buffered close fails id 99");
    let text = err.to_string();
    assert!(text.contains("closed before the result of invoke 99"), "{text}");
    assert!(!text.contains("no frame within"), "{text}");
    assert_eq!(client.buffered.len(), 1, "the Close stays buffered and nothing was read");

    client.disconnect().await;
    server.await.expect("server task");
}

/// `send_invoke` writes without waiting: three frames reach the server in send order with
/// their own ids, methods and payloads.
#[tokio::test]
async fn send_invoke_pipelines_frames_in_send_order() {
    let (addr, server) = pipelined_server(3, vec![], true).await;
    let mut client = PushClient::connect(&format!("ws://{addr}"), "tok", "key")
        .await
        .expect("connect");
    tokio::time::timeout(BUDGET, async {
        client.send_invoke(11, "a.first", serde_json::json!({"n":1})).await.expect("send");
        client.send_invoke(12, "b.second", serde_json::json!({"n":2})).await.expect("send");
        client.send_invoke(13, "c.third", serde_json::json!({"n":3})).await.expect("send");
    })
    .await
    .expect("send_invoke must not wait for a result");
    client.disconnect().await;

    let received = server.await.expect("server task");
    let got: Vec<(String, i64, String, i64)> = received
        .iter()
        .map(|f| {
            (
                f["type"].as_str().unwrap().to_string(),
                f["id"].as_i64().unwrap(),
                f["method"].as_str().unwrap().to_string(),
                f["payload"]["n"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("invoke".into(), 11, "a.first".into(), 1),
            ("invoke".into(), 12, "b.second".into(), 2),
            ("invoke".into(), 13, "c.third".into(), 3),
        ]
    );
}

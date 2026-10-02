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

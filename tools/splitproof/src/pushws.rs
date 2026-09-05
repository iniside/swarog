//! The `/push` WebSocket client the `[PH*]` assertions drive, plus the backplane
//! injector one of them addresses a group with.
//!
//! The client is deliberately thin: it speaks the exact frames
//! `modules/gateway`'s `ServerFrame`/`ClientFrame` serialize, so an assertion reads the
//! same JSON a real client would and a rename on either side surfaces as a failed
//! assertion rather than a silently ignored frame.
//!
//! Every read is bounded by a caller-supplied deadline and every wait is a poll on a
//! FRAME, never a sleep: a push message has no id and no redelivery, so a proof that
//! slept and then looked would be racing the socket rather than observing it.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use edge::DevCA;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// One server→client frame, in the shape the front serializes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// The handshake succeeded and the front minted this connection id.
    Ack { connection_id: u64 },
    /// A delivered `push::Message`; `payload` is the producer's bytes, base64-decoded.
    Message { topic: String, payload: Vec<u8> },
    /// The typed close, carrying the code a client dispatches on.
    Close { code: String, retryable: bool },
    /// Anything this harness does not model — kept whole so a failure message can show it.
    Other(String),
}

impl Frame {
    pub fn topic(&self) -> Option<&str> {
        match self {
            Frame::Message { topic, .. } => Some(topic.as_str()),
            _ => None,
        }
    }

    /// The `player_id`/`online` pair of a `push.presence` payload.
    pub fn presence(&self) -> Option<(String, bool)> {
        let Frame::Message { topic, payload } = self else {
            return None;
        };
        if topic != PRESENCE_TOPIC {
            return None;
        }
        let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
        Some((
            value.get("player_id")?.as_str()?.to_string(),
            value.get("online")?.as_bool()?,
        ))
    }

    fn parse(text: &str) -> Frame {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return Frame::Other(text.to_string());
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("ack") => match value.get("connection_id").and_then(|v| v.as_u64()) {
                Some(connection_id) => Frame::Ack { connection_id },
                None => Frame::Other(text.to_string()),
            },
            Some("message") => {
                let topic = value.get("topic").and_then(|v| v.as_str()).unwrap_or_default();
                let encoded = value.get("payload").and_then(|v| v.as_str()).unwrap_or_default();
                match base64::engine::general_purpose::STANDARD.decode(encoded) {
                    Ok(payload) => Frame::Message {
                        topic: topic.to_string(),
                        payload,
                    },
                    Err(_) => Frame::Other(text.to_string()),
                }
            }
            Some("close") => Frame::Close {
                code: value
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                retryable: value
                    .get("retryable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or_default(),
            },
            _ => Frame::Other(text.to_string()),
        }
    }
}

/// The topic the front publishes a presence transition under (`push_ws::PRESENCE_TOPIC`,
/// which is private to the module — spelled here because it reaches a client as the
/// frame's dispatch key).
pub const PRESENCE_TOPIC: &str = "push.presence";

/// One live `/push` connection.
pub struct PushClient {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl PushClient {
    /// Dials `GET /push` on `ws_base` presenting the credentials in HEADERS — the path a
    /// native client takes. The hello-frame path exists for browsers, which cannot set
    /// headers on a WebSocket dial; both funnel into the same `handshake`.
    pub async fn connect(ws_base: &str, token: &str, api_key: &str) -> Result<PushClient> {
        let mut request = format!("{ws_base}/push")
            .into_client_request()
            .context("build the /push upgrade request")?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).context("bearer header")?,
        );
        request.headers_mut().insert(
            "x-api-key",
            HeaderValue::from_str(api_key).context("api key header")?,
        );
        let (stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .context("upgrade GET /push")?;
        Ok(PushClient { stream })
    }

    /// The next frame the front sends, within `budget`. `Ok(None)` means the socket ended
    /// (a close or a drop) and `Err` means the budget elapsed with the socket still open —
    /// the two are different findings, so they are different answers.
    pub async fn next_frame(&mut self, budget: Duration) -> Result<Option<Frame>> {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no frame within {budget:?}");
            }
            match tokio::time::timeout(left, self.stream.next()).await {
                Err(_elapsed) => bail!("no frame within {budget:?}"),
                Ok(None) => return Ok(None),
                Ok(Some(Err(e))) => return Err(anyhow::anyhow!("websocket read: {e}")),
                Ok(Some(Ok(WsMessage::Text(text)))) => return Ok(Some(Frame::parse(text.as_str()))),
                Ok(Some(Ok(WsMessage::Binary(bytes)))) => {
                    return Ok(Some(Frame::parse(&String::from_utf8_lossy(&bytes))))
                }
                Ok(Some(Ok(WsMessage::Close(_)))) => return Ok(None),
                // Ping/Pong/raw: tungstenite answers a ping itself; neither is a hub frame.
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// The first frame satisfying `want` within `budget`, skipping every other one.
    ///
    /// Skipping matters: with `PUSH_PRESENCE=1` the front broadcasts every bind and
    /// departure on the fleet to every bound connection, so an assertion that demanded
    /// its message be the NEXT frame would fail on an unrelated connection elsewhere.
    pub async fn await_frame(
        &mut self,
        budget: Duration,
        want: impl Fn(&Frame) -> bool,
    ) -> Option<Frame> {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.next_frame(left).await {
                Ok(Some(frame)) => {
                    if want(&frame) {
                        return Some(frame);
                    }
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// The first `push::Message` on `topic` within `budget`.
    pub async fn await_topic(&mut self, topic: &str, budget: Duration) -> Option<Frame> {
        self.await_frame(budget, |frame| frame.topic() == Some(topic))
            .await
    }

    /// The first `push.presence` transition for `player` within `budget`.
    pub async fn await_presence(
        &mut self,
        player: &str,
        online: bool,
        budget: Duration,
    ) -> bool {
        self.await_frame(budget, |frame| {
            frame.presence().as_ref().is_some_and(|(id, up)| id == player && *up == online)
        })
        .await
        .is_some()
    }

    /// Sends one client→server verb frame (`join`/`leave`).
    pub async fn send(&mut self, frame: serde_json::Value) -> Result<()> {
        self.stream
            .send(WsMessage::Text(frame.to_string()))
            .await
            .context("write a client frame")
    }

    /// Joins `group`, addressable as `push::Target::Group`. The verb is silent by design
    /// (there is no join ack), so a caller proves it landed by receiving a group-addressed
    /// message, never by assuming it was applied.
    pub async fn join(&mut self, group: &str) -> Result<()> {
        self.send(serde_json::json!({ "type": "join", "group": group })).await
    }

    /// Closes the socket cleanly, so the front sees a close rather than a reset.
    pub async fn disconnect(mut self) {
        let _ = self.stream.close(None).await;
    }
}

/// Injects one ORDERED batch straight into the front's inbound backplane face
/// (`push.deliver` on its internal mTLS edge), as `core/remote`'s sender does.
///
/// This is the only way to address a `Target::Group` from outside the front: no module
/// produces a group-addressed message (groups are a client-driven audience), so the
/// resolution path would otherwise ship with nothing executing it. The batch travels the
/// production codec, the production wire method and the production handler — only the
/// producer is the harness.
pub async fn deliver_batch(
    edge_addr: SocketAddr,
    ca_cert: &str,
    ca_key: &str,
    batch: &[push::Envelope],
) -> Result<()> {
    let ca = DevCA::load(ca_cert, ca_key).map_err(|e| anyhow::anyhow!("load edge CA: {e}"))?;
    let client = edge::Client::dial(edge_addr, &ca)
        .await
        .map_err(|e| anyhow::anyhow!("dial the front's internal edge at {edge_addr}: {e}"))?;
    let encoded = push::encode_batch(batch).map_err(|e| anyhow::anyhow!("encode batch: {e}"))?;
    client
        .call_raw("push.deliver", &encoded)
        .await
        .map_err(|e| anyhow::anyhow!("push.deliver: {e}"))?;
    client.close();
    Ok(())
}

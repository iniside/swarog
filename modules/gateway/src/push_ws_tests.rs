//! Tests for the `/push` WebSocket hub (`push_ws.rs`).
//!
//! Two harnesses, chosen per branch:
//!
//! * the **hub directly** (`PushHub::accept`/`bind`/`deliver`/`shutdown` and its `Slot`
//!   guard) for everything decided under the registry lock — caps, group membership,
//!   presence transitions, drop-oldest, the shutdown abort collection. These are the
//!   branches a socket cannot make deterministic: a disconnect racing a reconnect is a
//!   lock-ordering property, not a network one;
//! * a **real bound listener** for everything that needs the upgrade. The rest of this
//!   crate's suite drives `oneshot`, whose requests carry no `hyper::upgrade::OnUpgrade`
//!   extension, so `WebSocketUpgrade::from_request_parts` fails `ConnectionNotUpgradable`
//!   there and NO upgrade can happen at all — the handshake, the typed close, the
//!   re-verify tick and the per-IP cap are only reachable over a socket.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use apikeysapi::KeyRecord;
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as ClientMessage;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;

use super::*;
use crate::keys::{check_api_key, KeyVerifier, LookupUnavailable};
use crate::verifier::{DevSessionVerifier, SessionVerifier, VerifyUnavailable};
use crate::Slots;

/// Every socket assertion is bounded by this. It is a HANG GUARD, not a latency budget:
/// the assertions are on what arrives, never on how fast.
const GUARD: Duration = Duration::from_secs(10);

const KEY: &str = "test-key";
const POLICYLESS_KEY: &str = "policyless-key";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Bounds small enough to reach every cap in a test, with deadlines far enough out that
/// nothing here races a real clock. Tests that need a short deadline shorten exactly the
/// one they assert on.
fn test_limits() -> PushLimits {
    PushLimits {
        max_connections: 16,
        max_per_ip: 16,
        max_per_player: 2,
        queue_depth: 8,
        max_frame_bytes: 16 * 1024,
        handshake_grace: Duration::from_secs(30),
        write_deadline: Duration::from_secs(10),
        reverify_interval: Duration::from_secs(3600),
        max_stale: Duration::from_secs(3600),
        trusted_proxies: Vec::new(),
        presence: false,
        ..PushLimits::default()
    }
}

fn hub_with(limits: PushLimits) -> Arc<PushHub> {
    Arc::new(PushHub::new(limits))
}

fn local() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

/// Accepts a connection, panicking on a refusal the test did not intend.
fn accept(hub: &Arc<PushHub>, ip: IpAddr) -> (Slot, mpsc::Receiver<()>) {
    match hub.accept(ip) {
        Ok(accepted) => accepted,
        Err(_) => panic!("the hub refused a connection this test expected it to accept"),
    }
}

/// Accepts AND binds — the shape every addressable connection has after its handshake.
fn bound(hub: &Arc<PushHub>, ip: IpAddr, player: &str) -> (Slot, mpsc::Receiver<()>) {
    let (slot, wake) = accept(hub, ip);
    hub.bind(slot.id, player).expect("a freshly accepted connection binds");
    (slot, wake)
}

/// The frames sitting in a connection's queue, WITHOUT consuming them, in the order `recv`
/// would yield them: the reply lane first, then event lanes round-robin from the cursor.
/// A synchronous read under the queue's own mutex, so no test has to await a connection
/// task that does not exist.
fn queued(queue: &ConnQueue) -> Vec<String> {
    let state = queue.state.lock().unwrap();
    let mut out: Vec<String> = state
        .reply
        .iter()
        .map(|reply| match reply {
            Reply::Frame(text) | Reply::Rejection(text) | Reply::Result(text, _) => {
                text.to_string()
            }
        })
        .collect();
    let mut lanes: Vec<VecDeque<Arc<str>>> =
        state.events.iter().map(|(_, frames)| frames.clone()).collect();
    let count = lanes.len();
    let mut next = state.next;
    while let Some(lane) =
        (0..count).map(|o| (next + o) % count).find(|l| !lanes[*l].is_empty())
    {
        out.push(lanes[lane].pop_front().expect("lane is non-empty").to_string());
        next = (lane + 1) % count;
    }
    out
}

/// The hub name and length of every event lane, in lane order (index 0 is always `""`).
fn lane_lens(queue: &ConnQueue) -> Vec<(String, usize)> {
    let state = queue.state.lock().unwrap();
    state.events.iter().map(|(name, frames)| (name.to_string(), frames.len())).collect()
}

/// The decoded payloads of the frames in event lane `hub`, front (next served) first.
fn lane_payloads(queue: &ConnQueue, hub: &str) -> Vec<String> {
    let state = queue.state.lock().unwrap();
    let (_, frames) = state
        .events
        .iter()
        .find(|(name, _)| &**name == hub)
        .unwrap_or_else(|| panic!("no lane {hub:?}"));
    frames
        .iter()
        .map(|frame| {
            let v: serde_json::Value = serde_json::from_str(frame).expect("a frame is JSON");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(v["payload"].as_str().expect("a message frame"))
                .expect("payload is base64");
            String::from_utf8(bytes).expect("test payloads are utf-8")
        })
        .collect()
}

/// Total frames queued across the event lanes (excludes the reply lane).
fn event_total(queue: &ConnQueue) -> usize {
    lane_lens(queue).iter().map(|(_, n)| n).sum()
}

fn queued_close(queue: &ConnQueue) -> Option<CloseCode> {
    queue.state.lock().unwrap().close
}

/// The `topic` of every message frame in a queue, in queue order.
fn queued_topics(queue: &ConnQueue) -> Vec<String> {
    queued(queue)
        .iter()
        .map(|frame| {
            let v: serde_json::Value = serde_json::from_str(frame).expect("a frame is JSON");
            v.get("topic")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The decoded payloads of every `message` frame in a queue, in queue order.
fn queued_payloads(queue: &ConnQueue) -> Vec<String> {
    queued(queue)
        .iter()
        .filter_map(|frame| {
            let v: serde_json::Value = serde_json::from_str(frame).expect("a frame is JSON");
            let payload = v.get("payload")?.as_str()?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .expect("payload is base64");
            Some(String::from_utf8(bytes).expect("test payloads are utf-8"))
        })
        .collect()
}

fn msg(payload: &str) -> Message {
    Message::new("test.topic", payload.as_bytes().to_vec())
}

/// A [`KeyVerifier`] over a fixed key → policy map (no store, no TTL cache).
struct FakeKeys {
    keys: HashMap<String, String>,
}

impl FakeKeys {
    fn demo() -> Arc<dyn KeyVerifier> {
        let mut keys = HashMap::new();
        keys.insert(KEY.to_string(), "full".to_string());
        // A key whose policy names NOTHING: valid, present, and allowed to hold a
        // `/push` socket while every operation refuses it.
        keys.insert(POLICYLESS_KEY.to_string(), String::new());
        Arc::new(FakeKeys { keys })
    }
}

#[async_trait::async_trait]
impl KeyVerifier for FakeKeys {
    async fn lookup(&self, key: &str) -> Result<Option<KeyRecord>, LookupUnavailable> {
        Ok(self
            .keys
            .get(key)
            .map(|policy| KeyRecord { name: key.to_string(), policy: policy.clone() }))
    }
}

/// A key verifier that can only fail — the outage arm of the key check.
struct UnavailableKeys;

#[async_trait::async_trait]
impl KeyVerifier for UnavailableKeys {
    async fn lookup(&self, _key: &str) -> Result<Option<KeyRecord>, LookupUnavailable> {
        Err(LookupUnavailable)
    }
}

/// A key verifier that never answers — the hung-backend shape the admission budget bounds.
struct HungKeys;

#[async_trait::async_trait]
impl KeyVerifier for HungKeys {
    async fn lookup(&self, _key: &str) -> Result<Option<KeyRecord>, LookupUnavailable> {
        std::future::pending().await
    }
}

/// A session verifier that can only fail — the accounts-outage arm.
struct UnavailableSessions;

#[async_trait::async_trait]
impl SessionVerifier for UnavailableSessions {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        Err(VerifyUnavailable)
    }
}

/// A session verifier whose FIRST answer admits `player` and whose later answers are
/// `later` — the re-verify fixture. It counts its calls, so a test can prove the tick
/// actually ran rather than inferring it from a close.
struct RevokingSessions {
    player: String,
    later: fn(&str) -> Result<Option<String>, VerifyUnavailable>,
    calls: AtomicUsize,
}

impl RevokingSessions {
    fn new(
        player: &str,
        later: fn(&str) -> Result<Option<String>, VerifyUnavailable>,
    ) -> Arc<RevokingSessions> {
        Arc::new(RevokingSessions {
            player: player.to_string(),
            later,
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl SessionVerifier for RevokingSessions {
    async fn verify(&self, token: &str) -> Result<Option<String>, VerifyUnavailable> {
        if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            return Ok(Some(self.player.clone()));
        }
        (self.later)(token)
    }
}

/// A front door with no operations at all: `/push` is a FIXED route, so nothing in the
/// route table is involved in any assertion here.
fn front_with(
    limits: PushLimits,
    verifier: Arc<dyn SessionVerifier>,
    keys: Arc<dyn KeyVerifier>,
) -> Arc<FrontDoor> {
    Arc::new(
        FrontDoor::new(Arc::new(Slots::new()), verifier, keys, Vec::new())
            .with_push_limits(limits),
    )
}

fn dev_front(limits: PushLimits) -> Arc<FrontDoor> {
    front_with(limits, Arc::new(DevSessionVerifier::new()), FakeKeys::demo())
}

/// Serves `front`'s router on an ephemeral loopback port with connection info wired,
/// exactly as `app::run` does — the per-IP cap resolves a client address, so a harness
/// without `ConnectInfo` would collapse every dial into one bucket.
async fn serve(front: &Arc<FrontDoor>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let router = front.router();
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    (addr, task)
}

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Dials `/push` with the given headers. `Err` carries the refusal's HTTP status — the
/// pre-upgrade path, which never becomes a WebSocket at all.
async fn dial(addr: SocketAddr, headers: &[(&str, &str)]) -> Result<Client, u16> {
    let mut req = format!("ws://{addr}/push")
        .into_client_request()
        .expect("a well-formed ws request");
    for (name, value) in headers {
        req.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    match tokio::time::timeout(GUARD, tokio_tungstenite::connect_async(req)).await {
        Ok(Ok((ws, _resp))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => Err(resp.status().as_u16()),
        Ok(Err(e)) => panic!("dial failed for a reason this test does not model: {e}"),
        Err(_) => panic!("the dial did not settle within the hang guard"),
    }
}

/// Dials with a full header credential set (the browser-less shape).
async fn dial_authed(addr: SocketAddr, token: &str, key: &str) -> Client {
    dial(addr, &[("authorization", &format!("Bearer {token}")), ("x-api-key", key)])
        .await
        .unwrap_or_else(|status| panic!("dial refused with HTTP {status}"))
}

/// The next TEXT frame, skipping the protocol pings the re-verify tick writes.
async fn next_text(ws: &mut Client) -> String {
    loop {
        let frame = match tokio::time::timeout(GUARD, ws.next()).await {
            Ok(frame) => frame,
            Err(_) => panic!("no frame arrived within the hang guard"),
        };
        match frame {
            Some(Ok(ClientMessage::Text(text))) => return text,
            Some(Ok(ClientMessage::Ping(_))) | Some(Ok(ClientMessage::Pong(_))) => continue,
            Some(Ok(ClientMessage::Close(frame))) => {
                panic!("socket closed while a text frame was expected: {frame:?}")
            }
            Some(Ok(other)) => panic!("unexpected frame: {other:?}"),
            Some(Err(e)) => panic!("socket errored: {e}"),
            None => panic!("socket ended while a text frame was expected"),
        }
    }
}

/// The next frame as JSON.
async fn next_json(ws: &mut Client) -> serde_json::Value {
    serde_json::from_str(&next_text(ws).await).expect("every server frame is JSON")
}

/// Completes a handshake and returns the connection id the ack carried.
async fn expect_ack(ws: &mut Client) -> u64 {
    let ack = next_json(ws).await;
    assert_eq!(ack["type"], "ack", "the first server frame is the ack: {ack}");
    ack["connection_id"].as_u64().expect("the ack carries a numeric connection id")
}

/// Reads until the typed close frame and returns `(code, retryable)`. The PROTOCOL close
/// that follows it is asserted separately where it matters.
async fn expect_close(ws: &mut Client) -> (String, bool) {
    let frame = next_json(ws).await;
    assert_eq!(frame["type"], "close", "expected a typed close frame, got {frame}");
    (
        frame["code"].as_str().expect("a close code").to_string(),
        frame["retryable"].as_bool().expect("a retryable flag"),
    )
}

/// Values for [`PushLimits::from_values`], so no test mutates process environment.
fn values(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

fn none() -> impl Fn(&str) -> Option<String> {
    |_: &str| None
}

// ---------------------------------------------------------------------------
// PushLimits::from_values — the parse policy, over VALUES (no process env)
// ---------------------------------------------------------------------------

/// Every knob absent (and every knob present-but-blank) keeps the build's default. The
/// blank half matters: a composition root reads `PUSH_*` straight out of the environment,
/// where an exported-but-empty variable is `Some("")`.
#[test]
fn push_limits_unset_or_blank_values_keep_the_defaults() {
    let d = PushLimits::default();
    let unset = PushLimits::from_values(none()).expect("no values is the default build");
    assert_eq!(unset.max_connections, d.max_connections);
    assert_eq!(unset.max_per_ip, d.max_per_ip);
    assert_eq!(unset.max_per_player, d.max_per_player);
    assert_eq!(unset.queue_depth, d.queue_depth);
    assert_eq!(unset.max_frame_bytes, d.max_frame_bytes);
    assert_eq!(unset.handshake_grace, d.handshake_grace);
    assert_eq!(unset.write_deadline, d.write_deadline);
    assert_eq!(unset.reverify_interval, d.reverify_interval);
    assert_eq!(unset.max_stale, d.max_stale);
    assert!(!unset.presence, "presence is OFF by default");
    assert!(unset.trusted_proxies.is_empty());

    let blank = PushLimits::from_values(values(&[
        (MAX_CONNECTIONS, ""),
        (HANDSHAKE_MS, "   "),
        (PRESENCE, ""),
        (TRUSTED_PROXIES, ""),
    ]))
    .expect("a blank value is an unset value");
    assert_eq!(blank.max_connections, d.max_connections);
    assert_eq!(blank.handshake_grace, d.handshake_grace);
    assert!(!blank.presence);
    assert!(blank.trusted_proxies.is_empty());
}

/// Every knob parses, and each lands on its OWN field — a swapped pair would be invisible
/// to a test that set them all to the same number.
#[test]
fn push_limits_parse_every_knob_from_values() {
    let limits = PushLimits::from_values(values(&[
        (MAX_CONNECTIONS, "11"),
        (MAX_PER_IP, "12"),
        (MAX_PER_PLAYER, "13"),
        (QUEUE_DEPTH, "14"),
        (MAX_FRAME_BYTES, "15"),
        (HANDSHAKE_MS, "16"),
        (WRITE_MS, "17"),
        (REVERIFY_MS, "18"),
        (MAX_STALE_MS, "19"),
        (PRESENCE, "1"),
        (TRUSTED_PROXIES, "10.0.0.0/8, 127.0.0.1"),
    ]))
    .expect("a fully-specified, valid set parses");

    assert_eq!(limits.max_connections, 11);
    assert_eq!(limits.max_per_ip, 12);
    assert_eq!(limits.max_per_player, 13);
    assert_eq!(limits.queue_depth, 14);
    assert_eq!(limits.max_frame_bytes, 15);
    assert_eq!(limits.handshake_grace, Duration::from_millis(16));
    assert_eq!(limits.write_deadline, Duration::from_millis(17));
    assert_eq!(limits.reverify_interval, Duration::from_millis(18));
    assert_eq!(limits.max_stale, Duration::from_millis(19));
    assert!(limits.presence);
    assert_eq!(
        limits.trusted_proxies,
        httpmw::parse_cidrs("10.0.0.0/8, 127.0.0.1").expect("the same parser")
    );
}

/// `0` and garbage FAIL STARTUP naming the offending variable — the deliberate divergence
/// from `admission_budget_from_value`, which falls back to its default. A cap an operator
/// typed and that was silently dropped is a bound they believe is in force and is not.
#[test]
fn push_limits_reject_zero_and_garbage_naming_the_variable() {
    for name in [
        MAX_CONNECTIONS,
        MAX_PER_IP,
        MAX_PER_PLAYER,
        QUEUE_DEPTH,
        MAX_FRAME_BYTES,
        HANDSHAKE_MS,
        WRITE_MS,
        REVERIFY_MS,
        MAX_STALE_MS,
    ] {
        for bad in ["0", "banana", "-1", "1.5", "10s"] {
            let err = match PushLimits::from_values(values(&[(name, bad)])) {
                Ok(_) => panic!("{name}={bad} must fail startup"),
                Err(err) => err,
            };
            let msg = format!("{err:#}");
            assert!(
                msg.contains(name) && msg.contains(bad),
                "the failure must name the variable and the value it refused: {msg}"
            );
        }
    }
}

/// `PUSH_PRESENCE` has its own grammar, and garbage in it fails startup like every other
/// knob — an operator who typed `PUSH_PRESENCE=enabled` must not silently get it OFF.
#[test]
fn push_presence_parses_its_own_bool_grammar() {
    for on in ["1", "true", "TRUE", "on", "On", "yes", "YES"] {
        let limits = PushLimits::from_values(values(&[(PRESENCE, on)]))
            .unwrap_or_else(|e| panic!("PUSH_PRESENCE={on} must parse: {e:#}"));
        assert!(limits.presence, "PUSH_PRESENCE={on} is ON");
    }
    for off in ["0", "false", "FALSE", "off", "Off", "no", "NO"] {
        let limits = PushLimits::from_values(values(&[(PRESENCE, off)]))
            .unwrap_or_else(|e| panic!("PUSH_PRESENCE={off} must parse: {e:#}"));
        assert!(!limits.presence, "PUSH_PRESENCE={off} is OFF");
    }
    for bad in ["enabled", "2", "y", "maybe"] {
        let err = PushLimits::from_values(values(&[(PRESENCE, bad)]))
            .expect_err("garbage in PUSH_PRESENCE fails startup");
        let msg = format!("{err:#}");
        assert!(msg.contains(PRESENCE) && msg.contains(bad), "must name both: {msg}");
    }
}

/// A malformed trusted-proxy list fails startup rather than silently trusting nobody —
/// which would leave the per-IP cap bucketing every forwarded client under the proxy.
#[test]
fn push_limits_reject_a_malformed_trusted_proxy_cidr() {
    let err = PushLimits::from_values(values(&[(TRUSTED_PROXIES, "10.0.0.0/8, not-a-cidr")]))
        .expect_err("a malformed CIDR fails startup");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("trusted proxy") || msg.contains("not-a-cidr"),
        "the failure must point at the CIDR list: {msg}"
    );
    assert!(
        PushLimits::default().with_trusted_proxies("bogus").is_err(),
        "the builder shares the one parser"
    );
}

// ---------------------------------------------------------------------------
// The denial → close mapping: an outage is never a credential verdict
// ---------------------------------------------------------------------------

/// All four `Unavailable` arms — the api-key verifier, the session verifier, and the
/// admission budget, whichever side reached them — map to ONE retryable close. This is
/// the mapping both the bind-time admission and the re-verify tick call, so an accounts
/// blip cannot log every player out on either path.
#[test]
fn every_unavailable_denial_is_a_retryable_close_never_a_verdict() {
    for denial in [
        AdmissionDenial::Key(KeyDenial::Unavailable),
        AdmissionDenial::SessionUnavailable,
        AdmissionDenial::Timeout,
    ] {
        let code = close_for(&denial);
        assert_eq!(code, CloseCode::Unavailable, "an outage is not a credential verdict");
        assert!(code.retryable(), "an outage must tell the client to come back");
    }

    // And the verdicts stay verdicts: a client that retries them would spin.
    assert_eq!(close_for(&AdmissionDenial::MissingBearer), CloseCode::Unauthorized);
    assert_eq!(close_for(&AdmissionDenial::InvalidSession), CloseCode::Unauthorized);
    assert_eq!(close_for(&AdmissionDenial::Key(KeyDenial::Missing)), CloseCode::ApiKey);
    assert_eq!(close_for(&AdmissionDenial::Key(KeyDenial::Invalid)), CloseCode::ApiKey);
    assert_eq!(close_for(&AdmissionDenial::Key(KeyDenial::Forbidden)), CloseCode::ApiKey);
    assert!(!CloseCode::Unauthorized.retryable());
    assert!(!CloseCode::ApiKey.retryable());
}

/// The client-facing half of the contract: what each code says about retrying, and the
/// RFC 6455 code a browser's `CloseEvent` carries.
#[test]
fn close_codes_carry_their_retry_and_protocol_contract() {
    for (code, retryable, ws) in [
        (CloseCode::Unauthorized, false, 1008u16),
        (CloseCode::ApiKey, false, 1008),
        (CloseCode::Protocol, false, 1008),
        // A retry would evict the next-oldest device, and a pair over the cap would
        // evict each other forever.
        (CloseCode::Replaced, false, 1000),
        (CloseCode::Unavailable, true, 1013),
        (CloseCode::HandshakeTimeout, true, 1013),
        (CloseCode::Capacity, true, 1013),
        (CloseCode::SessionExpired, true, 1000),
        (CloseCode::Shutdown, true, 1001),
    ] {
        assert_eq!(code.retryable(), retryable, "{code:?} retryable");
        assert_eq!(code.ws_code(), ws, "{code:?} ws code");
        let frame: serde_json::Value =
            serde_json::from_str(&code.frame()).expect("the close frame is JSON");
        assert_eq!(frame["type"], "close");
        assert_eq!(frame["retryable"], retryable);
        assert_eq!(frame["reason"], code.reason());
    }
}

// ---------------------------------------------------------------------------
// KeyCheck: the MODE of the one authority
// ---------------------------------------------------------------------------

/// The same key, the same `check_api_key`, two modes: a policy that names NOTHING holds a
/// `/push` socket (`PresenceOnly`) and is refused for an operation (`Policy`). If the
/// mode ever stopped being consulted, one of these two assertions goes red whichever way
/// it collapsed.
#[tokio::test]
async fn presence_only_admits_the_policyless_key_that_an_operation_forbids() {
    let keys = FakeKeys::demo();

    assert!(
        check_api_key(&*keys, Some(POLICYLESS_KEY), KeyCheck::PresenceOnly)
            .await
            .is_ok(),
        "a fixed route has no wire method, so presence and validity are the whole check"
    );
    assert!(
        matches!(
            check_api_key(&*keys, Some(POLICYLESS_KEY), KeyCheck::Policy("demo.echo")).await,
            Err(KeyDenial::Forbidden)
        ),
        "an operation still demands a policy match"
    );

    // The shared verdicts are shared: neither mode invents its own.
    for mode in [KeyCheck::PresenceOnly, KeyCheck::Policy("demo.echo")] {
        assert!(matches!(
            check_api_key(&*keys, None, mode).await,
            Err(KeyDenial::Missing)
        ));
        assert!(matches!(
            check_api_key(&*keys, Some("no-such-key"), mode).await,
            Err(KeyDenial::Invalid)
        ));
        assert!(matches!(
            check_api_key(&UnavailableKeys, Some(KEY), mode).await,
            Err(KeyDenial::Unavailable)
        ));
    }
    assert!(check_api_key(&*keys, Some(KEY), KeyCheck::Policy("demo.echo"))
        .await
        .is_ok());
}

// ---------------------------------------------------------------------------
// The per-connection queue: drop-OLDEST, and a close that preempts
// ---------------------------------------------------------------------------

/// At depth the queue evicts its HEAD, so the newest window survives in order. Dropping
/// the newest would be the wrong end: the durable copy of anything that matters is in the
/// producing module's tables and a push frame only says "refetch".
#[tokio::test]
async fn the_queue_drops_the_oldest_and_keeps_the_newest_window_in_order() {
    let hub = hub_with(PushLimits { queue_depth: 2, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    for n in 0..5 {
        assert_eq!(
            hub.deliver(&Target::Player("alice".into()), &msg(&format!("m{n}"))),
            1,
            "every offered frame is accepted — an eviction is not a refusal"
        );
    }

    assert_eq!(
        queued_payloads(&slot.queue),
        vec!["m3".to_string(), "m4".to_string()],
        "the two NEWEST frames survive, oldest-first"
    );
}

/// A close is set once (the FIRST reason wins, so a shutdown cannot relabel a connection
/// already closing for another reason), preempts the backlog, and every later frame is
/// refused — which is what keeps `Delivered::Local(n)` honest.
#[tokio::test]
async fn the_queue_keeps_the_first_close_reason_and_refuses_later_frames() {
    let hub = hub_with(test_limits());
    let (slot, mut wake) = bound(&hub, local(), "alice");

    assert!(slot.queue.push("", Arc::from("pending")), "a live queue accepts");
    slot.queue.close(CloseCode::Replaced);
    slot.queue.close(CloseCode::Shutdown);
    assert_eq!(queued_close(&slot.queue), Some(CloseCode::Replaced));
    assert!(
        !slot.queue.push("", Arc::from("too late")),
        "a closing connection refuses the frame, so it is not counted as delivered"
    );

    match slot.queue.recv(&mut wake).await {
        Some(Outbound::Close(code)) => assert_eq!(code, CloseCode::Replaced),
        other => panic!("the close must preempt the pending frame, got {:?}", other.is_some()),
    }
}

fn topic_msg(topic: &str, payload: &str) -> Message {
    Message::new(topic, payload.as_bytes().to_vec())
}

fn to_alice(hub: &Arc<PushHub>, topic: &str, payload: &str) {
    assert_eq!(hub.deliver(&Target::Player("alice".into()), &topic_msg(topic, payload)), 1);
}

/// One chatty hub must not starve another: with a single shared drop-oldest ring, the `b`
/// frame queued FIRST would be the oldest of 201 and the first thing evicted. Here it is
/// queued before the flood, at depth 64, and survives: the flood only ever evicts from
/// its own (longest) lane.
#[tokio::test]
async fn a_flooded_hub_does_not_evict_another_hubs_frame() {
    let hub = hub_with(PushLimits { queue_depth: 64, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    to_alice(&hub, "b.tick", "b0");
    for n in 0..200 {
        to_alice(&hub, "a.tick", &format!("a{n}"));
    }

    assert_eq!(
        lane_lens(&slot.queue),
        vec![("".to_string(), 0), ("b".to_string(), 1), ("a".to_string(), 63)],
        "b keeps its one frame; the flood is held to depth minus b's frame in its own lane"
    );
    let payloads = queued_payloads(&slot.queue);
    assert!(payloads.contains(&"b0".to_string()), "the oldest frame overall survived");
    assert!(!payloads.contains(&"a0".to_string()), "the flood evicted its own oldest");
    assert!(payloads.contains(&"a199".to_string()), "the flood's newest is queued");
    assert_eq!(event_total(&slot.queue), 64, "depth is the total across event lanes");
}

/// At capacity the victim is the oldest frame of the LONGEST lane, and a tie goes to the
/// lowest lane index.
#[tokio::test]
async fn eviction_takes_from_the_longest_lane_and_the_lowest_index_on_a_tie() {
    let hub = hub_with(PushLimits { queue_depth: 4, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    // Lanes: "" (0), a (1), b (2).
    to_alice(&hub, "a.x", "a1");
    to_alice(&hub, "a.x", "a2");
    to_alice(&hub, "a.x", "a3");
    to_alice(&hub, "b.x", "b1");
    assert_eq!(
        lane_lens(&slot.queue),
        vec![("".to_string(), 0), ("a".to_string(), 3), ("b".to_string(), 1)]
    );
    // Full: the longest lane is `a`, so its OLDEST frame goes, not b's or the newest.
    to_alice(&hub, "b.x", "b2");
    assert_eq!(
        lane_lens(&slot.queue),
        vec![("".to_string(), 0), ("a".to_string(), 2), ("b".to_string(), 2)]
    );
    let payloads = queued_payloads(&slot.queue);
    assert!(!payloads.contains(&"a1".to_string()), "a's oldest was evicted");
    assert!(payloads.contains(&"b1".to_string()) && payloads.contains(&"b2".to_string()));

    // Tie (a=2, b=2), full: the lowest index (a) loses its oldest, even though the new
    // frame goes to b.
    to_alice(&hub, "b.x", "b3");
    assert_eq!(
        lane_lens(&slot.queue),
        vec![("".to_string(), 0), ("a".to_string(), 1), ("b".to_string(), 3)]
    );
    assert!(!queued_payloads(&slot.queue).contains(&"a2".to_string()));
}

/// The longest lane and the oldest frame anywhere disagree: `b1` is the oldest frame in
/// the whole queue, but `a` is the longest lane, so the victim is `a1`. A single ring (or
/// any "evict the globally oldest" rule) evicts `b1` instead.
#[tokio::test]
async fn eviction_takes_the_longest_lane_even_when_another_lane_holds_the_oldest_frame() {
    let hub = hub_with(PushLimits { queue_depth: 4, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    to_alice(&hub, "b.x", "b1");
    to_alice(&hub, "a.x", "a1");
    to_alice(&hub, "a.x", "a2");
    to_alice(&hub, "a.x", "a3");
    assert_eq!(event_total(&slot.queue), 4, "the queue is exactly full");

    to_alice(&hub, "c.x", "c1");

    assert_eq!(
        lane_lens(&slot.queue),
        vec![
            ("".to_string(), 0),
            ("b".to_string(), 1),
            ("a".to_string(), 2),
            ("c".to_string(), 1),
        ]
    );
    let payloads = queued_payloads(&slot.queue);
    assert!(payloads.contains(&"b1".to_string()), "the globally oldest frame survived");
    assert!(!payloads.contains(&"a1".to_string()), "the longest lane lost its oldest");
    assert!(payloads.contains(&"c1".to_string()), "the new frame was queued");
}

/// Named lanes are capped at MAX_HUBS_PER_CONN including `""`; a hub past the cap shares
/// lane 0 and the lane count never grows.
#[tokio::test]
async fn hubs_past_the_lane_cap_share_lane_zero() {
    let hub = hub_with(PushLimits { queue_depth: 64, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    // 15 named hubs fill indices 1..=15.
    for n in 0..MAX_HUBS_PER_CONN - 1 {
        to_alice(&hub, &format!("h{n}.x"), &format!("p{n}"));
    }
    assert_eq!(lane_lens(&slot.queue).len(), MAX_HUBS_PER_CONN);
    assert_eq!(lane_lens(&slot.queue)[0], ("".to_string(), 0));

    // The 16th distinct named hub (and a 17th) land in lane 0.
    to_alice(&hub, "overflow.x", "o1");
    to_alice(&hub, "another.x", "o2");
    let lanes = lane_lens(&slot.queue);
    assert_eq!(lanes.len(), MAX_HUBS_PER_CONN, "lane count never exceeds the cap");
    assert_eq!(lanes[0], ("".to_string(), 2), "overflow hubs share lane \"\" at index 0");
    assert!(lanes.iter().skip(1).all(|(_, n)| *n == 1));
}

/// recv order: a pending close beats a reply, a reply beats events, and two non-empty
/// event lanes alternate.
#[tokio::test]
async fn recv_serves_close_then_reply_then_events_round_robin() {
    let hub = hub_with(PushLimits { queue_depth: 16, ..test_limits() });
    let (slot, mut wake) = bound(&hub, local(), "alice");
    let text = |o: Outbound| match o {
        Outbound::Text(t) | Outbound::Result(t, _) => t.to_string(),
        Outbound::Close(c) => panic!("unexpected close {c:?}"),
    };

    to_alice(&hub, "a.x", "a1");
    to_alice(&hub, "a.x", "a2");
    to_alice(&hub, "b.x", "b1");
    to_alice(&hub, "b.x", "b2");
    assert!(slot.queue.push_reply(Reply::Frame(Arc::from("reply"))));

    // Reply first even though events were queued before it, then a/b alternate.
    assert_eq!(text(slot.queue.recv(&mut wake).await.unwrap()), "reply");
    let mut order = Vec::new();
    for _ in 0..4 {
        let frame = text(slot.queue.recv(&mut wake).await.unwrap());
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        order.push(v["hub"].as_str().unwrap().to_string());
    }
    assert_eq!(order, vec!["a", "b", "a", "b"], "non-empty lanes alternate");

    // Close preempts both a pending reply and pending events.
    to_alice(&hub, "a.x", "a3");
    assert!(slot.queue.push_reply(Reply::Frame(Arc::from("reply2"))));
    slot.queue.close(CloseCode::Shutdown);
    match slot.queue.recv(&mut wake).await {
        Some(Outbound::Close(code)) => assert_eq!(code, CloseCode::Shutdown),
        _ => panic!("the close must come before the reply and the events"),
    }
}

/// The reply lane is never evicted: filling the event lanes past depth leaves the ack.
#[tokio::test]
async fn the_reply_lane_survives_event_lane_eviction() {
    let hub = hub_with(PushLimits { queue_depth: 2, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");

    assert!(slot.queue.push_reply(Reply::Frame(Arc::from("ack"))));
    for n in 0..10 {
        to_alice(&hub, "a.x", &format!("m{n}"));
    }
    let frames = queued(&slot.queue);
    assert_eq!(frames.first().map(String::as_str), Some("ack"));
    assert_eq!(frames.len(), 3, "the ack plus the two-frame event window");
}

/// The wire `message` frame carries `hub` = the topic prefix before the first dot, and
/// `""` for a dotless topic; two topics of one hub share ONE lane.
#[tokio::test]
async fn the_message_frame_carries_its_hub() {
    let hub = hub_with(test_limits());
    let (slot, _wake) = bound(&hub, local(), "alice");

    to_alice(&hub, "party.state", "x");
    to_alice(&hub, "party.member.joined", "z");
    to_alice(&hub, "nodot", "y");

    let frames: Vec<serde_json::Value> = queued(&slot.queue)
        .iter()
        .map(|f| serde_json::from_str(f).unwrap())
        .collect();
    let by_topic = |topic: &str| {
        frames.iter().find(|f| f["topic"] == topic).unwrap_or_else(|| panic!("no {topic}"))
    };
    assert_eq!(by_topic("party.state")["hub"], "party");
    assert_eq!(
        by_topic("party.member.joined")["hub"],
        "party",
        "the hub is the prefix before the FIRST dot, not the last"
    );
    assert_eq!(by_topic("nodot")["hub"], "");
    assert_eq!(
        lane_lens(&slot.queue),
        vec![("".to_string(), 1), ("party".to_string(), 2)],
        "both party topics share the one party lane; the dotless topic is lane \"\""
    );
}

/// `queued()` claims to list frames in the order `recv` yields them; the assertions that
/// lean on that order are only as good as the claim. Drain a mixed-lane queue through the
/// production `recv`, from a non-zero round-robin cursor, and compare.
#[tokio::test]
async fn the_queued_helper_lists_frames_in_the_order_recv_yields_them() {
    let hub = hub_with(PushLimits { queue_depth: 32, ..test_limits() });
    let (slot, mut wake) = bound(&hub, local(), "alice");

    for (n, topic) in ["a.x", "a.x", "a.x", "b.x", "c.x", "c.x", "nodot"].iter().enumerate() {
        to_alice(&hub, topic, &format!("p{n}"));
    }
    assert!(slot.queue.push_reply(Reply::Frame(Arc::from("reply"))));
    // One recv per lane advances the cursor off zero, so a helper that always starts at
    // lane 0 disagrees with the production cursor.
    for _ in 0..3 {
        let _ = slot.queue.recv(&mut wake).await.expect("a queued frame");
    }

    let listed = queued(&slot.queue);
    assert!(listed.len() > 3, "a mixed queue with several lanes left to drain");
    let mut drained = Vec::new();
    for _ in 0..listed.len() {
        match slot.queue.recv(&mut wake).await.expect("a queued frame") {
            Outbound::Text(text) | Outbound::Result(text, _) => drained.push(text.to_string()),
            Outbound::Close(code) => panic!("unexpected close {code:?}"),
        }
    }
    assert_eq!(listed, drained);
    assert!(queued(&slot.queue).is_empty(), "the drain consumed exactly what was listed");
}

/// The handshake ack is queued by the production `enqueue_ack` on the reply lane. Case 1
/// moves the round-robin cursor OFF lane 0 (e1 is served from lane `a`, leaving the cursor on
/// lane `b`) before the ack is enqueued: an ack pushed as an event frame would land in lane 0
/// behind the cursor and be yielded AFTER e2, so "ack first" fails. Case 2 floods the event
/// lanes to depth: an ack pushed as an event frame would evict an event, so the event total
/// after the ack is served would be 3, not 4 (the ack-first assertion alone does not
/// discriminate there, since the cursor is still on lane 0).
#[tokio::test]
async fn the_ack_is_the_first_frame_even_behind_queued_events_and_a_flood() {
    let hub = hub_with(PushLimits { queue_depth: 4, ..test_limits() });
    let ack_is = |frame: &str, id: ConnId| {
        let v: serde_json::Value = serde_json::from_str(frame).unwrap();
        assert_eq!(v["type"], "ack", "the first frame is the ack: {v}");
        assert_eq!(v["connection_id"], serde_json::to_value(id).unwrap());
    };

    // Events queued, and the cursor moved off lane 0, when the ack is enqueued.
    let (slot, mut wake) = bound(&hub, local(), "alice");
    to_alice(&hub, "a.x", "e1");
    to_alice(&hub, "b.x", "e2");
    to_alice(&hub, "a.x", "e3");
    match slot.queue.recv(&mut wake).await {
        Some(Outbound::Text(text)) => {
            assert!(text.contains("\"topic\":\"a.x\""), "e1 is served first: {text}")
        }
        _ => panic!("expected the first event"),
    }
    assert_eq!(event_total(&slot.queue), 2);
    assert!(enqueue_ack(&slot.queue, slot.id));
    match slot.queue.recv(&mut wake).await {
        Some(Outbound::Text(text)) => ack_is(&text, slot.id),
        _ => panic!("expected the ack before the remaining events"),
    }
    assert_eq!(event_total(&slot.queue), 2, "the events were not consumed by the ack");

    // The event lanes flooded far past depth before the ack.
    let (flooded, mut wake) = bound(&hub, local(), "bob");
    for n in 0..20 {
        assert_eq!(hub.deliver(&Target::Player("bob".into()), &msg(&format!("f{n}"))), 1);
    }
    assert_eq!(event_total(&flooded.queue), 4, "the flood is at depth");
    assert!(enqueue_ack(&flooded.queue, flooded.id));
    match flooded.queue.recv(&mut wake).await {
        Some(Outbound::Text(text)) => ack_is(&text, flooded.id),
        _ => panic!("expected the ack first under a flood"),
    }
    assert_eq!(event_total(&flooded.queue), 4, "the ack cost the event lanes nothing");
}

// ---------------------------------------------------------------------------
// Addressing: an accepted-but-unbound socket is not a participant
// ---------------------------------------------------------------------------

/// NO `Target` variant reaches a connection that has not bound an identity — the property
/// the handshake's queue branch states as `unreachable!`. It is true only because of one
/// `filter` and one `?` in `HubState::resolve`/`addressable`, in a different function from
/// the panic: an edit there turns a routing change into a PANICKING public-facing
/// connection task (and a panicked task never runs its bind's matching offline
/// announcement, latching that player's presence `online` forever).
#[tokio::test]
async fn no_target_variant_reaches_an_accepted_but_unbound_connection() {
    let hub = hub_with(test_limits());
    // Accepted, never bound — and a member of a group, which a client can only reach
    // after binding but which the hub must refuse to address regardless.
    let (unbound, _wake) = accept(&hub, local());
    assert!(hub.join(unbound.id, "raid"), "the registry entry exists, so the join lands");

    // A bound connection under the SAME player name and group, so each target below
    // resolves to a non-empty set and the assertion is about WHICH connections it names.
    let (member, _wake2) = bound(&hub, local(), "alice");
    assert!(hub.join(member.id, "raid"));

    for target in [
        Target::Player("alice".into()),
        Target::Group("raid".into()),
        Target::All,
    ] {
        assert_eq!(hub.deliver(&target, &msg("hello")), 1, "{target:?} reaches the bound one");
    }
    assert_eq!(queued(&member.queue).len(), 3);
    assert!(
        queued(&unbound.queue).is_empty(),
        "an unauthenticated socket is not a participant and must not be told about the ones \
         that are — its task treats a queued frame as a routing defect"
    );
}

/// A payload over the outbound cap is dropped and counted, never multiplied across the
/// fan-out: `deliver` runs synchronously on a producer's thread, which may be a durable
/// handler holding its delivery transaction's connection.
#[tokio::test]
async fn an_oversize_payload_is_refused_before_any_connection_is_touched() {
    let hub = hub_with(test_limits());
    let (slot, _wake) = bound(&hub, local(), "alice");

    let big = Message::new("test.topic", vec![b'x'; MAX_PAYLOAD_BYTES + 1]);
    assert_eq!(
        hub.deliver(&Target::All, &big),
        0,
        "the 0 is a DROP, not an empty target set"
    );
    assert!(queued(&slot.queue).is_empty(), "nothing is enqueued anywhere");

    let at_cap = Message::new("test.topic", vec![b'x'; MAX_PAYLOAD_BYTES]);
    assert_eq!(hub.deliver(&Target::All, &at_cap), 1, "exactly at the cap still delivers");
}

/// `Delivered::Local(n)` counts connections the frame REACHED: zero when nobody addressed
/// is here (the normal answer on a front that owns none of this player's devices), and a
/// closing connection is excluded rather than counted on its way out.
#[tokio::test]
async fn local_sink_counts_only_the_connections_a_frame_reached() {
    let hub = hub_with(test_limits());
    let sink = LocalSink::new(hub.clone());
    // The production trait method, not a helper: `push::Sink::send` is what a producer calls.
    use push::Sink as _;

    assert_eq!(
        sink.send(&Target::Player("nobody".into()), &msg("m")).unwrap(),
        push::Delivered::Local(0),
        "nobody addressed here is a normal 0"
    );

    let (live, _w1) = bound(&hub, local(), "alice");
    let (leaving, _w2) = bound(&hub, local(), "alice");
    leaving.queue.close(CloseCode::Replaced);

    assert_eq!(
        sink.send(&Target::Player("alice".into()), &msg("m")).unwrap(),
        push::Delivered::Local(1),
        "the closing device is not a delivery"
    );
    assert_eq!(queued(&live.queue).len(), 1);
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

/// Join is idempotent (a re-join consumes no further quota), leave removes membership,
/// and a leave for a group the connection never held is a `false` — not an error, because
/// the client's view of its own membership is advisory.
#[tokio::test]
async fn group_join_is_idempotent_and_leave_removes_membership() {
    let hub = hub_with(test_limits());
    let (slot, _wake) = bound(&hub, local(), "alice");

    assert!(hub.join(slot.id, "raid"));
    assert!(hub.join(slot.id, "raid"), "a re-join succeeds");
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("m")), 1);

    assert!(hub.leave(slot.id, "raid"));
    assert!(!hub.leave(slot.id, "raid"), "a leave for a group not held is not an error");
    assert!(!hub.leave(slot.id, "never-joined"));
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("m")), 0);
    assert_eq!(queued(&slot.queue).len(), 1, "only the frame from before the leave");
}

/// The two bounds one socket can make this process allocate: an over-long group name and
/// a connection already holding its quota. Both are refusals, never a disconnect — a
/// refused join costs the client only the membership it asked for.
#[tokio::test]
async fn group_join_refuses_an_overlong_name_and_an_over_quota_connection() {
    let hub = hub_with(test_limits());
    let (slot, _wake) = bound(&hub, local(), "alice");

    assert!(!hub.join(slot.id, ""), "an empty name is refused");
    assert!(hub.join(slot.id, &"g".repeat(MAX_GROUP_NAME_BYTES)), "exactly at the cap");
    assert!(!hub.join(slot.id, &"g".repeat(MAX_GROUP_NAME_BYTES + 1)), "one byte over");
    assert!(PushHub::conformance_group_name_rejected(MAX_GROUP_NAME_BYTES + 1));
    assert!(!PushHub::conformance_group_name_rejected(MAX_GROUP_NAME_BYTES));

    // One group is already held (the at-cap name above), so the quota is reached at
    // MAX_GROUPS_PER_CONN distinct names.
    for n in 1..MAX_GROUPS_PER_CONN {
        assert!(hub.join(slot.id, &format!("g{n}")), "join {n} is within quota");
    }
    assert!(!hub.join(slot.id, "one-too-many"), "the quota refuses a NEW group");
    assert!(hub.join(slot.id, "g1"), "a re-join of a held group still succeeds at quota");
}

/// Membership dies with the connection and is NOT restored on a reconnect: it is
/// per-process ephemeral state the client rebuilds, never an authorization the hub owes
/// a returning player.
#[tokio::test]
async fn group_membership_dies_with_the_connection_and_is_not_restored() {
    let hub = hub_with(test_limits());
    let (first, _wake) = bound(&hub, local(), "alice");
    assert!(hub.join(first.id, "raid"));
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("m")), 1);

    drop(first);
    assert_eq!(
        hub.deliver(&Target::Group("raid".into()), &msg("m")),
        0,
        "the group is gone with its last member"
    );

    // The same player reconnects: a NEW connection, with no membership.
    let (again, _wake2) = bound(&hub, local(), "alice");
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("m")), 0);
    assert_eq!(
        hub.deliver(&Target::Player("alice".into()), &msg("m")),
        1,
        "the player is addressable again — only the membership was lost"
    );
    assert_eq!(queued(&again.queue).len(), 1);
}

// ---------------------------------------------------------------------------
// The caps, and the release path a test that only connects cannot see
// ---------------------------------------------------------------------------

/// The global cap refuses at the boundary and RELEASES on disconnect: a missing decrement
/// would wedge the front at its cap forever, and a doubled one would let it over.
#[tokio::test]
async fn the_global_cap_refuses_at_the_boundary_and_releases_on_disconnect() {
    let hub = hub_with(PushLimits { max_connections: 2, ..test_limits() });
    let (first, _w1) = accept(&hub, local());
    let (second, _w2) = accept(&hub, local());
    assert!(matches!(hub.accept(local()), Err(AcceptError::Full)));

    drop(second);
    let (third, _w3) = accept(&hub, local());
    assert!(matches!(hub.accept(local()), Err(AcceptError::Full)), "released exactly one");

    drop(first);
    drop(third);
    assert_eq!(hub.live(), 0);
    // Both slots came back — a doubled decrement would have let a THIRD in here.
    let (_a, _wa) = accept(&hub, local());
    let (_b, _wb) = accept(&hub, local());
    assert!(matches!(hub.accept(local()), Err(AcceptError::Full)));
}

/// The per-IP cap buckets by RESOLVED address, refuses at the boundary, and drops the
/// bucket when its last connection leaves.
#[tokio::test]
async fn the_per_ip_cap_refuses_at_the_boundary_and_releases_its_bucket() {
    let hub = hub_with(PushLimits { max_per_ip: 1, ..test_limits() });
    let other = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7));

    let (first, _w1) = accept(&hub, local());
    assert!(matches!(hub.accept(local()), Err(AcceptError::IpFull)));
    let (_second, _w2) = accept(&hub, other);

    drop(first);
    assert!(
        !hub.state.lock().unwrap().per_ip.contains_key(&local()),
        "an emptied bucket is removed, not left at zero"
    );
    let (_third, _w3) = accept(&hub, local());
}

/// The per-player cap evicts the LONGEST-BOUND device (so a player's newest device always
/// connects), the eviction is a typed close on the victim, and the slot is released when
/// that victim's task ends.
#[tokio::test]
async fn the_per_player_cap_evicts_the_longest_bound_device_and_releases_it() {
    let hub = hub_with(PushLimits { max_per_player: 2, ..test_limits() });

    let (a, _wa) = accept(&hub, local());
    let bound_a = hub.bind(a.id, "alice").expect("first device binds");
    assert!(bound_a.first, "the first bound connection is the online transition");
    assert!(bound_a.evicted.is_none());

    let (b, _wb) = accept(&hub, local());
    let bound_b = hub.bind(b.id, "alice").expect("second device binds");
    assert!(!bound_b.first, "a second device is not a new online transition");
    assert!(bound_b.evicted.is_none(), "still within the cap");

    let (c, _wc) = accept(&hub, local());
    let bound_c = hub.bind(c.id, "alice").expect("third device binds");
    let evicted = bound_c.evicted.expect("the cap evicts one");
    evicted.close(CloseCode::Replaced);
    assert_eq!(
        queued_close(&a.queue),
        Some(CloseCode::Replaced),
        "the LONGEST-BOUND device is the one evicted"
    );
    assert_eq!(queued_close(&b.queue), None);

    assert_eq!(hub.deliver(&Target::Player("alice".into()), &msg("m")), 2, "the two live ones");

    // The counterfactual first: still at the cap, so a fourth device evicts the
    // now-longest-bound one. Without it the release assertion below would be vacuous.
    let (d, _wd) = accept(&hub, local());
    let evicted_b = hub.bind(d.id, "alice").expect("binds").evicted.expect("still at the cap");
    assert!(
        Arc::ptr_eq(&evicted_b, &b.queue),
        "the eviction walks the deque in bind order"
    );

    // The RELEASE path. `a` and `b` left the player's deque AT their eviction, so their
    // tasks ending must NOT decrement anything a second time; the live devices are `c`
    // and `d`, and one of THEM leaving must free a real slot — otherwise the player sits
    // permanently at the cap and every reconnect evicts a device.
    drop(a);
    drop(b);
    drop(c);
    let (e, _we) = accept(&hub, local());
    assert!(
        hub.bind(e.id, "alice").expect("binds").evicted.is_none(),
        "one live departure frees exactly one device slot (and two already-evicted ones          free none)"
    );

    drop(d);
    drop(e);
    let (f, _wf) = accept(&hub, local());
    assert!(
        hub.bind(f.id, "alice").expect("binds").first,
        "with every device gone the player's next bind is an online transition again"
    );
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

fn presence_hub() -> Arc<PushHub> {
    hub_with(PushLimits { presence: true, ..test_limits() })
}

/// The transitions an observer sees: `online` on a player's FIRST device only, nothing on
/// the second, and `offline` only when the LAST one leaves.
#[tokio::test]
async fn presence_announces_once_per_player_and_only_on_the_last_departure() {
    let hub = presence_hub();
    let (observer, _wo) = bound(&hub, local(), "observer");
    // The observer's own bind announced nothing to itself yet — it announces AFTER
    // binding, which `run` does; here the announcement is driven explicitly.
    assert!(queued(&observer.queue).is_empty());

    let (first, _w1) = accept(&hub, local());
    let b1 = hub.bind(first.id, "alice").expect("binds");
    assert!(b1.first);
    hub.announce("alice", true);

    let (second, _w2) = accept(&hub, local());
    let b2 = hub.bind(second.id, "alice").expect("binds");
    assert!(!b2.first, "no duplicate online for a second device");

    drop(second);
    assert_eq!(
        queued_topics(&observer.queue),
        vec![PRESENCE_TOPIC.to_string()],
        "one device leaving while another remains announces nothing"
    );

    drop(first);
    let topics = queued_topics(&observer.queue);
    assert_eq!(topics, vec![PRESENCE_TOPIC.to_string(), PRESENCE_TOPIC.to_string()]);
    let payloads = queued_payloads(&observer.queue);
    assert_eq!(payloads[0], r#"{"player_id":"alice","online":true}"#);
    assert_eq!(payloads[1], r#"{"player_id":"alice","online":false}"#);
}

/// THE race the transition is decided under the mutation's own guard for: a disconnect
/// landing after the same player's reconnect must not latch `offline` on a player who is
/// connected. Deciding afterwards — by re-reading the registry — would announce it.
///
/// A test that only connects and disconnects cannot see this: the removal happens either
/// way, and only the ORDER of the deque emptiness check against the reconnect's push
/// distinguishes the two implementations.
#[tokio::test]
async fn a_disconnect_racing_the_same_players_reconnect_does_not_latch_offline() {
    let hub = presence_hub();
    let (observer, _wo) = bound(&hub, local(), "observer");

    let (old, _w1) = bound(&hub, local(), "alice");
    // The reconnect binds BEFORE the old connection's slot is released — the interleaving
    // a mobile client produces on every network change.
    let (new, _w2) = bound(&hub, local(), "alice");

    drop(old);
    assert!(
        queued(&observer.queue).is_empty(),
        "the departing connection must observe that the player still has a device and \
         announce nothing"
    );
    assert_eq!(
        hub.deliver(&Target::Player("alice".into()), &msg("m")),
        1,
        "and the reconnected device is still addressable"
    );
    assert_eq!(queued(&new.queue).len(), 1);

    drop(new);
    assert_eq!(
        queued_payloads(&observer.queue),
        vec![r#"{"player_id":"alice","online":false}"#.to_string()],
        "offline is announced exactly once, by the LAST departure"
    );
}

/// Presence is OFF by default, and the one gate covers BOTH emission sites — neither can
/// be enabled without the other.
#[tokio::test]
async fn presence_is_silent_when_the_limit_is_off() {
    assert!(!test_limits().presence);
    let hub = hub_with(test_limits());
    let (observer, _wo) = bound(&hub, local(), "observer");

    let (alice, _w1) = bound(&hub, local(), "alice");
    hub.announce("alice", true);
    drop(alice);

    assert!(queued(&observer.queue).is_empty(), "no presence traffic at all");
}

/// A stopping front announces nothing: every queue already carries its close, so each
/// departing connection would walk every other one to deliver zero frames.
#[tokio::test]
async fn a_stopping_front_announces_no_presence() {
    let hub = presence_hub();
    let (observer, _wo) = bound(&hub, local(), "observer");
    let (alice, _w1) = bound(&hub, local(), "alice");

    hub.state.lock().unwrap().closing = true;
    hub.announce("alice", true);
    drop(alice);

    assert_eq!(
        queued(&observer.queue),
        Vec::<String>::new(),
        "the observer's queue carries no presence frame"
    );
}

// ---------------------------------------------------------------------------
// Shutdown: the drain, and the abort collection AFTER the grace
// ---------------------------------------------------------------------------

/// Every live connection is signalled, and a task that drains its close and leaves is
/// gone before the grace elapses — the ordinary path.
#[tokio::test(start_paused = true)]
async fn shutdown_signals_every_connection_and_returns_on_the_drain() {
    let hub = hub_with(test_limits());
    let (a, mut wa) = bound(&hub, local(), "alice");
    let (b, mut wb) = bound(&hub, local(), "bob");

    let drained = tokio::spawn({
        let hub = hub.clone();
        async move {
            for (slot, wake) in [(a, &mut wa), (b, &mut wb)] {
                match slot.queue.recv(wake).await {
                    Some(Outbound::Close(code)) => assert_eq!(code, CloseCode::Shutdown),
                    other => panic!("expected the shutdown close, got {:?}", other.is_some()),
                }
                drop(slot);
            }
            drop(hub);
        }
    });

    let started = tokio::time::Instant::now();
    hub.shutdown().await;
    drained.await.expect("the connection tasks ended");

    assert_eq!(hub.live(), 0, "the registry is empty because every slot was released");
    assert!(
        started.elapsed() < PUSH_STOP_GRACE,
        "a drained registry must not cost the grace: {:?}",
        started.elapsed()
    );
}

/// The leak the Step 4 review found: a connection whose task attaches its abort handle
/// AFTER `closing` is set, and which then never drains its queue. Snapshotting the abort
/// handles up front leaves exactly this connection in nobody's list, and a peer that
/// stopped reading then keeps the task — and its socket — alive past module stop.
///
/// The ordering is a happens-before, not a sleep: the task attaches only once it has
/// OBSERVED the shutdown close in its queue, which is written under the same guard that
/// sets `closing`, and the test advances the virtual clock past the grace only after the
/// attach is acknowledged.
#[tokio::test(start_paused = true)]
async fn shutdown_aborts_a_connection_that_attached_its_handle_after_closing() {
    let hub = hub_with(test_limits());
    let (slot, mut wake) = bound(&hub, local(), "alice");

    let (abort_tx, abort_rx) = oneshot::channel::<tokio::task::AbortHandle>();
    let (attached_tx, attached_rx) = oneshot::channel::<()>();

    let stuck = tokio::spawn(async move {
        let abort = abort_rx.await.expect("its own abort handle");
        match slot.queue.recv(&mut wake).await {
            Some(Outbound::Close(code)) => assert_eq!(code, CloseCode::Shutdown),
            other => panic!("expected the shutdown close, got {:?}", other.is_some()),
        }
        // Attached only NOW — after `closing` was set, which is the whole point.
        slot.hub.attach_abort(slot.id, abort);
        attached_tx.send(()).expect("the test is waiting");
        // A peer that stopped reading: the task holds its slot and never exits on its own.
        std::future::pending::<()>().await;
        drop(slot);
    });
    abort_tx.send(stuck.abort_handle()).expect("the task is waiting");

    let shutdown = tokio::spawn({
        let hub = hub.clone();
        async move { hub.shutdown().await }
    });

    attached_rx.await.expect("the stuck task attached its handle after `closing`");
    // Only now can the grace elapse, so the attach is provably inside the window the fix
    // exists for.
    tokio::time::advance(PUSH_STOP_GRACE * 2).await;

    tokio::time::timeout(GUARD, shutdown)
        .await
        .expect("shutdown must not outlive its grace")
        .expect("shutdown task");

    let outcome = tokio::time::timeout(GUARD, stuck)
        .await
        .expect("the aborted task must end");
    assert!(
        outcome.expect_err("the task was aborted, never completed").is_cancelled(),
        "a task that ignored its queue is force-stopped after the grace"
    );
    assert_eq!(hub.live(), 0, "and its slot is released by the abort");
}

/// A closing hub refuses new upgrades under the SAME guard the registry insert happens
/// under: testing it separately would let an upgrade that passed the test register into
/// an already-drained map and never be closed.
#[tokio::test]
async fn a_closing_hub_refuses_a_new_connection() {
    let hub = hub_with(test_limits());
    hub.shutdown().await;
    assert!(matches!(hub.accept(local()), Err(AcceptError::Closing)));
}

// ---------------------------------------------------------------------------
// The inbound backplane face
// ---------------------------------------------------------------------------

/// The batch is replayed IN ORDER and every `Target` variant reaches the hub — the
/// property the sender pays for by batching instead of issuing a call per message. A
/// message addressed to nobody on this front is a normal skip, not a failure.
#[tokio::test]
async fn deliver_batch_replays_every_target_variant_in_order() {
    let hub = hub_with(test_limits());
    let (slot, _wake) = bound(&hub, local(), "alice");
    assert!(hub.join(slot.id, "raid"));

    let batch = push::encode_batch(&[
        push::Envelope::new(Target::Player("alice".into()), msg("one")),
        push::Envelope::new(Target::Player("nobody".into()), msg("skipped")),
        push::Envelope::new(Target::Group("raid".into()), msg("two")),
        push::Envelope::new(Target::Group("empty".into()), msg("skipped")),
        push::Envelope::new(Target::All, msg("three")),
    ])
    .expect("encode");

    deliver_batch(&hub, &batch).expect("a well-formed batch is replayed");
    assert_eq!(
        queued_payloads(&slot.queue),
        vec!["one".to_string(), "two".to_string(), "three".to_string()],
        "order is the contract, and an unaddressed message is simply skipped"
    );
}

/// `deliver_batch` order is per HUB now: hubs alternate at `recv`, so the contract a
/// producer can rely on is that each hub's frames keep their batch order. Read straight
/// from the lanes, a batch that replayed out of order (reversed, sorted, or fanned out
/// concurrently) puts a hub's frames out of order.
#[tokio::test]
async fn deliver_batch_keeps_each_hubs_order_across_a_mixed_hub_batch() {
    let hub = hub_with(PushLimits { queue_depth: 32, ..test_limits() });
    let (slot, _wake) = bound(&hub, local(), "alice");
    let to = |topic: &str, payload: &str| {
        push::Envelope::new(Target::Player("alice".into()), topic_msg(topic, payload))
    };

    let batch = push::encode_batch(&[
        to("a.x", "a1"),
        to("b.x", "b1"),
        to("a.x", "a2"),
        to("c.x", "c1"),
        to("b.x", "b2"),
        to("a.x", "a3"),
    ])
    .expect("encode");
    deliver_batch(&hub, &batch).expect("a well-formed batch is replayed");

    let lane = |name: &str| lane_payloads(&slot.queue, name);
    assert_eq!(lane("a"), vec!["a1", "a2", "a3"]);
    assert_eq!(lane("b"), vec!["b1", "b2"]);
    assert_eq!(lane("c"), vec!["c1"]);
}

/// A malformed batch is a typed error, not a panic and not a silent success — the peer
/// learns its call failed.
#[tokio::test]
async fn deliver_batch_rejects_a_malformed_batch() {
    let hub = hub_with(test_limits());
    assert!(deliver_batch(&hub, b"not a batch").is_err());
    assert!(deliver_batch(&hub, b"[]").is_ok(), "an empty batch is legal");
}

/// The registration claims exactly the wire method the sender calls. Nothing links the
/// two constants, so a rename on either side answers `UnknownMethod` on every batch — a
/// total push outage that looks exactly like every front being down and fails no boot.
#[test]
fn the_edge_registration_claims_the_push_deliver_method() {
    let mut server = edge::Server::new();
    deliver_registration(hub_with(test_limits())).apply(&mut server);
    assert!(
        server.methods().contains(&DELIVER_METHOD.to_string()),
        "registered methods: {:?}",
        server.methods()
    );
    assert_eq!(DELIVER_METHOD, "push.deliver");
}

// ---------------------------------------------------------------------------
// Client address resolution for the per-IP cap
// ---------------------------------------------------------------------------

/// A forwarded header is honoured ONLY from a trusted peer. From an untrusted one it is
/// ignored, so a forged `X-Forwarded-For` cannot mint a fresh per-IP bucket per
/// connection and defeat the cap entirely.
#[test]
fn a_forwarded_address_is_honoured_only_from_a_trusted_peer() {
    let peer: SocketAddr = "203.0.113.9:4444".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());

    assert_eq!(
        client_ip(&headers, Some(peer), &[]),
        peer.ip(),
        "an untrusted peer's forwarded address is ignored"
    );
    let trusted = httpmw::parse_cidrs("203.0.113.9").expect("cidr");
    assert_eq!(
        client_ip(&headers, Some(peer), &trusted),
        "198.51.100.7".parse::<IpAddr>().unwrap(),
        "a trusted proxy's forwarded address is the client"
    );
    // No `ConnectInfo` collapses every such connection into ONE bucket rather than
    // exempting them from the cap.
    assert_eq!(
        client_ip(&headers, None, &[]),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    );
}

// ---------------------------------------------------------------------------
// Over a REAL socket: the handshake, the typed closes, the re-verify tick
// ---------------------------------------------------------------------------

/// Header-borne credentials bind straight away — the client that CAN set headers never
/// spends a frame on the handshake — and the ack is queued like every other server frame
/// rather than written past the queue.
#[tokio::test]
async fn a_header_credentialed_dial_binds_and_receives_its_ack() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    let id = expect_ack(&mut ws).await;
    assert!(id > 0);

    // The connection is BOUND: it is addressable by the player the bearer verified as.
    let hub = front.push_hub();
    assert_eq!(hub.deliver(&Target::Player("alice".into()), &msg("hello")), 1);
    let frame = next_json(&mut ws).await;
    assert_eq!(frame["type"], "message");
    assert_eq!(frame["topic"], "test.topic");
    assert_eq!(
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(frame["payload"].as_str().unwrap())
                .unwrap()
        )
        .unwrap(),
        "hello"
    );

    server.abort();
}

/// A browser cannot set headers on a WebSocket dial, so the credentials arrive as the
/// first frame — and NEVER from the query string, which lands in access logs, proxy
/// history and browser history.
#[tokio::test]
async fn a_hello_frame_dial_binds_when_no_headers_were_set() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    let mut ws = dial(addr, &[]).await.expect("the upgrade itself needs no credentials");
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "hello", "token": "dev-alice", "api_key": KEY}).to_string(),
    ))
    .await
    .expect("send hello");
    expect_ack(&mut ws).await;
    assert_eq!(front.push_hub().deliver(&Target::Player("alice".into()), &msg("m")), 1);

    server.abort();
}

/// The first frame is the ONE place a client must speak this build's grammar: a group
/// verb or garbage there is a `Protocol` close, and nothing can be joined by a connection
/// with no identity.
#[tokio::test]
async fn a_group_verb_or_garbage_as_the_first_frame_is_a_protocol_close() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    for first in [
        serde_json::json!({"type": "join", "group": "raid"}).to_string(),
        serde_json::json!({"type": "leave", "group": "raid"}).to_string(),
        serde_json::json!({"type": "chat", "text": "hi"}).to_string(),
        "not json at all".to_string(),
    ] {
        let mut ws = dial(addr, &[]).await.expect("upgrade");
        ws.send(ClientMessage::Text(first.clone())).await.expect("send");
        let (code, retryable) = expect_close(&mut ws).await;
        assert_eq!(code, "protocol", "first frame {first:?}");
        assert!(!retryable, "a retry reproduces a grammar error");
    }

    await_live(&front.push_hub(), 0).await;
    server.abort();
}

/// An outage on EITHER credential authority closes retryably at bind time. An accounts or
/// apikeys blip must not read as "your credentials are bad" — that logs every player out
/// at once and they all reconnect into the same blip.
#[tokio::test]
async fn an_unavailable_credential_authority_closes_retryably_at_bind() {
    // (a) the session verifier cannot answer.
    let front = front_with(test_limits(), Arc::new(UnavailableSessions), FakeKeys::demo());
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unavailable");
    assert!(retryable);
    server.abort();

    // (b) the api-key verifier cannot answer.
    let front = front_with(
        test_limits(),
        Arc::new(DevSessionVerifier::new()),
        Arc::new(UnavailableKeys),
    );
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unavailable", "an unavailable key check is not an api-key verdict");
    assert!(retryable);
    server.abort();
}

/// The fourth `Unavailable` arm: the whole admission exceeded the process budget — a hung
/// backend, which the front classes with the outage rather than with a rejection.
#[tokio::test]
async fn an_elapsed_admission_budget_closes_retryably_at_bind() {
    let front = Arc::new(
        FrontDoor::new(
            Arc::new(Slots::new()),
            Arc::new(DevSessionVerifier::new()),
            Arc::new(HungKeys),
            Vec::new(),
        )
        .with_push_limits(test_limits())
        .with_admission_budget(Duration::from_millis(100)),
    );
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unavailable");
    assert!(retryable);

    server.abort();
}

/// A definitively-rejected bearer and a missing one are credential verdicts, and the
/// client is told not to retry them.
#[tokio::test]
async fn a_rejected_or_missing_credential_closes_unretryably() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    for (token, key, expected) in [
        ("not-a-dev-token", KEY, "unauthorized"),
        ("dev-alice", "no-such-key", "api_key"),
    ] {
        let mut ws = dial_authed(addr, token, key).await;
        let (code, retryable) = expect_close(&mut ws).await;
        assert_eq!(code, expected);
        assert!(!retryable);
    }

    // No bearer at all: the api key alone is not an identity.
    let mut ws = dial(addr, &[("x-api-key", KEY)]).await.expect("upgrade");
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "hello", "api_key": KEY}).to_string(),
    ))
    .await
    .expect("send hello");
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unauthorized");
    assert!(!retryable);

    server.abort();
}

/// `/push` runs the key check in `PresenceOnly` mode, so a key whose policy names nothing
/// holds a socket — while the very same key is `Forbidden` for any operation (asserted
/// against the one `check_api_key` above). Nothing is added to any key's policy for a
/// fixed route.
#[tokio::test]
async fn a_policyless_key_holds_a_push_socket() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "dev-alice", POLICYLESS_KEY).await;
    expect_ack(&mut ws).await;

    server.abort();
}

/// The re-verify tick stops a session that stopped being valid. An expired token and a
/// revoked session are indistinguishable here, so the close is retryable — the 401 at
/// re-bind is what stops a genuinely revoked one.
#[tokio::test]
async fn a_revoked_session_closes_the_live_socket_as_session_expired() {
    let sessions = RevokingSessions::new("alice", |_| Ok(None));
    let front = front_with(
        PushLimits {
            reverify_interval: Duration::from_millis(50),
            ..test_limits()
        },
        sessions.clone(),
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "session-token", KEY).await;
    expect_ack(&mut ws).await;
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "session_expired");
    assert!(retryable, "the client cannot tell an expiry from a revocation, so it retries");
    assert!(
        sessions.calls.load(AtomicOrdering::SeqCst) >= 2,
        "the close came from a re-verification, not from the bind"
    );

    server.abort();
}

/// A token that now resolves to a DIFFERENT player is not this connection's credential
/// any more — and this arm must not be mistaken for the revocation one: the connection is
/// bound to a player, not to a token.
#[tokio::test]
async fn a_reverified_token_for_another_player_closes_unauthorized() {
    let sessions = RevokingSessions::new("alice", |_| Ok(Some("mallory".to_string())));
    let front = front_with(
        PushLimits {
            reverify_interval: Duration::from_millis(50),
            ..test_limits()
        },
        sessions,
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "session-token", KEY).await;
    expect_ack(&mut ws).await;
    let (code, _retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unauthorized");

    server.abort();
}

/// An UNAVAILABLE re-verification keeps the connection and retries on the next tick — an
/// accounts blip must not disconnect every player at once — but not forever: the socket
/// ends once it has been stale longer than `max_stale`.
///
/// The assertion on elapsed time is a LOWER bound (the connection outlived several failing
/// ticks), never an upper one.
#[tokio::test]
async fn an_unavailable_reverification_holds_the_connection_until_max_stale() {
    let sessions = RevokingSessions::new("alice", |_| Err(VerifyUnavailable));
    let max_stale = Duration::from_millis(400);
    let front = front_with(
        PushLimits {
            reverify_interval: Duration::from_millis(50),
            max_stale,
            ..test_limits()
        },
        sessions.clone(),
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "session-token", KEY).await;
    expect_ack(&mut ws).await;
    let bound_at = std::time::Instant::now();

    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "unavailable", "an outage is never a session verdict, even at max stale");
    assert!(retryable);
    assert!(
        bound_at.elapsed() >= max_stale,
        "the connection must survive the failing ticks until it is stale: {:?}",
        bound_at.elapsed()
    );
    assert!(
        sessions.calls.load(AtomicOrdering::SeqCst) >= 3,
        "several ticks were attempted and none of them ended the connection early"
    );

    server.abort();
}

/// The stop path a client actually sees: `Gateway::stop` calls exactly this, and the
/// connection ends with a TYPED close it can dispatch on — asserting merely that the
/// socket ended would pass from the cancel alone.
#[tokio::test]
async fn stop_writes_a_typed_close_and_then_the_protocol_close() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    front.push_hub().shutdown().await;

    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "shutdown");
    assert!(retryable, "the client should come back once the process is up again");

    // And the protocol-level close a browser's `CloseEvent` reads.
    let frame = tokio::time::timeout(GUARD, ws.next())
        .await
        .expect("the protocol close arrives")
        .expect("a frame")
        .expect("a frame");
    match frame {
        ClientMessage::Close(Some(close)) => {
            assert_eq!(u16::from(close.code), CloseCode::Shutdown.ws_code());
            assert_eq!(close.reason, CloseCode::Shutdown.reason());
        }
        other => panic!("expected a protocol close, got {other:?}"),
    }
    assert_eq!(front.push_hub().live(), 0);

    server.abort();
}

/// A socket still in its handshake is not addressable, and a broadcast to every bound
/// connection must not reach it: the connection task treats a queued frame as a routing
/// defect and PANICS, which a test counting only recipients cannot see.
#[tokio::test]
async fn a_broadcast_never_reaches_a_socket_still_in_its_handshake() {
    let front = dev_front(PushLimits {
        handshake_grace: Duration::from_millis(300),
        ..test_limits()
    });
    let (addr, server) = serve(&front).await;

    let mut bound_client = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut bound_client).await;
    // Upgraded, credentials never sent: accepted, unbound, sitting in the handshake.
    let mut silent = dial(addr, &[]).await.expect("upgrade");

    assert_eq!(
        front.push_hub().deliver(&Target::All, &msg("broadcast")),
        1,
        "only the bound connection is a participant"
    );
    let frame = next_json(&mut bound_client).await;
    assert_eq!(frame["type"], "message");

    // The silent socket's ONLY frame is its own handshake timeout — never the broadcast,
    // and never a task that died on the routing defect.
    let (code, retryable) = expect_close(&mut silent).await;
    assert_eq!(code, "handshake_timeout");
    assert!(retryable);

    server.abort();
}

/// The client-driven group verbs over the socket, including the negative a budget-less
/// implementation would leak: a refused join keeps the connection, and membership is NOT
/// restored on a reconnect.
#[tokio::test]
async fn group_verbs_over_the_socket_address_and_then_release_a_connection() {
    let front = dev_front(test_limits());
    let (addr, server) = serve(&front).await;
    let hub = front.push_hub();

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    // A refused join (over-long name) is ignored, not fatal — proven by the join that
    // follows it on the SAME connection still working.
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "join", "group": "g".repeat(MAX_GROUP_NAME_BYTES + 1)})
            .to_string(),
    ))
    .await
    .expect("send");
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "join", "group": "raid"}).to_string(),
    ))
    .await
    .expect("send");
    // An unknown verb from a newer client is ignored too.
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "subscribe", "what": "everything"}).to_string(),
    ))
    .await
    .expect("send");

    // The join is observable only once the hub applied it, so poll the registry rather
    // than sleeping for it.
    await_group_size(&hub, "raid", 1).await;
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("in-group")), 1);
    let frame = next_json(&mut ws).await;
    assert_eq!(frame["type"], "message");

    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "leave", "group": "raid"}).to_string(),
    ))
    .await
    .expect("send");
    await_group_size(&hub, "raid", 0).await;
    assert_eq!(hub.deliver(&Target::Group("raid".into()), &msg("nobody")), 0);

    // Re-join, then disconnect: membership dies with the connection and the reconnect
    // starts empty.
    ws.send(ClientMessage::Text(
        serde_json::json!({"type": "join", "group": "raid"}).to_string(),
    ))
    .await
    .expect("send");
    await_group_size(&hub, "raid", 1).await;
    drop(ws);
    await_group_size(&hub, "raid", 0).await;

    let mut again = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut again).await;
    assert_eq!(
        hub.deliver(&Target::Group("raid".into()), &msg("nobody")),
        0,
        "a reconnect rebuilds its membership; the hub never restores it"
    );

    server.abort();
}

/// Waits until group `name` holds `want` members — a poll on OBSERVABLE registry state,
/// bounded by the hang guard, so no assertion depends on how fast a frame crossed a
/// socket.
async fn await_group_size(hub: &Arc<PushHub>, name: &str, want: usize) {
    let deadline = std::time::Instant::now() + GUARD;
    loop {
        let size = hub
            .state
            .lock()
            .unwrap()
            .groups
            .get(name)
            .map(|members| members.len())
            .unwrap_or(0);
        if size == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "group {name:?} still has {size} members, wanted {want}"
        );
        tokio::task::yield_now().await;
    }
}

/// The caps are taken BEFORE the upgrade, so an unauthenticated socket cannot occupy the
/// process for free — and a refusal is an HTTP status, because there is no socket yet to
/// carry a typed close.
#[tokio::test]
async fn a_capacity_refusal_is_an_http_status_before_the_upgrade() {
    let front = dev_front(PushLimits { max_connections: 1, ..test_limits() });
    let (addr, server) = serve(&front).await;

    let mut first = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut first).await;
    assert_eq!(
        dial(addr, &[]).await.expect_err("the second dial is over the cap"),
        503
    );

    // The RELEASE path: the first connection leaves and the slot comes back. A missing
    // decrement wedges the front at its cap until a restart.
    drop(first);
    await_live(&front.push_hub(), 0).await;
    let mut third = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut third).await;

    server.abort();
}

/// A forged `X-Forwarded-For` cannot mint a fresh per-IP bucket: with no trusted proxies
/// both dials resolve to the direct peer and the second is refused. The trusted half of
/// the pair is what makes the assertion non-vacuous — the header IS honoured when the
/// peer is allowed to set it.
#[tokio::test]
async fn a_forged_forwarded_header_cannot_evade_the_per_ip_cap() {
    let untrusting = dev_front(PushLimits { max_per_ip: 1, ..test_limits() });
    let (addr, server) = serve(&untrusting).await;

    let mut first = dial(
        addr,
        &[
            ("authorization", "Bearer dev-alice"),
            ("x-api-key", KEY),
            ("x-forwarded-for", "198.51.100.1"),
        ],
    )
    .await
    .expect("the first dial fits the cap");
    expect_ack(&mut first).await;

    assert_eq!(
        dial(addr, &[("x-forwarded-for", "198.51.100.2")])
            .await
            .expect_err("a different forged address must NOT be a different bucket"),
        503
    );
    server.abort();

    // The same two dials against a front that trusts the loopback peer: now the header is
    // the client address and they are two buckets.
    let trusting = dev_front(PushLimits {
        max_per_ip: 1,
        trusted_proxies: httpmw::parse_cidrs("127.0.0.1").expect("cidr"),
        ..test_limits()
    });
    let (addr, server) = serve(&trusting).await;
    let mut a = dial(
        addr,
        &[
            ("authorization", "Bearer dev-alice"),
            ("x-api-key", KEY),
            ("x-forwarded-for", "198.51.100.1"),
        ],
    )
    .await
    .expect("first bucket");
    expect_ack(&mut a).await;
    let mut b = dial(
        addr,
        &[
            ("authorization", "Bearer dev-bob"),
            ("x-api-key", KEY),
            ("x-forwarded-for", "198.51.100.2"),
        ],
    )
    .await
    .expect("second bucket");
    expect_ack(&mut b).await;

    server.abort();
}

/// Presence over real sockets, observed by a DIFFERENT connection — the audience the
/// broadcast is for.
#[tokio::test]
async fn presence_transitions_are_observed_by_another_connection() {
    let front = dev_front(PushLimits { presence: true, ..test_limits() });
    let (addr, server) = serve(&front).await;

    let mut observer = dial_authed(addr, "dev-observer", KEY).await;
    expect_ack(&mut observer).await;
    // The observer is itself a participant, so its own bind announces to it too.
    assert_eq!(
        presence_payload(&next_json(&mut observer).await),
        (String::from("observer"), true)
    );

    let mut alice = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut alice).await;
    let online = next_json(&mut observer).await;
    assert_eq!(online["topic"], PRESENCE_TOPIC);
    assert_eq!(presence_payload(&online), (String::from("alice"), true));

    // A second device for the same player is NOT a second online.
    let mut alice2 = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut alice2).await;
    drop(alice2);

    drop(alice);
    let offline = next_json(&mut observer).await;
    assert_eq!(
        presence_payload(&offline),
        (String::from("alice"), false),
        "the only further transition is the LAST device leaving"
    );

    server.abort();
}

fn presence_payload(frame: &serde_json::Value) -> (String, bool) {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(frame["payload"].as_str().expect("a payload"))
        .expect("base64");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("presence is JSON");
    (
        v["player_id"].as_str().expect("player_id").to_string(),
        v["online"].as_bool().expect("online"),
    )
}

/// Waits until the hub holds `want` connections, bounded by the hang guard.
async fn await_live(hub: &Arc<PushHub>, want: usize) {
    let deadline = std::time::Instant::now() + GUARD;
    while hub.live() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "the hub still holds {} connections, wanted {want}",
            hub.live()
        );
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// invoke/result: the fixtures
// ---------------------------------------------------------------------------
//
// Two harnesses again, chosen per branch. The admission ORDER inside `Inbound::frame`
// (bucket, probe, limiter, parse, semaphore) and every bound that reads a clock are proven
// by driving `Inbound::frame` directly over a real `Slot`/`FrontDoor` — synchronous, so a
// paused tokio clock is safe (a paused clock over a real TCP socket auto-advances while
// bytes are still in flight). Everything that is `run`/`serve` wiring — the handshake's
// stored credentials, the typed closes, the permit released only after the socket write,
// the shutdown abort — is proven over a real bound listener.

use opsapi::{
    AuthReq, DecodeFn, EncodeFn, Identity, LocalInvoker, LocalOp, OpBinding, Operation, RetryMode,
};

/// What a fake backend op records, and the gate it can hold a call on. `calls` counts every
/// dispatch that reached the op, so "zero backend calls" is an assertion on the fake and not
/// an inference from the absence of an error.
struct Probe {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    /// A gated call proceeds only by taking a permit from here; a test that never adds one
    /// holds the call forever.
    release: Semaphore,
    /// Gated calls whose future was dropped — completed OR cancelled.
    dropped: AtomicUsize,
    dropped_signal: tokio::sync::Notify,
}

impl Probe {
    fn new() -> Arc<Probe> {
        Arc::new(Probe {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: Semaphore::new(0),
            dropped: AtomicUsize::new(0),
            dropped_signal: tokio::sync::Notify::new(),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(AtomicOrdering::SeqCst)
    }
}

struct DropGuard(Arc<Probe>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, AtomicOrdering::SeqCst);
        self.0.dropped_signal.notify_one();
    }
}

/// The `{status, pid, echo}` response every echo-shaped fake op answers with: the verified
/// identity the front threaded, and the request bytes it received.
fn echo_response(ident: &Identity, req: &[u8]) -> Result<Vec<u8>, opsapi::Error> {
    let pid = ident.player_id().ok_or_else(|| opsapi::Error::invalid("no identity"))?;
    Ok(format!(
        r#"{{"status":"Ok","pid":"{pid}","echo":{}}}"#,
        String::from_utf8_lossy(req)
    )
    .into_bytes())
}

/// Counts the call, answers immediately.
fn echo_invoker(probe: Arc<Probe>) -> LocalInvoker {
    Arc::new(move |ident: Identity, req: Vec<u8>| {
        let probe = probe.clone();
        Box::pin(async move {
            probe.calls.fetch_add(1, AtomicOrdering::SeqCst);
            echo_response(&ident, &req)
        })
    })
}

/// Counts the call, signals `entered`, then holds until the test adds a `release` permit.
/// Dropping the call's future (an abort, a timeout) is observable on `dropped`.
fn gated_invoker(probe: Arc<Probe>) -> LocalInvoker {
    Arc::new(move |ident: Identity, req: Vec<u8>| {
        let probe = probe.clone();
        Box::pin(async move {
            probe.calls.fetch_add(1, AtomicOrdering::SeqCst);
            probe.entered.notify_one();
            let _guard = DropGuard(probe.clone());
            probe.release.acquire().await.expect("the gate is never closed").forget();
            echo_response(&ident, &req)
        })
    })
}

/// Answers with a domain error, so the envelope's error arm is compared too.
fn failing_invoker() -> LocalInvoker {
    Arc::new(|_ident: Identity, _req: Vec<u8>| {
        Box::pin(async move { Err::<Vec<u8>, _>(opsapi::Error::not_found("gone")) })
    })
}

fn domain_code_panics() -> opsapi::Error {
    panic!("a Local op's domain code panicked")
}

/// A Local op whose domain code panics inside the invoke task.
fn panicking_invoker() -> LocalInvoker {
    Arc::new(|_ident: Identity, _req: Vec<u8>| {
        Box::pin(async move { Err::<Vec<u8>, _>(domain_code_panics()) })
    })
}

/// Slots carrying each `(method, invoker)` as a full player op (`Operation` + binding +
/// `LocalOp`), i.e. reachable through the route table.
fn ops_slots(ops: Vec<(&str, LocalInvoker)>) -> Arc<Slots> {
    let slots = Arc::new(Slots::new());
    for (method, invoke) in ops {
        slots.contribute(
            opsapi::SLOT,
            Operation {
                method: method.into(),
                verb: "POST".into(),
                path: format!("/{}", method.replace('.', "/")),
                auth: AuthReq::Player,
                success: 200,
                retry_mode: RetryMode::Never,
            },
        );
        let decode: DecodeFn = Arc::new(|body, _path| Ok(body.unwrap_or(b"null").to_vec()));
        let encode: EncodeFn = Arc::new(|resp: &[u8]| Ok((Some(resp.to_vec()), Status::Ok)));
        slots.contribute(
            opsapi::BINDING_SLOT,
            OpBinding { method: method.into(), decode, encode },
        );
        slots.contribute(opsapi::LOCAL_SLOT, LocalOp { method: method.into(), invoke });
    }
    slots
}

fn front_over(
    slots: Arc<Slots>,
    limits: PushLimits,
    verifier: Arc<dyn SessionVerifier>,
    keys: Arc<dyn KeyVerifier>,
) -> Arc<FrontDoor> {
    Arc::new(FrontDoor::new(slots, verifier, keys, Vec::new()).with_push_limits(limits))
}

/// A dev-session front over `ops`.
fn ops_front(limits: PushLimits, ops: Vec<(&str, LocalInvoker)>) -> Arc<FrontDoor> {
    front_over(
        ops_slots(ops),
        limits,
        Arc::new(DevSessionVerifier::new()),
        FakeKeys::demo(),
    )
}

fn invoke_frame(id: i64, method: &str, payload: Option<&str>) -> String {
    match payload {
        Some(payload) => format!(
            r#"{{"type":"invoke","id":{id},"method":"{method}","payload":{payload}}}"#
        ),
        None => format!(r#"{{"type":"invoke","id":{id},"method":"{method}"}}"#),
    }
}

async fn send(ws: &mut Client, text: String) {
    ws.send(ClientMessage::Text(text)).await.expect("the socket accepts a client frame");
}

/// The next server frame, which must be a `result`: `(id, response)`.
async fn next_result(ws: &mut Client) -> (i64, serde_json::Value) {
    let frame = next_json(ws).await;
    assert_eq!(frame["type"], "result", "expected a result frame, got {frame}");
    (frame["id"].as_i64().expect("a numeric id"), frame["response"].clone())
}

/// Waits until the op behind `probe` has been entered `want` times — a `Notify`
/// happens-before, bounded by the hang guard, never a sleep.
async fn wait_calls(probe: &Probe, want: usize) {
    let waited = tokio::time::timeout(GUARD, async {
        loop {
            if probe.calls() >= want {
                return;
            }
            probe.entered.notified().await;
        }
    })
    .await;
    assert!(waited.is_ok(), "the backend saw {} calls, wanted {want}", probe.calls());
}

/// One connection's inbound machinery over a real `Slot`, WITHOUT a socket: what `run`
/// builds for `serve`, minus the `WebSocket`. The queue is read back through the
/// production `recv`.
struct Rig {
    front: Arc<FrontDoor>,
    hub: Arc<PushHub>,
    slot: Slot,
    limits: PushLimits,
}

fn rig(front: Arc<FrontDoor>) -> (Rig, mpsc::Receiver<()>) {
    let hub = front.push_hub();
    let limits = hub.limits().clone();
    let (slot, wake) = accept(&hub, local());
    (Rig { front, hub, slot, limits }, wake)
}

impl Rig {
    fn inbound(&self, api_key: &str) -> Inbound<'_> {
        Inbound::new(
            self.front.clone(),
            &self.slot,
            "dev-alice".to_string(),
            Some(api_key.to_string()),
            &self.limits,
        )
    }

    fn reply_len(&self) -> usize {
        self.slot.queue.state.lock().unwrap().reply.len()
    }
}

/// The next outbound item as JSON, with the permit a `result` carries (the writer's job to
/// drop after the socket write; the test drops it where `serve` would).
async fn take(
    rig: &Rig,
    wake: &mut mpsc::Receiver<()>,
) -> (serde_json::Value, Option<OwnedSemaphorePermit>) {
    let out = tokio::time::timeout(GUARD, rig.slot.queue.recv(wake))
        .await
        .expect("an outbound item within the hang guard")
        .expect("the queue is open");
    match out {
        Outbound::Text(text) => (serde_json::from_str(&text).expect("a frame is JSON"), None),
        Outbound::Result(text, permit) => {
            (serde_json::from_str(&text).expect("a frame is JSON"), Some(permit))
        }
        Outbound::Close(code) => panic!("unexpected close {code:?}"),
    }
}

/// Runs every spawned invoke task to completion.
async fn settle(inbound: &mut Inbound<'_>) {
    let drained = tokio::time::timeout(GUARD, async {
        while let Some(joined) = inbound.tasks.join_next().await {
            joined.expect("an invoke task never panics: the panic is caught and answered");
        }
    })
    .await;
    assert!(drained.is_ok(), "an invoke task did not finish within the hang guard");
}

fn status_of(response: &serde_json::Value) -> &str {
    response["status"].as_str().unwrap_or_else(|| panic!("no status in {response}"))
}

// ---------------------------------------------------------------------------
// invoke/result over a real socket
// ---------------------------------------------------------------------------

/// The RawValue-in-a-tagged-enum branch: a well-formed invoke with an OBJECT payload must
/// parse (via the separate `InvokeFrame`), dispatch with the handshake's bearer, and leave
/// the socket open. An `invoke` folded into the tagged `ClientFrame`, or a probe that fails
/// on a nested object, would close or ignore it.
#[tokio::test]
async fn an_invoke_with_an_object_payload_is_dispatched_and_the_socket_stays_open() {
    let probe = Probe::new();
    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(probe.clone()))]);
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    send(&mut ws, invoke_frame(7, "demo.echo", Some(r#"{"n":1,"nested":{"a":[1,2]}}"#))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!(id, 7);
    assert_eq!(status_of(&response), "Ok");
    assert_eq!(response["pid"], "alice", "the identity is the handshake bearer's player");
    assert_eq!(response["echo"], serde_json::json!({"n":1,"nested":{"a":[1,2]}}));

    // A missing payload is `{}`, an array payload is legal JSON: both answer, on the same
    // still-open socket.
    send(&mut ws, invoke_frame(8, "demo.echo", None)).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!((id, response["echo"].clone()), (8, serde_json::json!({})));
    send(&mut ws, invoke_frame(9, "demo.echo", Some("[1,2]"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!((id, response["echo"].clone()), (9, serde_json::json!([1, 2])));
    assert_eq!(probe.calls(), 3);

    server.abort();
}

/// `response` is `handle_player`'s envelope VERBATIM: for the same input the socket answers
/// exactly what the player-QUIC handler computes — success, a domain error, an unknown
/// method, a missing payload.
#[tokio::test]
async fn the_result_envelope_equals_handle_players_output_for_the_same_input() {
    let probe = Probe::new();
    let front = ops_front(
        test_limits(),
        vec![
            ("demo.echo", echo_invoker(probe.clone())),
            ("demo.gone", failing_invoker()),
        ],
    );
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    let cases: [(&str, Option<&str>); 4] = [
        ("demo.echo", Some(r#"{"n":1}"#)),
        ("demo.echo", None),
        ("demo.gone", Some("{}")),
        ("no.such.op", Some("{}")),
    ];
    for (n, (method, payload)) in cases.into_iter().enumerate() {
        send(&mut ws, invoke_frame(n as i64, method, payload)).await;
        let (id, response) = next_result(&mut ws).await;
        assert_eq!(id, n as i64);

        let direct = front
            .handle_player(
                method.to_string(),
                Some("dev-alice".to_string()),
                Some(KEY.to_string()),
                payload.unwrap_or("{}").as_bytes().to_vec(),
            )
            .await;
        let direct: serde_json::Value = serde_json::from_slice(&direct).expect("an envelope");
        assert_eq!(response, direct, "{method}: the socket answers handle_player's envelope");
        if method == "demo.gone" {
            assert_eq!(status_of(&response), "NotFound", "the error arm is a real non-Ok envelope");
        }
    }

    server.abort();
}

/// A wire-only method (`characters.ownerOf`) IS served in-process — its invoker is
/// registered — but is not an `#[http]`-bound op, so the route table does not list it and an
/// invoke must answer NotFound without ever reaching the invoker. Were the invoke to dispatch
/// by method name instead of through the route table, the counting fake would see a call.
#[tokio::test]
async fn invoking_a_wire_only_method_is_not_found_and_never_reaches_the_backend() {
    let owner = Probe::new();
    let slots = ops_slots(vec![("demo.echo", echo_invoker(Probe::new()))]);
    slots.contribute(
        opsapi::LOCAL_SLOT,
        LocalOp { method: "characters.ownerOf".into(), invoke: echo_invoker(owner.clone()) },
    );
    let front = front_over(
        slots,
        test_limits(),
        Arc::new(DevSessionVerifier::new()),
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    send(&mut ws, invoke_frame(1, "characters.ownerOf", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!(id, 1);
    assert_eq!(
        response,
        serde_json::json!({"status":"NotFound","err":"unknown operation"})
    );
    assert_eq!(owner.calls(), 0, "the wire-only invoker was never called");

    server.abort();
}

/// The api-key POLICY is checked per method on every invoke. `/push` admits with
/// `PresenceOnly`, so a key whose policy names nothing holds a socket; were the invoke to
/// reuse that admission (or drop the key), the op would run. The same front answers the
/// full-policy key.
#[tokio::test]
async fn a_key_without_the_method_in_its_policy_is_forbidden_per_invoke() {
    let probe = Probe::new();
    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(probe.clone()))]);
    let (addr, server) = serve(&front).await;

    let mut limited = dial_authed(addr, "dev-alice", POLICYLESS_KEY).await;
    expect_ack(&mut limited).await;
    send(&mut limited, invoke_frame(1, "demo.echo", Some("{}"))).await;
    let (id, response) = next_result(&mut limited).await;
    assert_eq!(id, 1);
    assert_eq!(status_of(&response), "Forbidden");
    assert_eq!(probe.calls(), 0, "a policy denial never reaches the backend");

    let mut full = dial_authed(addr, "dev-bob", KEY).await;
    expect_ack(&mut full).await;
    send(&mut full, invoke_frame(2, "demo.echo", Some("{}"))).await;
    let (_, response) = next_result(&mut full).await;
    assert_eq!(status_of(&response), "Ok");
    assert_eq!(probe.calls(), 1);

    server.abort();
}

/// A session revoked AFTER the bind is refused on the very next invoke: the bearer is
/// verified per call, not once at admission and not on the (here, hour-long) re-verify tick.
/// The verifier counts its calls, so the per-invoke lookup is observed rather than inferred.
#[tokio::test]
async fn a_bearer_revoked_after_bind_is_unauthorized_on_the_next_invoke() {
    let sessions = RevokingSessions::new("alice", |_| Ok(None));
    let probe = Probe::new();
    let front = front_over(
        ops_slots(vec![("demo.echo", echo_invoker(probe.clone()))]),
        test_limits(),
        sessions.clone(),
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;

    let mut ws = dial_authed(addr, "session-token", KEY).await;
    expect_ack(&mut ws).await;
    assert_eq!(sessions.calls.load(AtomicOrdering::SeqCst), 1, "only the bind so far");

    for id in [1, 2] {
        send(&mut ws, invoke_frame(id, "demo.echo", Some("{}"))).await;
        let (got, response) = next_result(&mut ws).await;
        assert_eq!(got, id);
        assert_eq!(status_of(&response), "Unauthorized");
    }
    assert_eq!(sessions.calls.load(AtomicOrdering::SeqCst), 3, "one lookup per invoke");
    assert_eq!(probe.calls(), 0, "a revoked bearer never reaches the backend");

    server.abort();
}

/// 17 concurrent held invokes against the default in-flight cap of 16: the 17th is
/// answered at once with `Unavailable` and never reaches the backend. Releasing one held call
/// and reading its written result frees a slot, so the next invoke is admitted.
#[tokio::test]
async fn the_17th_concurrent_invoke_is_refused_and_a_written_result_frees_a_slot() {
    let probe = Probe::new();
    let front = ops_front(test_limits(), vec![("demo.hold", gated_invoker(probe.clone()))]);
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;
    let cap = front.push_hub().limits().invoke_max_in_flight;
    assert_eq!(cap, 16);

    for id in 1..=cap as i64 {
        send(&mut ws, invoke_frame(id, "demo.hold", Some("{}"))).await;
    }
    wait_calls(&probe, cap).await;

    send(&mut ws, invoke_frame(17, "demo.hold", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!(id, 17);
    assert_eq!(
        response,
        serde_json::json!({"status":"Unavailable","err":"too many in-flight invokes"})
    );
    assert_eq!(probe.calls(), cap, "the refused invoke never reached the backend");

    probe.release.add_permits(1);
    let (first, response) = next_result(&mut ws).await;
    assert!((1..=cap as i64).contains(&first), "a held call finished: {first}");
    assert_eq!(status_of(&response), "Ok");

    send(&mut ws, invoke_frame(18, "demo.hold", Some("{}"))).await;
    wait_calls(&probe, cap + 1).await;

    probe.release.add_permits(cap);
    let mut rest = std::collections::BTreeSet::new();
    for _ in 0..cap {
        let (id, response) = next_result(&mut ws).await;
        assert_eq!(status_of(&response), "Ok");
        rest.insert(id);
    }
    let mut expected: std::collections::BTreeSet<i64> = (1..=cap as i64).collect();
    expected.remove(&first);
    expected.insert(18);
    assert_eq!(rest, expected, "every admitted invoke answers exactly once");

    server.abort();
}

/// A Local op runs domain code inside the invoke task. A panic in it must still answer the
/// invoke's id with `Internal` "invoke failed", return the permit, and leave the socket
/// open. Without the catch the client waits forever on an id that never comes back.
#[tokio::test]
async fn a_panicking_local_op_answers_internal_returns_its_permit_and_keeps_the_socket() {
    let probe = Probe::new();
    let front = ops_front(
        PushLimits { invoke_max_in_flight: 1, ..test_limits() },
        vec![
            ("demo.boom", panicking_invoker()),
            ("demo.echo", echo_invoker(probe.clone())),
        ],
    );
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    send(&mut ws, invoke_frame(5, "demo.boom", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!(id, 5);
    assert_eq!(
        response,
        serde_json::json!({"status":"Internal","err":"invoke failed"})
    );

    // The one in-flight permit came back: the next invoke is admitted, not refused.
    send(&mut ws, invoke_frame(6, "demo.echo", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!(id, 6);
    assert_eq!(status_of(&response), "Ok");
    assert_eq!(probe.calls(), 1);

    server.abort();
}

/// The per-IP limiter is REAL and shared by every connection from an address: with rate 0
/// and burst 1 (no refill, so nothing here races a clock) the first invoke from
/// 127.0.0.1 passes, and both a second CONNECTION's invoke and the first connection's second
/// invoke get `Unavailable` "rate limited". A different resolved client IP has its own
/// bucket.
#[tokio::test]
async fn the_per_ip_invoke_limiter_is_shared_across_connections_from_one_ip() {
    let probe = Probe::new();
    let limits = PushLimits { invoke_ip_rps: 0, invoke_ip_burst: 1, ..test_limits() }
        .with_trusted_proxies("127.0.0.1")
        .expect("a valid CIDR");
    let front = ops_front(limits, vec![("demo.echo", echo_invoker(probe.clone()))]);
    let (addr, server) = serve(&front).await;
    let limited = |response: &serde_json::Value| {
        *response == serde_json::json!({"status":"Unavailable","err":"rate limited"})
    };

    let mut a = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut a).await;
    send(&mut a, invoke_frame(1, "demo.echo", Some("{}"))).await;
    assert_eq!(status_of(&next_result(&mut a).await.1), "Ok");

    let mut b = dial_authed(addr, "dev-bob", KEY).await;
    expect_ack(&mut b).await;
    send(&mut b, invoke_frame(2, "demo.echo", Some("{}"))).await;
    assert!(limited(&next_result(&mut b).await.1), "a second connection shares the bucket");

    send(&mut a, invoke_frame(3, "demo.echo", Some("{}"))).await;
    assert!(limited(&next_result(&mut a).await.1), "so does the first connection's next invoke");
    assert_eq!(probe.calls(), 1, "a limited invoke never reaches the backend");

    // Another client IP, forwarded by the trusted proxy, has a fresh bucket.
    let mut c = dial(
        addr,
        &[
            ("authorization", "Bearer dev-carol"),
            ("x-api-key", KEY),
            ("x-forwarded-for", "203.0.113.9"),
        ],
    )
    .await
    .unwrap_or_else(|status| panic!("dial refused with HTTP {status}"));
    expect_ack(&mut c).await;
    send(&mut c, invoke_frame(4, "demo.echo", Some("{}"))).await;
    assert_eq!(status_of(&next_result(&mut c).await.1), "Ok");
    assert_eq!(probe.calls(), 2);

    server.abort();
}

/// A connection with an EMPTY `JoinSet` that lives through several re-verify ticks stays
/// open and still answers a later invoke: the `join_next` arm must neither end the loop on
/// an empty set nor starve the other arms.
///
/// KNOWN LIMIT: the `if !tasks.is_empty()` guard on that arm is not itself observable —
/// `select!` disables a branch whose pattern (`Some(joined)`) does not match the `None` an
/// empty set yields, so removing the guard changes no behaviour a test can see. This pins
/// the neighbouring regression (an arm that breaks the loop on `None`); the guard has no
/// failing test.
#[tokio::test]
async fn an_idle_connection_survives_reverify_ticks_and_answers_a_later_invoke() {
    let sessions = RevokingSessions::new("alice", |_| Ok(Some("alice".to_string())));
    let probe = Probe::new();
    let front = front_over(
        ops_slots(vec![("demo.echo", echo_invoker(probe.clone()))]),
        PushLimits { reverify_interval: Duration::from_millis(20), ..test_limits() },
        sessions.clone(),
        FakeKeys::demo(),
    );
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "session-token", KEY).await;
    expect_ack(&mut ws).await;

    // Explicit persisted state, not a sleep: the verifier counts every tick that ran.
    let deadline = std::time::Instant::now() + GUARD;
    while sessions.calls.load(AtomicOrdering::SeqCst) < 4 {
        assert!(std::time::Instant::now() < deadline, "the re-verify ticks never ran");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(front.push_hub().live(), 1, "still connected after the idle ticks");

    send(&mut ws, invoke_frame(1, "demo.echo", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!((id, status_of(&response)), (1, "Ok"));

    server.abort();
}

/// Grammar errors on a live connection close `bad_frame` (non-retryable): no id, an id of
/// the wrong type, no method, a method one byte over `MAX_INVOKE_METHOD_BYTES`. A method of
/// exactly the cap is NOT a grammar error — it reaches the route table and is NotFound.
#[tokio::test]
async fn a_malformed_invoke_closes_bad_frame_and_a_method_at_the_cap_does_not() {
    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(Probe::new()))]);
    let (addr, server) = serve(&front).await;

    let long = "m".repeat(MAX_INVOKE_METHOD_BYTES + 1);
    let bad = [
        r#"{"type":"invoke","method":"demo.echo","payload":{}}"#.to_string(),
        r#"{"type":"invoke","id":"one","method":"demo.echo"}"#.to_string(),
        r#"{"type":"invoke","id":1,"payload":{}}"#.to_string(),
        invoke_frame(1, &long, Some("{}")),
    ];
    for frame in bad {
        let mut ws = dial_authed(addr, "dev-alice", KEY).await;
        expect_ack(&mut ws).await;
        send(&mut ws, frame.clone()).await;
        let (code, retryable) = expect_close(&mut ws).await;
        assert_eq!(code, "bad_frame", "{frame}");
        assert!(!retryable, "a retry reproduces a malformed frame");
    }

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;
    let at_cap = "m".repeat(MAX_INVOKE_METHOD_BYTES);
    send(&mut ws, invoke_frame(2, &at_cap, Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!((id, status_of(&response)), (2, "NotFound"));

    server.abort();
}

/// An inbound frame over `max_frame_bytes` closes `frame_too_large`, RETRYABLE (the
/// reconnect restores every stream) — distinct from `bad_frame`. A frame just under the cap
/// is served. The protocol-level 1009 close is not asserted: the server closes with the
/// rest of the oversize frame unread, so the peer may see a reset before it.
#[tokio::test]
async fn an_over_cap_frame_closes_frame_too_large_retryably() {
    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(Probe::new()))]);
    let (addr, server) = serve(&front).await;
    let cap = front.push_hub().limits().max_frame_bytes;

    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;
    let under = format!(r#"{{"pad":"{}"}}"#, "x".repeat(cap / 2));
    send(&mut ws, invoke_frame(1, "demo.echo", Some(&under))).await;
    assert_eq!(status_of(&next_result(&mut ws).await.1), "Ok", "a frame under the cap is served");

    let over = format!(r#"{{"pad":"{}"}}"#, "x".repeat(cap + 1024));
    send(&mut ws, invoke_frame(2, "demo.echo", Some(&over))).await;
    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "frame_too_large");
    assert!(retryable, "the reconnect restores every hub stream");

    server.abort();
}

/// An unknown `type` on a live connection is still ignored — a newer client speaking a verb
/// this build lacks stays connected — and so is a frame that is not JSON at all.
#[tokio::test]
async fn an_unknown_frame_type_is_ignored_and_the_connection_keeps_answering() {
    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(Probe::new()))]);
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    send(&mut ws, r#"{"type":"future_verb","x":1}"#.to_string()).await;
    send(&mut ws, "not json at all".to_string()).await;
    send(&mut ws, invoke_frame(1, "demo.echo", Some("{}"))).await;
    let (id, response) = next_result(&mut ws).await;
    assert_eq!((id, status_of(&response)), (1, "Ok"));

    server.abort();
}

/// Graceful stop does NOT drain invokes: with one invoke held in the backend, `shutdown`
/// writes the typed `shutdown` close, no `result` frame follows it, and the invoke task is
/// ABORTED — the backend call's future is dropped (observed by a drop guard) though the
/// gate was never released.
#[tokio::test]
async fn shutdown_aborts_an_in_flight_invoke_and_the_close_is_written() {
    let probe = Probe::new();
    let front = ops_front(test_limits(), vec![("demo.hold", gated_invoker(probe.clone()))]);
    let (addr, server) = serve(&front).await;
    let mut ws = dial_authed(addr, "dev-alice", KEY).await;
    expect_ack(&mut ws).await;

    send(&mut ws, invoke_frame(1, "demo.hold", Some("{}"))).await;
    wait_calls(&probe, 1).await;
    assert_eq!(probe.dropped.load(AtomicOrdering::SeqCst), 0, "the call is held, not finished");

    front.push_hub().shutdown().await;

    let (code, retryable) = expect_close(&mut ws).await;
    assert_eq!(code, "shutdown");
    assert!(retryable);
    loop {
        match tokio::time::timeout(GUARD, ws.next()).await {
            Err(_) => panic!("the socket neither ended nor closed within the hang guard"),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(ClientMessage::Close(_)))) => break,
            Ok(Some(Ok(ClientMessage::Text(text)))) => panic!("a frame after the close: {text}"),
            Ok(Some(Ok(_))) => {}
        }
    }

    let aborted = tokio::time::timeout(GUARD, async {
        while probe.dropped.load(AtomicOrdering::SeqCst) == 0 {
            probe.dropped_signal.notified().await;
        }
    })
    .await;
    assert!(aborted.is_ok(), "the in-flight invoke task was aborted");
    assert_eq!(probe.release.available_permits(), 0, "the gate was never released");
    assert_eq!(front.push_hub().live(), 0);

    server.abort();
}

// ---------------------------------------------------------------------------
// Inbound::frame driven directly: admission order, rejection accounting, the clock
// ---------------------------------------------------------------------------

/// Unwritten REJECTION entries are capped at `invoke_max_in_flight`: with every in-flight
/// permit held and the client never reading, the first 16 refusals queue and the 17th closes
/// `capacity` (retryable) without queueing anything.
#[tokio::test]
async fn sixteen_unwritten_rejections_close_capacity_on_the_next() {
    let probe = Probe::new();
    let limits = PushLimits {
        inbound_rps: 1000,
        inbound_burst: 1000,
        invoke_ip_burst: 1000,
        ..test_limits()
    };
    let (rig, _wake) = rig(ops_front(limits, vec![("demo.hold", gated_invoker(probe.clone()))]));
    let cap = rig.limits.invoke_max_in_flight;
    let mut inbound = rig.inbound(KEY);

    for id in 0..cap as i64 {
        assert_eq!(inbound.frame(invoke_frame(id, "demo.hold", None).as_bytes()), None);
    }
    for id in 0..cap as i64 {
        assert_eq!(
            inbound.frame(invoke_frame(100 + id, "demo.hold", None).as_bytes()),
            None,
            "refusal {id} is answered, not fatal"
        );
    }
    assert_eq!(rig.slot.queue.unwritten_rejections(), cap);

    assert_eq!(
        inbound.frame(invoke_frame(999, "demo.hold", None).as_bytes()),
        Some(CloseCode::Capacity)
    );
    assert!(CloseCode::Capacity.retryable());
    assert_eq!(rig.slot.queue.unwritten_rejections(), cap, "the closing refusal queued nothing");
}

/// The false-positive branch: 16 unwritten PERMIT-BACKED results are not rejections. With
/// them holding every in-flight permit, one more invoke is admitted-then-refused — answered,
/// NOT closed. Counting every unwritten reply entry (16 >= 16) would close `capacity` here.
/// Reading one result and dropping its permit, as the writer does, admits the next invoke.
#[tokio::test]
async fn unwritten_results_are_not_rejections_and_never_close_capacity() {
    let probe = Probe::new();
    let limits = PushLimits {
        inbound_rps: 1000,
        inbound_burst: 1000,
        invoke_ip_burst: 1000,
        ..test_limits()
    };
    let (rig, mut wake) = rig(ops_front(limits, vec![("demo.echo", echo_invoker(probe.clone()))]));
    let cap = rig.limits.invoke_max_in_flight;
    let mut inbound = rig.inbound(KEY);

    for id in 0..cap as i64 {
        assert_eq!(inbound.frame(invoke_frame(id, "demo.echo", None).as_bytes()), None);
    }
    settle(&mut inbound).await;
    assert_eq!(rig.reply_len(), cap, "sixteen unwritten, permit-backed results");
    assert_eq!(rig.slot.queue.unwritten_rejections(), 0);

    assert_eq!(
        inbound.frame(invoke_frame(100, "demo.echo", None).as_bytes()),
        None,
        "refused for want of a permit, but not a capacity close"
    );
    assert_eq!(rig.slot.queue.unwritten_rejections(), 1);
    assert_eq!(probe.calls(), cap, "the refused invoke never reached the backend");

    let (first, permit) = take(&rig, &mut wake).await;
    assert_eq!(first["type"], "result");
    drop(permit.expect("a result carries its in-flight permit"));
    assert_eq!(inbound.frame(invoke_frame(101, "demo.echo", None).as_bytes()), None);
    settle(&mut inbound).await;
    assert_eq!(probe.calls(), cap + 1, "the freed permit admitted the next invoke");
}

/// Every inbound frame is charged to the per-connection bucket BEFORE any byte is parsed.
/// Under a paused clock: an exhausted bucket drops a valid invoke (zero backend calls) and a
/// MALFORMED invoke (returns no `bad_frame` close, so it was never parsed) and never touches
/// the shared per-IP limiter (its token is still there when the bucket refills); the refill
/// is clock-driven. As a control, the same malformed frame closes `bad_frame` once tokens are
/// available.
#[tokio::test(start_paused = true)]
async fn an_exhausted_inbound_bucket_drops_unparsed_with_no_backend_call_or_limiter_charge() {
    let probe = Probe::new();
    let limits = PushLimits {
        inbound_rps: 1,
        inbound_burst: 1,
        invoke_ip_rps: 0,
        invoke_ip_burst: 3,
        ..test_limits()
    };
    let (rig, mut wake) = rig(ops_front(limits, vec![("demo.echo", echo_invoker(probe.clone()))]));
    let mut inbound = rig.inbound(KEY);

    // Token 1 of 1: admitted; the limiter spends 1 of 3.
    assert_eq!(inbound.frame(invoke_frame(1, "demo.echo", None).as_bytes()), None);
    settle(&mut inbound).await;
    let (frame, permit) = take(&rig, &mut wake).await;
    drop(permit);
    assert_eq!((frame["id"].as_i64(), status_of(&frame["response"])), (Some(1), "Ok"));
    assert_eq!(probe.calls(), 1);

    // Bucket empty: a valid invoke and a malformed invoke are both dropped.
    assert_eq!(inbound.frame(invoke_frame(2, "demo.echo", None).as_bytes()), None);
    assert_eq!(
        inbound.frame(br#"{"type":"invoke"}"#),
        None,
        "dropped before the parse that would have closed bad_frame"
    );
    assert!(inbound.tasks.is_empty(), "nothing was spawned");
    assert_eq!(probe.calls(), 1, "zero backend calls for a dropped frame");
    assert_eq!(rig.reply_len(), 0, "a dropped frame is not even answered");

    // One virtual second refills one token; the limiter still has its second token, which
    // it would not had the two dropped frames been charged to it.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(inbound.frame(invoke_frame(4, "demo.echo", None).as_bytes()), None);
    settle(&mut inbound).await;
    let (frame, permit) = take(&rig, &mut wake).await;
    drop(permit);
    assert_eq!(
        (frame["id"].as_i64(), status_of(&frame["response"])),
        (Some(4), "Ok"),
        "the refilled frame was admitted: the limiter was not consulted for frames 2 and 3"
    );
    assert_eq!(probe.calls(), 2);
    assert!(rig.hub.invoke_limiter.allow(local()), "one limiter token is left");
    assert!(!rig.hub.invoke_limiter.allow(local()), "and only one");

    // Control: with a token available the same malformed frame IS parsed and closes.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(inbound.frame(br#"{"type":"invoke"}"#), Some(CloseCode::BadFrame));
}

/// Invokes do not spend `MAX_VERBS_PER_INTERVAL` — that budget bounds acquisitions of the
/// registry mutex, which an invoke never takes. 65 invokes (each under the inbound bucket)
/// all get results and leave the budget untouched; group verbs still spend it, and the 65th
/// verb is dropped (a `leave` of a joined group is ignored, so the membership survives).
#[tokio::test]
async fn invokes_do_not_consume_the_verb_budget_but_group_verbs_still_do() {
    let probe = Probe::new();
    let limits = PushLimits {
        inbound_rps: 1000,
        inbound_burst: 1000,
        invoke_ip_rps: 1000,
        invoke_ip_burst: 1000,
        ..test_limits()
    };
    let (rig, mut wake) = rig(ops_front(limits, vec![("demo.echo", echo_invoker(probe.clone()))]));
    let mut inbound = rig.inbound(KEY);

    for id in 0..=MAX_VERBS_PER_INTERVAL as i64 {
        assert_eq!(inbound.frame(invoke_frame(id, "demo.echo", None).as_bytes()), None);
        settle(&mut inbound).await;
        let (frame, permit) = take(&rig, &mut wake).await;
        drop(permit);
        assert_eq!(
            (frame["id"].as_i64(), status_of(&frame["response"])),
            (Some(id), "Ok"),
            "invoke {id} of {} was answered",
            MAX_VERBS_PER_INTERVAL + 1
        );
    }
    assert_eq!(probe.calls(), MAX_VERBS_PER_INTERVAL as usize + 1);
    assert_eq!(inbound.verbs, MAX_VERBS_PER_INTERVAL, "invokes spent none of the verb budget");

    let members = |name: &str| {
        rig.hub.state.lock().unwrap().groups.get(name).map_or(0, |members| members.len())
    };
    assert_eq!(inbound.frame(br#"{"type":"join","group":"g"}"#), None);
    assert_eq!(members("g"), 1);
    assert_eq!(inbound.verbs, MAX_VERBS_PER_INTERVAL - 1, "a group verb spends the budget");
    for n in 0..MAX_VERBS_PER_INTERVAL - 1 {
        let leave = format!(r#"{{"type":"leave","group":"other{n}"}}"#);
        assert_eq!(inbound.frame(leave.as_bytes()), None);
    }
    assert_eq!(inbound.verbs, 0);
    assert_eq!(inbound.frame(br#"{"type":"leave","group":"g"}"#), None);
    assert_eq!(members("g"), 1, "the 65th verb was dropped, so the leave never ran");
}

/// A held invoke past `invoke_timeout` answers `Unavailable` "invoke timed out": the
/// backend call is CANCELLED (its future dropped, observed by the guard) and the permit
/// returns. Paused clock, no I/O: the deadline is virtual time.
#[tokio::test(start_paused = true)]
async fn a_held_invoke_past_the_timeout_answers_unavailable_and_returns_its_permit() {
    let probe = Probe::new();
    let limits = PushLimits { invoke_timeout: Duration::from_secs(1), ..test_limits() };
    let (rig, mut wake) = rig(ops_front(limits, vec![("demo.hold", gated_invoker(probe.clone()))]));
    let mut inbound = rig.inbound(KEY);

    assert_eq!(inbound.frame(invoke_frame(9, "demo.hold", None).as_bytes()), None);
    wait_calls(&probe, 1).await;
    assert_eq!(inbound.in_flight.available_permits(), rig.limits.invoke_max_in_flight - 1);
    assert_eq!(probe.dropped.load(AtomicOrdering::SeqCst), 0);

    tokio::time::advance(Duration::from_millis(1100)).await;
    settle(&mut inbound).await;

    let (frame, permit) = take(&rig, &mut wake).await;
    assert_eq!(frame["id"], 9);
    assert_eq!(
        frame["response"],
        serde_json::json!({"status":"Unavailable","err":"invoke timed out"})
    );
    assert_eq!(probe.dropped.load(AtomicOrdering::SeqCst), 1, "the backend call was cancelled");
    drop(permit);
    assert_eq!(
        inbound.in_flight.available_permits(),
        rig.limits.invoke_max_in_flight,
        "the permit came back with the written result"
    );
}

/// `result_frame` embeds the response as JSON; a backend that answers bytes that are not
/// JSON must become a well-formed `Internal` result rather than an unparseable frame the
/// client cannot correlate.
#[test]
fn a_non_json_backend_response_becomes_an_internal_result_with_the_same_id() {
    let ok: serde_json::Value =
        serde_json::from_str(&result_frame(-4, br#"{"status":"Ok","v":1}"#)).unwrap();
    assert_eq!(ok["type"], "result");
    assert_eq!(ok["id"], -4);
    assert_eq!(ok["response"], serde_json::json!({"status":"Ok","v":1}));

    let bad: serde_json::Value = serde_json::from_str(&result_frame(3, b"not json")).unwrap();
    assert_eq!(bad["type"], "result");
    assert_eq!(bad["id"], 3);
    assert_eq!(
        bad["response"],
        serde_json::json!({"status":"Internal","err":"malformed backend response"})
    );
}

// ---------------------------------------------------------------------------
// PushLimits::from_values for the invoke knobs
// ---------------------------------------------------------------------------

const INVOKE_KNOBS: [&str; 6] = [
    INVOKE_MAX_IN_FLIGHT,
    INVOKE_TIMEOUT_MS,
    INBOUND_RPS,
    INBOUND_BURST,
    INVOKE_IP_RPS,
    INVOKE_IP_BURST,
];

/// `0` and garbage in any invoke knob FAIL STARTUP naming the variable and the value — a
/// zero `PUSH_INBOUND_RPS` would otherwise be a bucket that never refills, silently.
#[test]
fn invoke_knobs_reject_zero_and_garbage_naming_the_variable() {
    for name in INVOKE_KNOBS {
        for bad in ["0", "banana", "-1", "1.5", "10s"] {
            let err = match PushLimits::from_values(values(&[(name, bad)])) {
                Ok(_) => panic!("{name}={bad} must fail startup"),
                Err(err) => err,
            };
            let msg = format!("{err:#}");
            assert!(
                msg.contains(name) && msg.contains(bad),
                "the failure must name the variable and the value it refused: {msg}"
            );
        }
    }
}

/// Unset and blank invoke knobs keep the defaults (and the frame cap default is 128 KiB);
/// set knobs land on their OWN field.
#[test]
fn invoke_knobs_default_when_blank_and_each_parse_onto_its_own_field() {
    let d = PushLimits::default();
    assert_eq!(d.invoke_max_in_flight, 16);
    assert_eq!(d.invoke_timeout, Duration::from_millis(30_000));
    assert_eq!(d.inbound_rps, 10);
    assert_eq!(d.inbound_burst, 20);
    assert_eq!(d.invoke_ip_rps, 20);
    assert_eq!(d.invoke_ip_burst, 40);
    assert_eq!(d.max_frame_bytes, 128 * 1024);
    for limits in [
        PushLimits::from_values(none()).expect("unset is the default"),
        PushLimits::from_values(values(&[
            (INVOKE_MAX_IN_FLIGHT, ""),
            (INVOKE_TIMEOUT_MS, "  "),
            (INBOUND_RPS, ""),
            (INBOUND_BURST, ""),
            (INVOKE_IP_RPS, ""),
            (INVOKE_IP_BURST, ""),
        ]))
        .expect("blank is unset"),
    ] {
        assert_eq!(limits.invoke_max_in_flight, d.invoke_max_in_flight);
        assert_eq!(limits.invoke_timeout, d.invoke_timeout);
        assert_eq!(limits.inbound_rps, d.inbound_rps);
        assert_eq!(limits.inbound_burst, d.inbound_burst);
        assert_eq!(limits.invoke_ip_rps, d.invoke_ip_rps);
        assert_eq!(limits.invoke_ip_burst, d.invoke_ip_burst);
        assert_eq!(limits.max_frame_bytes, d.max_frame_bytes);
    }

    let limits = PushLimits::from_values(values(&[
        (INVOKE_MAX_IN_FLIGHT, "21"),
        (INVOKE_TIMEOUT_MS, "22"),
        (INBOUND_RPS, "23"),
        (INBOUND_BURST, "24"),
        (INVOKE_IP_RPS, "25"),
        (INVOKE_IP_BURST, "26"),
    ]))
    .expect("a fully-specified, valid set parses");
    assert_eq!(limits.invoke_max_in_flight, 21);
    assert_eq!(limits.invoke_timeout, Duration::from_millis(22));
    assert_eq!(limits.inbound_rps, 23);
    assert_eq!(limits.inbound_burst, 24);
    assert_eq!(limits.invoke_ip_rps, 25);
    assert_eq!(limits.invoke_ip_burst, 26);
}

/// The per-IP invoke limiter is consulted ONLY for invoke frames. With rate 0 / burst 1 the
/// bucket holds exactly one token and never refills: a join, an unknown verb and malformed
/// JSON must not spend it, so the one invoke that follows is still answered `Ok`, and the
/// token is spent afterwards. A limiter charged before the invoke probe (or on every frame)
/// would have emptied the bucket and answered "rate limited".
#[tokio::test]
async fn non_invoke_frames_never_touch_the_per_ip_invoke_limiter() {
    let probe = Probe::new();
    let limits = PushLimits { invoke_ip_rps: 0, invoke_ip_burst: 1, ..test_limits() };
    let (rig, mut wake) = rig(ops_front(limits, vec![("demo.echo", echo_invoker(probe.clone()))]));
    let mut inbound = rig.inbound(KEY);

    for frame in [r#"{"type":"join","group":"g"}"#, r#"{"type":"future_verb"}"#, "not json"] {
        assert_eq!(inbound.frame(frame.as_bytes()), None);
    }
    assert_eq!(inbound.frame(invoke_frame(1, "demo.echo", None).as_bytes()), None);
    settle(&mut inbound).await;

    let (frame, permit) = take(&rig, &mut wake).await;
    drop(permit);
    assert_eq!(frame["type"], "result");
    assert_eq!(status_of(&frame["response"]), "Ok", "the token was still there: {frame}");
    assert_eq!(probe.calls(), 1);
    assert!(
        !rig.hub.invoke_limiter.allow(local()),
        "the invoke spent the one token, so the limiter is consulted for invokes"
    );
}

/// `PushHub::new` names its limiter `push_invoke`: the series exists (at zero) from
/// construction, so a hub that passed another name — or the HTTP limiter's — would leave no
/// `limiter="push_invoke"` series behind.
#[test]
fn the_hub_registers_its_limiter_series_as_push_invoke() {
    let _hub = hub_with(test_limits());
    let families = httpmw::table_saturation_collector().collect();
    let labelled = families
        .iter()
        .flat_map(|family| family.get_metric())
        .any(|metric| {
            metric.get_label().iter().any(|l| l.get_name() == "limiter" && l.get_value() == "push_invoke")
        });
    assert!(labelled, "no limiter=\"push_invoke\" series after constructing a PushHub");
}

/// `Gateway::start` spawns the invoke-limiter eviction task and `stop` aborts it. The task
/// owns a clone of the limiter, so the limiter's strong count is the observable: 1 before
/// start, 2 while the task lives, 1 again once `stop` has aborted and awaited it. A `start`
/// that never spawns leaves it at 1; a `stop` that drops the handle without aborting leaves
/// it at 2.
#[tokio::test]
async fn gateway_start_spawns_the_invoke_limiter_eviction_and_stop_aborts_it() {
    use lifecycle::Module as _;

    let front = ops_front(test_limits(), vec![("demo.echo", echo_invoker(Probe::new()))]);
    let gw = crate::Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    let _ = gw.front_door.set(front.clone());
    let hub = front.push_hub();
    assert_eq!(Arc::strong_count(&hub.invoke_limiter), 1, "fixture: no task holds the limiter yet");
    let baseline = 1;

    let ctx = lifecycle::Context::new();
    gw.start(&ctx).await.expect("start in the slots topology");
    assert!(gw.limiter_eviction.lock().unwrap().is_some(), "start keeps the eviction handle");
    assert_eq!(Arc::strong_count(&hub.invoke_limiter), baseline + 1, "the task holds the limiter");

    gw.stop(&ctx).await.expect("stop");
    assert!(gw.limiter_eviction.lock().unwrap().is_none(), "stop takes the handle");
    assert_eq!(Arc::strong_count(&hub.invoke_limiter), baseline, "the aborted task released it");
}

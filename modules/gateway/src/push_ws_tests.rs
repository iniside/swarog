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

/// The frames sitting in a connection's queue, WITHOUT consuming them — a synchronous
/// read of the ring under its own mutex, so no test has to await a connection task that
/// does not exist.
fn queued(queue: &ConnQueue) -> Vec<String> {
    let state = queue.state.lock().unwrap();
    state.ring.iter().map(|frame| frame.to_string()).collect()
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

    assert!(slot.queue.push(Arc::from("pending")), "a live queue accepts");
    slot.queue.close(CloseCode::Replaced);
    slot.queue.close(CloseCode::Shutdown);
    assert_eq!(queued_close(&slot.queue), Some(CloseCode::Replaced));
    assert!(
        !slot.queue.push(Arc::from("too late")),
        "a closing connection refuses the frame, so it is not counted as delivered"
    );

    match slot.queue.recv(&mut wake).await {
        Some(Outbound::Close(code)) => assert_eq!(code, CloseCode::Replaced),
        other => panic!("the close must preempt the pending frame, got {:?}", other.is_some()),
    }
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

//! `GET /push` — the WebSocket transport half of the push hub: the upgrade, the
//! credential handshake, the per-connection registry and the per-connection task.
//!
//! It owns SOCKETS, not the hub model (`core/push` owns `ConnId`/`Target`/`Message`) and
//! not target resolution. What lives here is everything that would otherwise be spread
//! across an accept path: the aggregate bounds, the one place a connection is inserted
//! and removed, and the task that reads/writes one socket.
//!
//! **Auth happens AFTER the upgrade, deliberately.** A browser cannot set headers on a
//! WebSocket dial, so a bearer must also be accepted as the first frame; and a refusal
//! must be a typed close the client can act on (`retryable`) rather than an HTTP status
//! it never sees. Credentials are read from `Authorization`/`X-Api-Key` when present and
//! from that first frame otherwise — NEVER from the query string, which lands in access
//! logs, proxy history and browser history.
//!
//! **The aggregate caps are taken BEFORE the upgrade** ([`PushHub::accept`]) so an
//! unauthenticated socket cannot sit in the handshake for free: a connection occupies its
//! slot from the accept to the moment its [`Slot`] guard drops, whether it ever
//! authenticated or not.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use push::ConnId;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::keys::{KeyCheck, KeyDenial};
use crate::{AdmissionDenial, FrontDoor};

/// Grace [`PushHub::shutdown`] gives every live connection to write its typed close
/// before the remaining tasks are aborted.
///
/// INVARIANT (`Gateway::stop`'s whole budget, re-derived — see [`crate::DESCRIBE_STOP_GRACE`]):
///
/// ```text
///   PUSH_STOP_GRACE       500ms
/// + DESCRIBE_STOP_GRACE  2000ms
/// + POOL_STOP_BUDGET     2000ms
/// = 4500ms  <  5000ms = MODULE_STOP_GRACE_MS (default)   → 500ms headroom
/// ```
///
/// 500ms is sized for the write it covers, not for a stalled peer: each task only has to
/// put one small close frame on an already-open socket, and a peer that cannot absorb it
/// is exactly what the abort after this grace is for.
const PUSH_STOP_GRACE: Duration = Duration::from_millis(500);

/// The aggregate bounds and deadlines of the `/push` surface. Constructed by a
/// composition root (`cmd/server`, `cmd/gateway-svc`) from env and handed to the module
/// via `Gateway::with_push_limits`; the module itself never reads env. The [`Default`]
/// values apply to any process whose root wired none.
#[derive(Clone, Debug)]
pub struct PushLimits {
    /// Sockets this process serves at once, authenticated or still handshaking.
    pub max_connections: usize,
    /// Sockets one resolved client IP may hold (see [`PushLimits::trusted_proxies`]).
    pub max_per_ip: usize,
    /// Sockets one player may hold across devices. The longest-bound one is closed when a
    /// new one would exceed this, so a player's newest device always connects.
    pub max_per_player: usize,
    /// Frames one connection may have pending before the queue starts dropping its
    /// OLDEST message.
    pub queue_depth: usize,
    /// Inbound message and frame cap applied to the upgrade. axum's own defaults are
    /// 64 MiB/16 MiB and nothing else bounds a socket once it is upgraded.
    pub max_frame_bytes: usize,
    /// How long a socket may stay unauthenticated after the upgrade.
    pub handshake_grace: Duration,
    /// Bound on one outbound write, so a peer that stops reading cannot pin the task.
    pub write_deadline: Duration,
    /// How often a live connection's bind-time bearer is re-verified (and a liveness
    /// ping is written).
    pub reverify_interval: Duration,
    /// How long a connection may keep running while re-verification is UNAVAILABLE.
    /// An accounts outage must not log every player out, so an unavailable verdict
    /// keeps the connection — but not forever.
    pub max_stale: Duration,
    /// The trusted-proxy set the per-IP cap resolves a client address against. An
    /// `X-Forwarded-For` from an UNTRUSTED direct peer is ignored, so a forged header
    /// cannot mint a fresh bucket per connection.
    pub trusted_proxies: Vec<IpNet>,
}

impl Default for PushLimits {
    fn default() -> PushLimits {
        PushLimits {
            max_connections: 10_000,
            max_per_ip: 64,
            max_per_player: 8,
            queue_depth: 64,
            max_frame_bytes: 32 * 1024,
            handshake_grace: Duration::from_secs(10),
            write_deadline: Duration::from_secs(10),
            reverify_interval: Duration::from_secs(60),
            max_stale: Duration::from_secs(900),
            trusted_proxies: Vec::new(),
        }
    }
}

impl PushLimits {
    pub fn new() -> PushLimits {
        PushLimits::default()
    }

    /// Parses a `TRUSTED_PROXY_CIDRS`-shaped list into [`PushLimits::trusted_proxies`]
    /// through `httpmw`'s parser — the same authority `core/app`'s rate limiter uses, so
    /// the two cannot disagree about which peers may forward a client address.
    pub fn with_trusted_proxies(mut self, csv: &str) -> anyhow::Result<PushLimits> {
        self.trusted_proxies = httpmw::parse_cidrs(csv)
            .map_err(|e| anyhow::anyhow!("push: parse trusted proxy CIDRs: {e}"))?;
        Ok(self)
    }

    /// Builds the limits from configuration VALUES, `get` returning what the composition
    /// root found for each name. It takes a lookup rather than reading the environment so
    /// the module never touches env (a checker builds these with no wiring at all) and so
    /// the failure branches below are reachable without mutating process env — the
    /// `admission_budget_from_value` precedent, which each front main also drives from a
    /// raw value.
    ///
    /// A name that is ABSENT or blank keeps this build's default. A name that is PRESENT
    /// but unusable — unparseable, or `0` for a cap or a deadline where zero cannot mean
    /// "disabled" — FAILS STARTUP naming the offender. **This is deliberately stricter
    /// than the precedent it borrows its shape from**: `admission_budget_from_value` falls
    /// back to its default on garbage, whereas a bound an operator typed and got silently
    /// dropped is a cap they believe is in force and is not.
    pub fn from_values(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<PushLimits> {
        fn value(raw: Option<String>) -> Option<String> {
            raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        }
        fn count(name: &str, raw: Option<String>, current: usize) -> anyhow::Result<usize> {
            match value(raw) {
                None => Ok(current),
                Some(v) => match v.parse::<usize>() {
                    Ok(0) | Err(_) => anyhow::bail!(
                        "{name}={v:?} is invalid: expected a positive integer (unset it \
                         for the default of {current})"
                    ),
                    Ok(n) => Ok(n),
                },
            }
        }
        fn ms(name: &str, raw: Option<String>, current: Duration) -> anyhow::Result<Duration> {
            match value(raw) {
                None => Ok(current),
                Some(v) => match v.parse::<u64>() {
                    Ok(0) | Err(_) => anyhow::bail!(
                        "{name}={v:?} is invalid: expected a positive number of \
                         milliseconds (unset it for the default of {}ms)",
                        current.as_millis()
                    ),
                    Ok(n) => Ok(Duration::from_millis(n)),
                },
            }
        }

        let d = PushLimits::default();
        let limits = PushLimits {
            max_connections: count(
                MAX_CONNECTIONS,
                get(MAX_CONNECTIONS),
                d.max_connections,
            )?,
            max_per_ip: count(MAX_PER_IP, get(MAX_PER_IP), d.max_per_ip)?,
            max_per_player: count(MAX_PER_PLAYER, get(MAX_PER_PLAYER), d.max_per_player)?,
            queue_depth: count(QUEUE_DEPTH, get(QUEUE_DEPTH), d.queue_depth)?,
            max_frame_bytes: count(MAX_FRAME_BYTES, get(MAX_FRAME_BYTES), d.max_frame_bytes)?,
            handshake_grace: ms(HANDSHAKE_MS, get(HANDSHAKE_MS), d.handshake_grace)?,
            write_deadline: ms(WRITE_MS, get(WRITE_MS), d.write_deadline)?,
            reverify_interval: ms(REVERIFY_MS, get(REVERIFY_MS), d.reverify_interval)?,
            max_stale: ms(MAX_STALE_MS, get(MAX_STALE_MS), d.max_stale)?,
            ..d
        };
        // The SAME trusted-proxy set `core/app`'s rate limiter resolves a client IP
        // against: a per-IP cap that honoured an untrusted peer's `X-Forwarded-For` would
        // be defeated by a forged header.
        limits.with_trusted_proxies(&get(TRUSTED_PROXIES).unwrap_or_default())
    }
}

/// The names [`PushLimits::from_values`] looks up. Public so a composition root reads the
/// environment under exactly these names and cannot drift from the parser.
pub const MAX_CONNECTIONS: &str = "PUSH_MAX_CONNECTIONS";
pub const MAX_PER_IP: &str = "PUSH_MAX_CONNECTIONS_PER_IP";
pub const MAX_PER_PLAYER: &str = "PUSH_MAX_CONNECTIONS_PER_PLAYER";
pub const QUEUE_DEPTH: &str = "PUSH_QUEUE_DEPTH";
pub const MAX_FRAME_BYTES: &str = "PUSH_MAX_FRAME_BYTES";
pub const HANDSHAKE_MS: &str = "PUSH_HANDSHAKE_TIMEOUT_MS";
pub const WRITE_MS: &str = "PUSH_WRITE_TIMEOUT_MS";
pub const REVERIFY_MS: &str = "PUSH_REVERIFY_INTERVAL_MS";
pub const MAX_STALE_MS: &str = "PUSH_MAX_STALE_MS";
/// Shared with `core/app`'s rate limiter — the one trusted-proxy set per process.
pub const TRUSTED_PROXIES: &str = "TRUSTED_PROXY_CIDRS";

// ---------------------------------------------------------------------------
// The wire frames
// ---------------------------------------------------------------------------

/// A client→server frame. An unknown `type` deserializes into [`ClientFrame::Other`],
/// which a LIVE connection ignores (a newer client speaking a verb this build does not
/// have stays connected) but which the HANDSHAKE rejects: the first frame is the one
/// place a client must speak this build's grammar.
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    /// The handshake frame, carrying the credentials a browser cannot put in headers.
    Hello {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        api_key: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// A server→client frame.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame<'a> {
    /// The handshake succeeded; the connection is registered under this id.
    Ack { connection_id: ConnId },
    /// The connection is ending, and why.
    Close {
        code: CloseCode,
        retryable: bool,
        reason: &'a str,
    },
}

/// Why a connection is being closed. The client dispatches on this, so the names are a
/// contract; `retryable` is the only part it must act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CloseCode {
    /// No bearer, or one the session verifier definitively rejected at bind time.
    Unauthorized,
    /// No api key, or one the store definitively does not know.
    ApiKey,
    /// A credential verifier could not answer (accounts/apikeys blip, or the admission
    /// budget elapsed). Never a credential verdict.
    Unavailable,
    /// No credentials arrived within the handshake grace.
    HandshakeTimeout,
    /// A malformed handshake frame.
    Protocol,
    /// An aggregate cap refused the connection.
    Capacity,
    /// This player connected from more devices than the per-player cap allows and this
    /// was the oldest.
    Replaced,
    /// Re-verification stopped accepting the bind-time bearer — an expired token and a
    /// revoked session are indistinguishable here, hence retryable.
    SessionExpired,
    /// The process is stopping.
    Shutdown,
}

impl CloseCode {
    /// Whether the client should reconnect. `false` means the connection failed for a
    /// reason a retry reproduces: reconnecting would spin.
    ///
    /// [`CloseCode::Replaced`] is deliberately NOT retryable even though a retry would
    /// succeed: the evicted device reconnecting would evict the next-oldest, and a pair
    /// of devices over the cap would evict each other forever.
    fn retryable(self) -> bool {
        match self {
            CloseCode::Unauthorized
            | CloseCode::ApiKey
            | CloseCode::Protocol
            | CloseCode::Replaced => false,
            CloseCode::Unavailable
            | CloseCode::HandshakeTimeout
            | CloseCode::Capacity
            | CloseCode::SessionExpired
            | CloseCode::Shutdown => true,
        }
    }

    /// The RFC 6455 close code carried on the protocol-level close frame, for a client
    /// that reads only that (a browser's `CloseEvent`).
    fn ws_code(self) -> u16 {
        match self {
            CloseCode::Unauthorized | CloseCode::ApiKey | CloseCode::Protocol => 1008,
            CloseCode::Replaced | CloseCode::SessionExpired => 1000,
            CloseCode::Unavailable | CloseCode::HandshakeTimeout | CloseCode::Capacity => 1013,
            CloseCode::Shutdown => 1001,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            CloseCode::Unauthorized => "unauthorized",
            CloseCode::ApiKey => "missing or invalid api key",
            CloseCode::Unavailable => "credential verification unavailable",
            CloseCode::HandshakeTimeout => "no handshake within the grace period",
            CloseCode::Protocol => "malformed handshake frame",
            CloseCode::Capacity => "push capacity exhausted",
            CloseCode::Replaced => "replaced by a newer connection for this player",
            CloseCode::SessionExpired => "session no longer valid",
            CloseCode::Shutdown => "server shutting down",
        }
    }

    fn frame(self) -> String {
        serde_json::to_string(&ServerFrame::Close {
            code: self,
            retryable: self.retryable(),
            reason: self.reason(),
        })
        .expect("close frame serialization cannot fail")
    }
}

/// Maps a credential denial onto a close code. The one mapping both the bind-time
/// admission and the re-verify tick use, so `/push` cannot classify an outage as a
/// rejection on one path and not the other: an unavailable verifier NEVER becomes a
/// credential verdict.
fn close_for(denial: &AdmissionDenial) -> CloseCode {
    match denial {
        AdmissionDenial::Key(k) => match k {
            KeyDenial::Unavailable => CloseCode::Unavailable,
            _ => CloseCode::ApiKey,
        },
        AdmissionDenial::MissingBearer | AdmissionDenial::InvalidSession => {
            CloseCode::Unauthorized
        }
        AdmissionDenial::SessionUnavailable | AdmissionDenial::Timeout => CloseCode::Unavailable,
    }
}

// ---------------------------------------------------------------------------
// The per-connection queue
// ---------------------------------------------------------------------------

/// What the connection task pulls out of its queue.
enum Outbound {
    Text(String),
    Close(CloseCode),
}

/// One connection's bounded outbound queue, dropping its OLDEST message when full.
///
/// A `tokio` mpsc cannot drop its head, and dropping the NEWEST is the wrong end: the
/// durable copy of anything that matters is in the producing module's tables and a push
/// message only says "refetch", so the freshest message is the one worth keeping. The
/// deque is guarded by a `std::sync::Mutex` held for a push/pop and never across an
/// await; a capacity-1 mpsc carries the WAKEUP only, so a coalesced signal can never
/// lose a message (the reader always drains the deque before it waits again).
struct ConnQueue {
    state: Mutex<QueueState>,
    wake: mpsc::Sender<()>,
    capacity: usize,
}

struct QueueState {
    ring: VecDeque<String>,
    /// Set once; preempts any pending frame — a close is the one frame worth blocking
    /// the queue's backlog for.
    close: Option<CloseCode>,
}

impl ConnQueue {
    fn new(capacity: usize) -> (Arc<ConnQueue>, mpsc::Receiver<()>) {
        let (wake, rx) = mpsc::channel(1);
        let queue = ConnQueue {
            state: Mutex::new(QueueState {
                ring: VecDeque::new(),
                close: None,
            }),
            wake,
            capacity: capacity.max(1),
        };
        (Arc::new(queue), rx)
    }

    /// Offers one frame, dropping the OLDEST pending one when the queue is full.
    /// Synchronous and non-blocking by contract: a producer may be holding a database
    /// transaction's connection.
    fn push(&self, text: String) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.close.is_some() {
                return;
            }
            while state.ring.len() >= self.capacity {
                state.ring.pop_front();
                DROPS.record();
            }
            state.ring.push_back(text);
        }
        let _ = self.wake.try_send(());
    }

    /// Ends the connection with `code`. Idempotent — the FIRST reason wins, so a
    /// shutdown landing on an already-closing connection cannot relabel it.
    fn close(&self, code: CloseCode) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.close.is_some() {
                return;
            }
            state.close = Some(code);
        }
        let _ = self.wake.try_send(());
    }

    /// The next outbound item, awaiting one if the queue is empty.
    ///
    /// Cancel-safe: everything before the `await` is synchronous, so a lost race in a
    /// `select!` cannot consume an item. A coalesced wakeup cannot strand a message
    /// either — the deque is drained before each wait, so a wakeup only matters while it
    /// is empty.
    async fn recv(&self, wake: &mut mpsc::Receiver<()>) -> Option<Outbound> {
        loop {
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(code) = state.close {
                    return Some(Outbound::Close(code));
                }
                if let Some(text) = state.ring.pop_front() {
                    return Some(Outbound::Text(text));
                }
            }
            wake.recv().await?;
        }
    }
}

/// The process-wide drop counter. A drop is invisible by construction — nobody is
/// waiting on the frame — so counting it here, at the one place a frame is discarded, is
/// the only signal an operator gets.
static DROPS: Drops = Drops {
    total: AtomicU64::new(0),
    warned: AtomicBool::new(false),
};

/// Counts dropped frames. The first drop warns, every later one logs at `debug!` with the
/// running total: a stalled client would otherwise emit one warning per produced message.
struct Drops {
    total: AtomicU64,
    warned: AtomicBool,
}

impl Drops {
    fn record(&self) {
        let total = self.total.fetch_add(1, Ordering::Relaxed) + 1;
        if self.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!(dropped_total = total, "push: dropped the oldest queued frame");
        } else {
            tracing::warn!(
                dropped_total = total,
                "push: connection queue full, dropped the oldest frame (further \
                 occurrences log at debug)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The connection registry
// ---------------------------------------------------------------------------

/// Every connection this process owns, under ONE lock.
///
/// One `Mutex` over one struct, not a lock per map: a fan-out walking the groups while a
/// disconnect edits the per-player index is a real inversion, and three independent locks
/// would make the order in which they are taken the thing that decides whether it
/// deadlocks. Nothing here is ever held across an `.await` — a caller clones what it
/// needs out, drops the guard, then awaits.
pub(crate) struct PushHub {
    limits: PushLimits,
    state: Mutex<HubState>,
    /// Woken every time a connection releases its slot, so [`PushHub::shutdown`] waits
    /// on the drain instead of polling for it.
    drained: tokio::sync::Notify,
}

struct HubState {
    /// Set by [`PushHub::shutdown`]. Tested and the insert performed under the SAME
    /// guard in [`PushHub::accept`] — testing it separately would let an upgrade that
    /// passed the test register into an already-drained map and never be closed.
    closing: bool,
    conns: HashMap<ConnId, Conn>,
    /// Connections per player in BIND order — [`PushHub::bind`] is the only writer, and
    /// it runs after the handshake, so this is not accept order: a socket that spent its
    /// whole handshake grace binds behind one that upgraded later and bound immediately.
    /// The cap evicts the front, i.e. the connection that has been a live PLAYER
    /// connection longest, which is the one this ordering is meant to name; an unbound
    /// socket carries no player identity and is bounded by the handshake grace instead.
    by_player: HashMap<String, VecDeque<ConnId>>,
    per_ip: HashMap<IpAddr, usize>,
    /// Group membership, filled by the hub's join/leave verbs. Cleared per connection
    /// here because membership dies with the connection.
    groups: HashMap<String, HashSet<ConnId>>,
}

struct Conn {
    /// `None` until the handshake binds an identity to the socket.
    player: Option<String>,
    queue: Arc<ConnQueue>,
    /// Set by the connection task before it does anything else, so a shutdown can force
    /// down a task that will not observe its queue.
    abort: Option<AbortHandle>,
    groups: HashSet<String>,
}

/// Why an upgrade was refused before it happened.
enum AcceptError {
    Closing,
    Full,
    IpFull,
}

impl PushHub {
    pub(crate) fn new(limits: PushLimits) -> PushHub {
        PushHub {
            limits,
            state: Mutex::new(HubState {
                closing: false,
                conns: HashMap::new(),
                by_player: HashMap::new(),
                per_ip: HashMap::new(),
                groups: HashMap::new(),
            }),
            drained: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn limits(&self) -> &PushLimits {
        &self.limits
    }

    /// Takes a connection slot for `ip` and mints its [`ConnId`]. The returned [`Slot`]
    /// owns the release path: dropping it — on a completed connection, a failed upgrade,
    /// or a panicking task — frees the aggregate counters and removes the connection
    /// from every index.
    fn accept(self: &Arc<Self>, ip: IpAddr) -> Result<(Slot, mpsc::Receiver<()>), AcceptError> {
        let (queue, wake) = ConnQueue::new(self.limits.queue_depth);
        let id = ConnId::mint();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closing {
                return Err(AcceptError::Closing);
            }
            if state.conns.len() >= self.limits.max_connections {
                return Err(AcceptError::Full);
            }
            let per_ip = state.per_ip.entry(ip).or_insert(0);
            if *per_ip >= self.limits.max_per_ip {
                return Err(AcceptError::IpFull);
            }
            *per_ip += 1;
            state.conns.insert(
                id,
                Conn {
                    player: None,
                    queue: queue.clone(),
                    abort: None,
                    groups: HashSet::new(),
                },
            );
        }
        Ok((
            Slot {
                hub: self.clone(),
                id,
                ip,
                queue,
            },
            wake,
        ))
    }

    fn attach_abort(&self, id: ConnId, abort: AbortHandle) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = state.conns.get_mut(&id) {
            conn.abort = Some(abort);
        }
    }

    /// Binds a verified player to an accepted connection, enforcing the per-player cap by
    /// evicting the longest-bound one (see [`HubState::by_player`] on why that is not the
    /// same as the earliest-accepted one). Returns the evicted connection's queue (if any) for the
    /// caller to close AFTER the guard is dropped.
    fn bind(&self, id: ConnId, player: &str) -> Option<Arc<ConnQueue>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        {
            // A `None` here means the slot was released underneath us; the task is about
            // to end anyway, so there is nothing to bind and nothing to evict.
            let conn = state.conns.get_mut(&id)?;
            conn.player = Some(player.to_string());
        }
        let ids = state.by_player.entry(player.to_string()).or_default();
        ids.push_back(id);
        if ids.len() <= self.limits.max_per_player {
            return None;
        }
        let evicted = ids.pop_front()?;
        state.conns.get(&evicted).map(|conn| conn.queue.clone())
    }

    /// Signals every live connection to close, waits (bounded) for their tasks to write
    /// it, then aborts whatever is left.
    ///
    /// The wait is on the registry draining rather than on task handles: a connection
    /// removes itself through its [`Slot`] guard on EVERY exit path, so an empty registry
    /// is the honest "everyone is gone" signal, and a task that ignores its queue is
    /// covered by the abort.
    ///
    /// The abort handles are collected AFTER the grace, not before it. A connection
    /// accepted just before this runs attaches its handle from its own task, which may not
    /// have been scheduled yet when the flag is set — snapshotting the handles up front
    /// would leave exactly that connection in nobody's list, and a peer that stopped
    /// reading would then keep the task (and the socket) alive past module stop.
    pub(crate) async fn shutdown(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            for conn in state.conns.values() {
                conn.queue.close(CloseCode::Shutdown);
            }
        }
        let deadline = tokio::time::Instant::now() + PUSH_STOP_GRACE;
        loop {
            // `enable()` performs the registration a bare `notified()` only does on its
            // FIRST POLL: without it, a last `Slot::drop` landing between the check below
            // and the await would notify nobody and this would burn the whole grace.
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            if self.live() == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline, drained).await.is_err() {
                break;
            }
        }
        let aborts: Vec<AbortHandle> = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.conns.values().filter_map(|c| c.abort.clone()).collect()
        };
        for abort in aborts {
            abort.abort();
        }
    }

    fn live(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).conns.len()
    }
}

/// One connection's slot in the registry: the ONE release path for the aggregate
/// counters and every index entry. Held by the connection task (and by the upgrade
/// callback until it runs), so a task that panics, is aborted, or never upgrades at all
/// still frees what it took.
struct Slot {
    hub: Arc<PushHub>,
    id: ConnId,
    ip: IpAddr,
    queue: Arc<ConnQueue>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut state = self.hub.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(count) = state.per_ip.get_mut(&self.ip) {
                *count -= 1;
                if *count == 0 {
                    state.per_ip.remove(&self.ip);
                }
            }
            if let Some(conn) = state.conns.remove(&self.id) {
                if let Some(player) = conn.player {
                    if let Some(ids) = state.by_player.get_mut(&player) {
                        ids.retain(|id| *id != self.id);
                        if ids.is_empty() {
                            state.by_player.remove(&player);
                        }
                    }
                }
                for name in conn.groups {
                    if let Some(members) = state.groups.get_mut(&name) {
                        members.remove(&self.id);
                        if members.is_empty() {
                            state.groups.remove(&name);
                        }
                    }
                }
            }
        }
        // Unconditional: `shutdown` waits on this, so an exit path that skipped it would
        // make the stop grace elapse in full.
        self.hub.drained.notify_waiters();
    }
}

// ---------------------------------------------------------------------------
// The route
// ---------------------------------------------------------------------------

/// `GET /push`: resolves the client address, takes a connection slot, and upgrades.
///
/// The caps are enforced HERE, before the upgrade, so an unauthenticated socket cannot
/// occupy the process for free; the credential check runs after it, in the connection
/// task, because a refusal must reach the client as a typed close.
pub(crate) async fn upgrade(
    front: Arc<FrontDoor>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let hub = front.push_hub();
    let limits = hub.limits().clone();
    let ip = client_ip(&headers, peer.map(|c| c.0), &limits.trusted_proxies);
    let (slot, wake) = match hub.accept(ip) {
        Ok(accepted) => accepted,
        Err(AcceptError::Closing) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "push is shutting down").into_response()
        }
        Err(AcceptError::Full) | Err(AcceptError::IpFull) => {
            return (StatusCode::SERVICE_UNAVAILABLE, CloseCode::Capacity.reason())
                .into_response()
        }
    };

    let bearer = crate::bearer(&headers);
    let api_key = crate::api_key_header(&headers);
    ws.max_message_size(limits.max_frame_bytes)
        .max_frame_size(limits.max_frame_bytes)
        .on_upgrade(move |socket| async move {
            // `on_upgrade` spawns a DETACHED task and drops its `JoinHandle`, so the task
            // below — not this one — is the connection, and its abort handle is what
            // `shutdown` can act on. The handle is delivered through a oneshot the task
            // awaits FIRST, so it is attached before the connection reads a frame; a
            // shutdown that races the spawn still sees it, because `shutdown` collects the
            // handles after its grace rather than up front.
            let (abort_tx, abort_rx) = oneshot::channel();
            let task = tokio::spawn(async move {
                let Ok(abort) = abort_rx.await else { return };
                slot.hub.attach_abort(slot.id, abort);
                run(front, slot, wake, socket, bearer, api_key).await;
            });
            let _ = abort_tx.send(task.abort_handle());
            let _ = task.await;
        })
}

/// Resolves the connection's client address for the per-IP cap: the direct peer, or the
/// forwarded address ONLY when that peer is a trusted proxy (`httpmw::client_ip`, the
/// same walk the app-level rate limiter runs).
///
/// A missing `ConnectInfo` (a test harness calling the router directly) collapses every
/// such connection into one bucket rather than exempting them from the cap.
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trusted: &[IpNet]) -> IpAddr {
    let remote = peer.map(|p| p.ip()).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    httpmw::client_ip(
        remote,
        headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()),
        headers.get("x-real-ip").and_then(|v| v.to_str().ok()),
        trusted,
    )
}

// ---------------------------------------------------------------------------
// The connection task
// ---------------------------------------------------------------------------

async fn run(
    front: Arc<FrontDoor>,
    slot: Slot,
    mut wake: mpsc::Receiver<()>,
    mut socket: WebSocket,
    header_bearer: Option<String>,
    header_api_key: Option<String>,
) {
    let limits = slot.hub.limits().clone();

    let creds = match handshake(
        &mut socket,
        &slot,
        &mut wake,
        header_bearer,
        header_api_key,
        limits.handshake_grace,
    )
    .await
    {
        Ok(creds) => creds,
        Err(Some(code)) => return write_close(&mut socket, code, limits.write_deadline).await,
        Err(None) => return,
    };

    let identity = match front
        .admit(
            creds.api_key.as_deref(),
            creds.bearer.as_deref(),
            opsapi::AuthReq::Player,
            // Presence and validity only: a fixed route has no wire method to match a
            // policy against.
            KeyCheck::PresenceOnly,
        )
        .await
    {
        Ok(identity) => identity,
        Err(denial) => {
            return write_close(&mut socket, close_for(&denial), limits.write_deadline).await
        }
    };
    // Admitted against `AuthReq::Player`, which denies a missing bearer and resolves an
    // identity only from a verified session, so both are `Some` here.
    let (Some(player), Some(bearer)) = (identity.player_id().map(str::to_string), creds.bearer)
    else {
        return write_close(&mut socket, CloseCode::Unauthorized, limits.write_deadline).await;
    };

    if let Some(evicted) = slot.hub.bind(slot.id, &player) {
        evicted.close(CloseCode::Replaced);
    }
    // Queued, not written directly: every server->client frame leaves through the one
    // bounded queue, so the ack cannot jump a backlog or bypass the write deadline.
    slot.queue.push(
        serde_json::to_string(&ServerFrame::Ack {
            connection_id: slot.id,
        })
        .expect("ack serialization cannot fail"),
    );

    serve(front, &slot, &mut wake, &mut socket, &player, &bearer, &limits).await;
}

struct Credentials {
    bearer: Option<String>,
    api_key: Option<String>,
}

/// Collects the credentials: the headers when a client could set them, otherwise the
/// first frame, within `grace`.
///
/// `Err(None)` means the peer is already gone and there is nothing to write back.
async fn handshake(
    socket: &mut WebSocket,
    slot: &Slot,
    wake: &mut mpsc::Receiver<()>,
    header_bearer: Option<String>,
    header_api_key: Option<String>,
    grace: Duration,
) -> Result<Credentials, Option<CloseCode>> {
    if header_bearer.is_some() && header_api_key.is_some() {
        return Ok(Credentials {
            bearer: header_bearer,
            api_key: header_api_key,
        });
    }
    let wait = async {
        loop {
            tokio::select! {
                // A shutdown reaches an unauthenticated socket the same way it reaches a
                // live one — through its queue — so a handshake in flight cannot outlive
                // the grace of a stopping process.
                out = slot.queue.recv(wake) => match out {
                    Some(Outbound::Close(code)) => return Err(Some(code)),
                    // Nothing addresses a connection before it binds a player, so a frame
                    // here would mean the hub resolved a target against an unauthenticated
                    // socket — a routing defect, not a frame to swallow.
                    Some(Outbound::Text(_)) => {
                        unreachable!("nothing is queued to a connection before it binds")
                    }
                    // The queue's sender lives in the registry entry this connection still
                    // holds, so this is unreachable; ending is the safe answer either way.
                    None => return Err(None),
                },
                msg = socket.recv() => match msg {
                    None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => return Err(None),
                    Some(Ok(WsMessage::Text(text))) => return parse_hello(text.as_bytes()),
                    Some(Ok(WsMessage::Binary(bytes))) => return parse_hello(&bytes),
                    Some(Ok(_)) => continue,
                },
            }
        }
    };
    let hello = match tokio::time::timeout(grace, wait).await {
        Ok(result) => result?,
        Err(_elapsed) => return Err(Some(CloseCode::HandshakeTimeout)),
    };
    Ok(Credentials {
        // A header, when the client could set one, outranks the frame: it is the value
        // the HTTP layer already saw.
        bearer: header_bearer.or(hello.bearer),
        api_key: header_api_key.or(hello.api_key),
    })
}

fn parse_hello(bytes: &[u8]) -> Result<Credentials, Option<CloseCode>> {
    match serde_json::from_slice::<ClientFrame>(bytes) {
        Ok(ClientFrame::Hello { token, api_key }) => Ok(Credentials {
            bearer: token,
            api_key,
        }),
        Ok(ClientFrame::Other) | Err(_) => Err(Some(CloseCode::Protocol)),
    }
}

/// The live connection: outbound frames, inbound frames, and the re-verification tick.
async fn serve(
    front: Arc<FrontDoor>,
    slot: &Slot,
    wake: &mut mpsc::Receiver<()>,
    socket: &mut WebSocket,
    player: &str,
    bearer: &str,
    limits: &PushLimits,
) {
    let mut reverify = tokio::time::interval(limits.reverify_interval);
    reverify.tick().await; // the first tick fires immediately; the bearer was just verified
    let mut last_ok = Instant::now();
    let mut close: Option<CloseCode> = None;

    loop {
        // Deliberately UNBIASED. A close preempts inside the queue branch already (the
        // queue answers it ahead of any pending frame), while a biased order would let a
        // producer that keeps the queue non-empty starve inbound reads forever.
        tokio::select! {
            out = slot.queue.recv(wake) => match out {
                Some(Outbound::Close(code)) => {
                    close = Some(code);
                    break;
                }
                Some(Outbound::Text(text)) => {
                    if write(socket, WsMessage::Text(text), limits.write_deadline).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => break,
                // Step 4 serves no client verb; the hub's join/leave land here. An
                // unknown frame is ignored rather than fatal so a newer client stays up.
                Some(Ok(_)) => {}
            },
            _ = reverify.tick() => {
                if write(socket, WsMessage::Ping(Vec::new()), limits.write_deadline).await.is_err() {
                    break;
                }
                match front.reverify_push(bearer).await {
                    Ok(identity) if identity.player_id() == Some(player) => {
                        last_ok = Instant::now();
                    }
                    // A token that now resolves to a DIFFERENT player is not this
                    // connection's credential any more.
                    Ok(_) => {
                        close = Some(CloseCode::Unauthorized);
                        break;
                    }
                    Err(denial) => match close_for(&denial) {
                        // No verdict was reached: keep the connection, retry on the next
                        // tick, and only give up once it has been stale too long.
                        CloseCode::Unavailable => {
                            if last_ok.elapsed() > limits.max_stale {
                                close = Some(CloseCode::Unavailable);
                                break;
                            }
                        }
                        _ => {
                            close = Some(CloseCode::SessionExpired);
                            break;
                        }
                    },
                }
            }
        }
    }
    if let Some(code) = close {
        write_close(socket, code, limits.write_deadline).await;
    }
}

/// Writes the typed close: the JSON frame a client dispatches on, then the protocol-level
/// close a browser sees as a `CloseEvent`. Both writes are bounded and their failures
/// ignored — the connection is ending either way.
async fn write_close(socket: &mut WebSocket, code: CloseCode, deadline: Duration) {
    let _ = write(socket, WsMessage::Text(code.frame()), deadline).await;
    let _ = write(
        socket,
        WsMessage::Close(Some(CloseFrame {
            code: code.ws_code(),
            reason: code.reason().into(),
        })),
        deadline,
    )
    .await;
}

/// One bounded write. A peer that stops reading fills the kernel buffer and would
/// otherwise pin this task for as long as it likes.
async fn write(socket: &mut WebSocket, msg: WsMessage, deadline: Duration) -> Result<(), ()> {
    match tokio::time::timeout(deadline, socket.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(()),
    }
}

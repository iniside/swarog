//! `push` — the server→client delivery model: what a message IS, who it is addressed
//! to, and the one seam a producer calls. A foundation leaf (`contrib` + serde,
//! `thiserror` and `tracing`); it owns no socket, no transport and no schedule.
//!
//! **The shape, taken from SignalR's hub/lifetime-manager split.** A producer never
//! learns where a player's connections live: it addresses a [`Target`] and hands a
//! [`Message`] to the process's [`Push`] handle. The installed [`Sink`] either answers
//! locally (this process owns the sockets — [`Delivered::Local`] with the number of
//! connections written to) or hands the message to a backplane queue for fan-out to the
//! processes that do ([`Delivered::Queued`]). The two answers are deliberately distinct
//! types of statement: a count is an observation, a queue handoff is a promise to try.
//!
//! **Delivery is best-effort, not durable.** There is no checkpoint, no redelivery and
//! no id: a socket that is gone was never going to receive the frame, and the
//! authoritative copy of anything that matters lives in the producing module's own
//! tables (the durable event plane is what carries state between modules). A push
//! message therefore says "something changed, refetch", never "here is the change".
//!
//! **The payload is opaque bytes here.** This crate never parses, validates or
//! re-encodes it; only the producer and the client agree on its shape.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// The contrib slot a transport contributes its [`Sink`] to during module `init`. The
/// composition layer (`core/app`, after the module build — contributions do not exist
/// before then) MUST drain this slot and install AT MOST ONE contribution via
/// [`Push::install`]; zero is legal and leaves the sinkless handle below.
pub const SINK_SLOT: contrib::Slot<Arc<dyn Sink>> = contrib::Slot::new("push.sink");

/// The server-minted identity of one client connection.
///
/// Process-local and short-lived by design: it is minted by [`ConnId::mint`] when a
/// connection is accepted, is never reused within that process, and is meaningless in
/// any other process or after a restart. A reconnect is a NEW connection and gets a new
/// id — the client must not treat it as a stable handle, and nothing addressable
/// ([`Target`]) is keyed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnId(u64);

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

impl ConnId {
    /// Mints the next id for this process. The one minting authority, so uniqueness
    /// within the process is structural rather than a convention each accept site
    /// re-implements.
    ///
    /// A sequential counter is sound ONLY while an id confers nothing: no [`Target`]
    /// variant selects a connection, and no client-supplied id is ever honoured. Adding
    /// either (a `Conn` target, a resume or kick verb naming an id) makes this a
    /// guessable authority over someone else's socket, and the counter must become a
    /// CSPRNG value in that same change.
    pub fn mint() -> ConnId {
        ConnId(NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Who a [`Message`] is addressed to. Resolution is the hub's job, per process, over
/// the connections that process owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Every connection authenticated as this player — a fan-out over their devices,
    /// not a single socket.
    Player(String),
    /// Every connection that joined this group. Groups are ephemeral, host-owned and
    /// die with the connection; membership is not authorization.
    Group(String),
    /// Every connection on the front that has completed its handshake. A connection with
    /// no identity yet is not addressable — it is not a participant, and it must not be
    /// told about the ones that are.
    All,
}

/// One server→client message: a `topic` the client dispatches on and an OPAQUE
/// `payload` this crate never interprets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn new(topic: impl Into<String>, payload: impl Into<Vec<u8>>) -> Message {
        Message {
            topic: topic.into(),
            payload: payload.into(),
        }
    }
}

/// One addressed message on the backplane wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub target: Target,
    pub msg: Message,
}

impl Envelope {
    pub fn new(target: Target, msg: Message) -> Envelope {
        Envelope { target, msg }
    }
}

/// Encodes an ORDERED batch of addressed messages for the backplane.
///
/// Order is the contract, not an implementation detail: the backplane sends a whole
/// batch in ONE call precisely so frames cannot invert between the split and the
/// monolith, and both this encoding and [`decode_batch`] preserve the slice order.
///
/// **The output is already-encoded JSON.** It belongs in `edge::Client::call_raw`'s
/// `payload`, which relays those bytes verbatim into the request envelope. Handing it
/// to generated RPC glue as a typed `Vec<u8>` argument instead would JSON-encode it a
/// SECOND time — each byte becoming a decimal number in an array, on top of the
/// numeric array this encoding already produces for [`Message::payload`].
///
/// **Nothing here bounds a message or a batch, and the sender must.** The internal
/// edge rejects an oversized frame whole, so one too-large batch discards every
/// unrelated small message travelling with it — the opposite of what
/// one-ordered-batch-per-call is for. A batching sink caps its own batch against the
/// transport's frame limit and splits into several ordered calls.
pub fn encode_batch(batch: &[Envelope]) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(batch).map_err(|e| Error::Codec(e.to_string()))
}

/// Decodes a batch produced by [`encode_batch`], preserving its order.
pub fn decode_batch(bytes: &[u8]) -> Result<Vec<Envelope>, Error> {
    serde_json::from_slice(bytes).map_err(|e| Error::Codec(e.to_string()))
}

/// What a [`Sink`] did with a message. The two variants are different KINDS of answer
/// and must never be collapsed into one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// The sink resolved the target against connections this process owns and wrote to
    /// `0..n` of them. `0` is a normal answer (nobody addressed is connected here), not
    /// a failure.
    Local(usize),
    /// The sink accepted the message for fan-out and has not written it to any
    /// connection yet. Nothing is promised about how many connections will receive it,
    /// or that any will.
    Queued,
}

/// The one seam between a producer and a transport.
///
/// **Synchronous on purpose.** A caller may hold a database transaction's connection —
/// the durable-event handler that nudges an inbox is the motivating case, and a handler
/// that exceeds `ASYNCEVENTS_HANDLER_TIMEOUT` pauses its whole subscription. A sink
/// therefore cannot await anything: it resolves against an in-memory registry or
/// non-blockingly offers to its own queue and returns [`Delivered::Queued`]. A full
/// queue is [`Error::Backlogged`] — dropped, never blocked on, and the sink owning that
/// queue is the one that must count the drops.
pub trait Sink: Send + Sync + 'static {
    fn send(&self, target: &Target, msg: &Message) -> Result<Delivered, Error>;
}

/// The process-wide push handle. Always present; the [`Sink`] behind it is optional and
/// installed at most once, by the composition layer.
#[derive(Default)]
pub struct Push {
    sink: OnceLock<Arc<dyn Sink>>,
    no_sink_logged: AtomicBool,
}

impl Push {
    pub fn new() -> Push {
        Push::default()
    }

    /// Installs this process's sink. PANICS on a second call: two sinks mean two
    /// answers to "where do this player's connections live", and `OnceLock::set`
    /// silently keeping the first would make the losing wiring invisible — the same
    /// loud-boot-failure convention as a duplicate `registry::provide`.
    pub fn install(&self, sink: Arc<dyn Sink>) {
        if self.sink.set(sink).is_err() {
            panic!("push sink already installed in this process");
        }
    }

    /// Sends `msg` to `target` through the installed sink.
    ///
    /// With no sink installed this is [`Error::NoSink`] — deliberately not a panic (it
    /// would kill a request or a durable-delivery path over a best-effort concern) and
    /// deliberately not a silent no-op. The mistake is logged at `error!` ONCE per
    /// process and at `debug!` thereafter: a sinkless process pushing once per durable
    /// event would otherwise bury every other line in the log, and the per-call signal
    /// callers act on is the returned error, not the log.
    pub fn send(&self, target: &Target, msg: &Message) -> Result<Delivered, Error> {
        let Some(sink) = self.sink.get() else {
            if self.no_sink_logged.swap(true, Ordering::Relaxed) {
                tracing::debug!(topic = %msg.topic, "push: no sink installed; message dropped");
            } else {
                tracing::error!(
                    topic = %msg.topic,
                    "push: no sink installed in this process; message dropped (further \
                     occurrences log at debug)"
                );
            }
            return Err(Error::NoSink);
        };
        sink.send(target, msg)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no push sink installed in this process")]
    NoSink,
    #[error("push queue full; message dropped")]
    Backlogged,
    #[error("push batch codec: {0}")]
    Codec(String),
    #[error("push transport: {0}")]
    Transport(String),
}

//! The push backplane SENDER: the half of server→client delivery that runs in a process
//! which owns no sockets. A producer there calls `ctx.push()`, the [`PushSender`]'s sink
//! accepts the message into a bounded queue, and a drain task forwards it over the
//! internal edge to every front that does own sockets ([`Pool::deliver_all`]).
//!
//! Two properties decide the whole shape:
//!
//! * **The producer may hold a database transaction's connection.** `push::Sink::send` is
//!   synchronous for that reason, and a durable-event handler that blocks (or returns an
//!   error) pauses its whole subscription for every player. So the sink does a
//!   NON-BLOCKING offer and returns immediately; a full queue is a counted drop, never
//!   backpressure and never a failure the producer must handle.
//! * **Ordering must not depend on topology.** The drain sends one ORDERED batch per call
//!   rather than a call per message: N concurrent per-message calls could arrive inverted
//!   in the split while the monolith's local sink never inverts them, which would make
//!   frame order a deployment detail.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use push::{Delivered, Envelope, Message, Target};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::{
    pool_refresh_loop, FanoutState, PeerListResolver, PeerSource, Pool, PROBE_STOP_GRACE,
};

/// The wire method the fronts serve for a backplane batch (`modules/gateway` registers the
/// receiving half under this same name).
const DELIVER_METHOD: &str = "push.deliver";

/// How many messages the sink holds before it starts dropping. Depth is a memory bound on
/// a producer that outruns the fronts, not a delivery guarantee: the durable copy of
/// anything that matters lives in the producing module's tables, and the message itself
/// only says "refetch".
const QUEUE_CAPACITY: usize = 1024;

/// Upper bound on the messages carried by ONE `push.deliver` call. Caps the tail latency a
/// long backlog can add to the newest message (the drain must finish a batch before it
/// picks up the next) independently of how large the byte budget would allow it to grow.
const BATCH_MAX_MESSAGES: usize = 128;

/// Byte budget for one encoded batch. The internal edge rejects an oversized frame WHOLE,
/// so a batch that overshoots would discard every unrelated small message travelling with
/// it — the opposite of what one-call-per-batch is for. The slack under `edge::MAX_FRAME`
/// covers the request envelope the batch is embedded in (method name, identity field, JSON
/// keys), which is tens of bytes.
const BATCH_BYTE_BUDGET: usize = edge::MAX_FRAME - 1024;

/// How long the drain waits for the pool's FIRST resolve before giving up on a batch, and
/// how often it re-checks. Bounded so a permanently unresolvable peer cannot pin the drain
/// (and with it the queue) forever.
const RESOLVE_WAIT: Duration = Duration::from_secs(5);
const RESOLVE_POLL: Duration = Duration::from_millis(50);

/// Counts and logs dropped messages. The first drop is a `warn!`, every later one a
/// `debug!` carrying the running total: a front outage would otherwise emit one warning per
/// produced message and bury every other line, while a silent drop would make the outage
/// invisible.
struct Drops {
    total: AtomicU64,
    warned: AtomicBool,
}

impl Drops {
    fn new() -> Drops {
        Drops {
            total: AtomicU64::new(0),
            warned: AtomicBool::new(false),
        }
    }

    fn record(&self, count: u64, reason: &str) {
        let total = self.total.fetch_add(count, Ordering::Relaxed) + count;
        if self.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!(
                dropped = count,
                dropped_total = total,
                reason,
                "push backplane dropped messages"
            );
        } else {
            tracing::warn!(
                dropped = count,
                dropped_total = total,
                reason,
                "push backplane dropped messages (further occurrences log at debug)"
            );
        }
    }
}

/// The [`push::Sink`] the producer's `ctx.push()` calls: a non-blocking offer into the
/// drain's queue.
struct BatchingSink {
    tx: mpsc::Sender<Envelope>,
    drops: Arc<Drops>,
}

impl push::Sink for BatchingSink {
    /// Offers the message to the queue and answers [`Delivered::Queued`] — a promise to
    /// try, never a count, because this process owns no connections and cannot know how
    /// many will receive it.
    ///
    /// `try_send` is the only send shape allowed here: the caller may be a durable-event
    /// handler running on the delivery transaction's connection, so this must neither
    /// await nor block. A full queue answers [`push::Error::Backlogged`] after counting
    /// the drop; the producer is expected to ignore it (the message is best-effort), and
    /// the operator sees it in the drop log.
    fn send(&self, target: &Target, msg: &Message) -> Result<Delivered, push::Error> {
        match self.tx.try_send(Envelope::new(target.clone(), msg.clone())) {
            Ok(()) => Ok(Delivered::Queued),
            Err(mpsc::error::TrySendError::Full(env)) => {
                self.drops.record(1, "queue-full");
                tracing::debug!(topic = %env.msg.topic, "push backplane queue full");
                Err(push::Error::Backlogged)
            }
            Err(mpsc::error::TrySendError::Closed(env)) => {
                self.drops.record(1, "drain-stopped");
                tracing::debug!(topic = %env.msg.topic, "push backplane drain is gone");
                Err(push::Error::Transport("push backplane drain stopped".to_string()))
            }
        }
    }
}

/// The backplane sender module: it contributes the sink in `init`, owns the pool + the
/// drain task from `start`, and tears both down in `stop`.
///
/// Deliberately NOT a [`crate::Stub`]: a stub carries provider-swap factories and
/// advertises a `PeerAddr` under [`opsapi::PEER_SLOT`], which would tell a co-hosted
/// gateway route table that this peer provides `#[http]` ops. The push front provides
/// none — it RECEIVES a backplane call — so this module contributes nothing but the sink.
pub struct PushSender {
    pool: Arc<Pool>,
    sink: Arc<BatchingSink>,
    /// The receiving half, moved into the drain task at `start`. `None` afterwards, so a
    /// second `start` cannot spawn a second drain competing for the same queue.
    rx: StdMutex<Option<mpsc::Receiver<Envelope>>>,
    drops: Arc<Drops>,
    stop: StdMutex<Option<watch::Sender<bool>>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl PushSender {
    /// Builds a sender that fans out to `peer` — the push front(s). A
    /// [`PeerSource::pooled`] source fans out over every resolved instance; a fixed or
    /// single-resolving source degenerates to a pool of one, so the split's
    /// one-gateway wiring and a multi-front fleet are the same code.
    pub fn new(peer: impl Into<PeerSource>) -> PushSender {
        PushSender::with_capacity(peer, QUEUE_CAPACITY)
    }

    /// [`PushSender::new`] with an explicit queue depth.
    pub fn with_capacity(peer: impl Into<PeerSource>, capacity: usize) -> PushSender {
        let list = match peer.into() {
            PeerSource::Pooled { list, .. } => list,
            // A single-address source still fans out — over one instance. Wrapping its
            // resolver keeps the live re-resolve the source promised.
            PeerSource::Single { resolver, .. } => {
                let list: PeerListResolver = Arc::new(move || {
                    let resolver = resolver.clone();
                    let fut: BoxFuture<'static, Result<Vec<String>, String>> =
                        Box::pin(async move { resolver().await.map(|a| vec![a]) });
                    fut
                });
                list
            }
        };
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let drops = Arc::new(Drops::new());
        PushSender {
            pool: Arc::new(Pool::new(list)),
            sink: Arc::new(BatchingSink {
                tx,
                drops: drops.clone(),
            }),
            rx: StdMutex::new(Some(rx)),
            drops,
            stop: StdMutex::new(None),
            tasks: StdMutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Module for PushSender {
    fn name(&self) -> &str {
        "push-backplane"
    }

    fn requires(&self) -> Vec<String> {
        Vec::new()
    }

    /// Contributes the sink. `core/app` installs at most one after the module build, so a
    /// process that also hosts the sockets locally is a loud startup failure there rather
    /// than a silent second answer to "where do this player's connections live".
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let sink: Arc<dyn push::Sink> = self.sink.clone();
        ctx.contribute(push::SINK_SLOT, sink);
        Ok(())
    }

    /// Arms the pool (its background refresh loop brings the instance set + per-instance
    /// probes up independently of traffic) and the drain task.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let Some(rx) = self.rx.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Ok(());
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let refresh = tokio::spawn(pool_refresh_loop(self.pool.clone(), stop_rx.clone()));
        let drain = tokio::spawn(drain_loop(
            self.pool.clone(),
            rx,
            stop_rx,
            self.drops.clone(),
        ));
        *self.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        *self.tasks.lock().unwrap_or_else(|e| e.into_inner()) = vec![refresh, drain];
        Ok(())
    }

    /// Signals and joins both loops (grace, then abort) BEFORE stopping the pool: the pool
    /// tears down every instance's probe task and connection, and a drain still running
    /// past that would re-dial one through its next `deliver_all` — leaving the process
    /// with a probing edge connection nothing owns.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(true);
        }
        // Take the handles into a local so the std guard is dropped before the awaits.
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for mut task in tasks {
            if tokio::time::timeout(PROBE_STOP_GRACE, &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
        self.pool.stop().await;
        Ok(())
    }
}

/// Pops a batch, waits for the pool to have an answer, and forwards the batch as ordered
/// `push.deliver` calls until the stop signal fires.
async fn drain_loop(
    pool: Arc<Pool>,
    mut rx: mpsc::Receiver<Envelope>,
    mut stop: watch::Receiver<bool>,
    drops: Arc<Drops>,
) {
    loop {
        let first = tokio::select! {
            biased;
            _ = stop.changed() => return,
            msg = rx.recv() => match msg {
                Some(msg) => msg,
                None => return,
            },
        };
        let mut batch = vec![first];
        // Everything already queued rides along, up to the batch cap — the whole point of
        // one call per pass. `try_recv` never waits, so a lone message is not delayed
        // waiting for company.
        while batch.len() < BATCH_MAX_MESSAGES {
            match rx.try_recv() {
                Ok(msg) => batch.push(msg),
                Err(_) => break,
            }
        }
        if !wait_for_resolve(&pool, &mut stop).await {
            drops.record(batch.len() as u64, "stopping");
            return;
        }
        send_batch(&pool, batch, &drops).await;
    }
}

/// Blocks the drain while the pool has NEVER resolved, up to [`RESOLVE_WAIT`]; returns
/// `false` only when the stop signal fired.
///
/// This is the startup window: `start` spawns the refresh loop and the drain together, so
/// the first messages routinely arrive before the first resolve lands. Treating that empty
/// instance set as "nothing alive" would drop them, and a producer has no way to tell that
/// from a real outage. A pool that HAS resolved is not waited on — a dead front is a
/// dropped message by design, not something to hold a queue for.
async fn wait_for_resolve(pool: &Pool, stop: &mut watch::Receiver<bool>) -> bool {
    let started = std::time::Instant::now();
    while pool.fanout_state() == FanoutState::Unresolved {
        if started.elapsed() >= RESOLVE_WAIT {
            tracing::debug!(
                waited = ?RESOLVE_WAIT,
                "push backplane: peer list still unresolved; delivering anyway"
            );
            return true;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return false,
            _ = tokio::time::sleep(RESOLVE_POLL) => {}
        }
    }
    true
}

/// Encodes `batch` into as few ordered calls as the byte budget allows and issues them
/// SEQUENTIALLY, so a batch that had to be split still arrives in produced order.
async fn send_batch(pool: &Pool, batch: Vec<Envelope>, drops: &Drops) {
    if pool.fanout_state() == FanoutState::NoTarget {
        drops.record(batch.len() as u64, "no-live-front");
        return;
    }
    for chunk in split_batch(batch, drops) {
        let count = chunk.len() as u64;
        let bytes = match push::encode_batch(&chunk) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(error = %e, "push backplane: batch encode failed");
                drops.record(count, "encode-failed");
                continue;
            }
        };
        if pool.deliver_all(DELIVER_METHOD, &bytes).await == 0 {
            drops.record(count, "no-front-accepted");
        }
    }
}

/// Splits `batch` into chunks whose encoded form fits [`BATCH_BYTE_BUDGET`], preserving
/// order. A single message that cannot fit alone is dropped and counted — carrying it
/// forward would make every later batch fail the same way.
fn split_batch(batch: Vec<Envelope>, drops: &Drops) -> Vec<Vec<Envelope>> {
    let mut chunks: Vec<Vec<Envelope>> = Vec::new();
    let mut chunk: Vec<Envelope> = Vec::new();
    // Both element sizes and the running total are measured through `push::encode_batch`
    // itself, so the budget cannot drift from the codec: a JSON array of one element is
    // the element plus its two brackets, and each further element costs its own size plus
    // one separator.
    let mut chunk_bytes = BRACKETS;
    for env in batch {
        let size = match push::encode_batch(std::slice::from_ref(&env)) {
            Ok(bytes) => bytes.len().saturating_sub(BRACKETS),
            Err(e) => {
                tracing::debug!(
                    topic = %env.msg.topic,
                    error = %e,
                    "push backplane: message encode failed"
                );
                drops.record(1, "encode-failed");
                continue;
            }
        };
        if BRACKETS + size > BATCH_BYTE_BUDGET {
            tracing::debug!(
                topic = %env.msg.topic,
                size,
                budget = BATCH_BYTE_BUDGET,
                "push backplane: message exceeds the transport frame budget"
            );
            drops.record(1, "oversized-message");
            continue;
        }
        if chunk_bytes + separator_len(&chunk) + size > BATCH_BYTE_BUDGET {
            chunks.push(std::mem::take(&mut chunk));
            chunk_bytes = BRACKETS;
        }
        chunk_bytes += separator_len(&chunk) + size;
        chunk.push(env);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// The `[` and `]` of an encoded batch.
const BRACKETS: usize = 2;

/// The `,` an element costs when it is not the first in its chunk.
fn separator_len(chunk: &[Envelope]) -> usize {
    if chunk.is_empty() {
        0
    } else {
        1
    }
}

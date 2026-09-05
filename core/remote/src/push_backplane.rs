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

use crate::{pool_refresh_loop, FanoutState, PeerListResolver, PeerSource, Pool};

/// The wire method one batch travels as. Nothing links this constant to the front's
/// registration, so the two names are a hand-maintained contract: a mismatch answers
/// `edge::Error::UnknownMethod` on every batch, which [`Pool::deliver_all`] logs at
/// `debug!` and counts as a rejection — a total outage that looks exactly like every front
/// being down, and never fails a boot.
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

/// How long the drain waits for the pool's FIRST resolve, ONCE per process (see
/// [`wait_for_resolve`]), and how often it re-checks while waiting.
const RESOLVE_WAIT: Duration = Duration::from_secs(5);
const RESOLVE_POLL: Duration = Duration::from_millis(50);

/// Shared deadline for joining BOTH background loops in `stop`. The drain observes the stop
/// signal between chunks and during a delivery, so it exits within it; the refresh loop can
/// be inside a resolve of unknown length, which is what the abort after this deadline is
/// for. Sized against `core/app`'s `MODULE_STOP_GRACE_MS` (default 5000ms), which bounds
/// this whole `stop`: 1s here, then a concurrent [`Pool::stop`] costing about one
/// `PROBE_STOP_GRACE` (2s) whatever the instance count, leaves headroom. Being truncated
/// there is what strands probe tasks and edge connections past teardown.
const STOP_JOIN_GRACE: Duration = Duration::from_secs(1);

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

    /// [`PushSender::new`] with an explicit queue depth — the in-crate seam for reaching
    /// the full-queue branch without producing [`QUEUE_CAPACITY`] messages.
    pub(crate) fn with_capacity(peer: impl Into<PeerSource>, capacity: usize) -> PushSender {
        let list = match peer.into() {
            PeerSource::Pooled { list, .. } => list,
            // A single-address source becomes a pool of one. Two differences from
            // `Backing::Single` matter and both are accounted for: the source's live
            // re-resolve is preserved by wrapping its resolver, and the pool's per-instance
            // health — which `Backing::Single` has no equivalent of — never gates a
            // delivery, because `Pool::deliver_all` attempts every resolved instance. So a
            // single-address sender attempts its one front exactly as unconditionally as a
            // `Reconnecting` caller would.
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

    /// Signals and joins both loops BEFORE stopping the pool: the pool tears down every
    /// instance's probe task and connection, and a drain still running past that would
    /// re-dial one through its next `deliver_all` — leaving the process with a probing edge
    /// connection nothing owns.
    ///
    /// The two joins share ONE [`STOP_JOIN_GRACE`] and run concurrently. Sequential graces
    /// plus a per-instance pool teardown do not fit inside `MODULE_STOP_GRACE_MS`, and
    /// being truncated there is what strands tasks and connections.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(true);
        }
        // Take the handles into a local so the std guard is dropped before the awaits.
        let mut tasks =
            std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        let joined = {
            let joins = futures::future::join_all(tasks.iter_mut());
            tokio::time::timeout(STOP_JOIN_GRACE, joins).await.is_ok()
        };
        if !joined {
            for task in &mut tasks {
                task.abort();
                let _ = task.await;
            }
        }
        self.pool.stop().await;
        Ok(())
    }
}

/// Closes the queue and counts everything still in it, plus the `held` messages the caller
/// was carrying — the drain's exit accounting.
///
/// Without this a shutdown discards up to a full queue with no record, and a drop total
/// that is wrong by an order of magnitude is worse than no total: it is the number an
/// operator uses to decide whether a push outage happened. Closing first also converts
/// every later producer call into the sink's own counted `drain-stopped` drop, instead of
/// leaving messages to disappear into a receiver nobody polls.
fn record_stop_loss(rx: &mut mpsc::Receiver<Envelope>, drops: &Drops, held: u64) {
    rx.close();
    let mut lost = held;
    while rx.try_recv().is_ok() {
        lost += 1;
    }
    if lost > 0 {
        drops.record(lost, "stopping");
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
    // The startup wait happens at most ONCE per drain, whatever its outcome.
    let mut resolve_waited = false;
    loop {
        let first = tokio::select! {
            biased;
            _ = stop.changed() => {
                record_stop_loss(&mut rx, &drops, 0);
                return;
            }
            msg = rx.recv() => match msg {
                Some(msg) => msg,
                // Every sender is gone; there is nothing left to account for.
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
        if !resolve_waited {
            resolve_waited = true;
            if !wait_for_resolve(&pool, &mut stop).await {
                record_stop_loss(&mut rx, &drops, batch.len() as u64);
                return;
            }
        }
        // `send_batch` has already accounted for the batch it was holding; what remains
        // unaccounted is the queue behind it.
        if !send_batch(&pool, batch, &drops, &mut stop).await {
            record_stop_loss(&mut rx, &drops, 0);
            return;
        }
    }
}

/// Blocks the drain while the pool has NEVER resolved, up to [`RESOLVE_WAIT`]; returns
/// `false` only when the stop signal fired.
///
/// This covers ONE window: `start` spawns the refresh loop and the drain together, so the
/// first messages routinely arrive before the first resolve lands, and treating that empty
/// instance set as "nothing there" would drop them — a producer cannot tell that from a
/// real outage. The caller runs this at most once, which is what makes it a startup window
/// rather than a per-batch tax: a list resolver that fails permanently never applies a
/// generation, so `fanout_state` stays [`FanoutState::Unresolved`] forever and a per-batch
/// wait would throttle the drain to one batch per [`RESOLVE_WAIT`] for the process
/// lifetime. A pool that HAS resolved is never waited on — a dead front is a dropped
/// message by design.
async fn wait_for_resolve(pool: &Pool, stop: &mut watch::Receiver<bool>) -> bool {
    let started = std::time::Instant::now();
    while pool.fanout_state() == FanoutState::Unresolved {
        if started.elapsed() >= RESOLVE_WAIT {
            tracing::warn!(
                waited = ?RESOLVE_WAIT,
                "push backplane: peer list did not resolve; delivering without waiting from now on"
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
/// SEQUENTIALLY, so a batch that had to be split still arrives in produced order. Returns
/// `false` when the stop signal fired — the caller must not start another batch.
///
/// The stop signal is honoured BETWEEN chunks and DURING a delivery: a fan-out call can
/// take up to [`FANOUT_CALL_TIMEOUT`] against a stalled front, which is longer than
/// `stop`'s join grace, so waiting it out would get the drain aborted instead — and an
/// aborted drain reports none of the messages it was holding.
async fn send_batch(
    pool: &Pool,
    batch: Vec<Envelope>,
    drops: &Drops,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    if pool.fanout_state() == FanoutState::Empty {
        drops.record(batch.len() as u64, "no-resolved-front");
        return true;
    }
    let chunks = split_batch(batch, drops);
    for (i, chunk) in chunks.iter().enumerate() {
        let remaining = || chunks[i..].iter().map(|c| c.len() as u64).sum::<u64>();
        if *stop.borrow_and_update() {
            drops.record(remaining(), "stopping");
            return false;
        }
        let count = chunk.len() as u64;
        let bytes = match push::encode_batch(chunk) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(error = %e, "push backplane: batch encode failed");
                drops.record(count, "encode-failed");
                continue;
            }
        };
        let delivered = tokio::select! {
            biased;
            _ = stop.changed() => {
                drops.record(remaining(), "stopping");
                return false;
            }
            n = pool.deliver_all(DELIVER_METHOD, &bytes) => n,
        };
        if delivered == 0 {
            drops.record(count, "no-front-accepted");
        }
    }
    true
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

// The backplane sender's tests (separate file per the tests-in-separate-files rule;
// same module so they reach the private drain, its byte-boundary split, and the pool's
// fake-instance seam).
#[cfg(test)]
#[path = "push_backplane_tests.rs"]
mod tests;

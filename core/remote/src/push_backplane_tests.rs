use super::*;

use std::sync::atomic::AtomicUsize;

use crate::{Caller, Error, Instance, InstanceCloser, InstanceHealth, RetryMode};

// ---------------------------------------------------------------------------
// Fixtures: a fake pool whose instances record the batches they were handed.
//
// No socket and no `edge::Client` anywhere here — a `Pool` built through
// `Pool::with_factory` with fake instances is the only way to observe WHAT the drain
// sent, WHEN, and how many calls it took to send it.
// ---------------------------------------------------------------------------

/// A fake per-instance caller: decodes the batch it was handed and publishes it, so a
/// test reads the drain's real wire payload (encoded by `push::encode_batch`) rather
/// than a proxy. With `block` set it never returns, which is how a test gets the stop
/// signal to fire DURING a delivery.
struct SpyCaller {
    sent: mpsc::UnboundedSender<Vec<Envelope>>,
    block: bool,
}

#[async_trait]
impl Caller for SpyCaller {
    async fn call(
        &self,
        method: &str,
        _identity: Option<&str>,
        payload: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        assert_eq!(method, "push.deliver", "the drain sends one wire method");
        let batch = push::decode_batch(payload).expect("the drain sends an encoded batch");
        let _ = self.sent.send(batch);
        if self.block {
            // Parks with no timer, so a paused or running clock cannot end the call —
            // only the stop signal the caller is racing can.
            std::future::pending::<()>().await;
        }
        Ok(Vec::new())
    }
}

/// One healthy fake instance (a completed `Ok` probe, no probe task, no connection),
/// counting its own teardown so a test can prove `Pool::stop` reached it.
fn fake_instance(addr: &str, caller: Arc<dyn Caller>, closed: Arc<AtomicUsize>) -> Instance {
    let health = Arc::new(InstanceHealth::seed());
    *health.verdict.lock().unwrap() = Ok(());
    health
        .last_probe_at
        .store(crate::coarse_now_secs().max(1), Ordering::SeqCst);
    let close: InstanceCloser = Arc::new(move || {
        closed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    });
    Instance {
        addr: addr.to_string(),
        caller,
        health,
        probe: None,
        close,
    }
}

fn spy_factory(sent: mpsc::UnboundedSender<Vec<Envelope>>, block: bool) -> crate::InstanceFactory {
    Arc::new(move |addr: &str| {
        let caller: Arc<dyn Caller> = Arc::new(SpyCaller {
            sent: sent.clone(),
            block,
        });
        fake_instance(addr, caller, Arc::new(AtomicUsize::new(0)))
    })
}

fn list_of(addrs: &[&str]) -> PeerListResolver {
    let addrs: Vec<String> = addrs.iter().map(|s| s.to_string()).collect();
    Arc::new(move || {
        let addrs = addrs.clone();
        Box::pin(async move { Ok(addrs) })
    })
}

/// A list resolver that never succeeds — `refresh_once` returns before stamping
/// `applied_gen`, so the pool stays [`FanoutState::Unresolved`] for the whole test.
fn never_resolves() -> PeerListResolver {
    Arc::new(|| Box::pin(async { Err("agent unreachable".to_string()) }))
}

fn envelope(tag: &str, pad: &str) -> Envelope {
    Envelope::new(
        Target::Player("p".to_string()),
        Message::new(format!("{tag}{pad}"), Vec::new()),
    )
}

fn topics(batch: &[Envelope]) -> Vec<String> {
    batch.iter().map(|e| e.msg.topic.clone()).collect()
}

/// An envelope whose ENCODED batch-of-one is exactly `total` bytes. The padding goes
/// into the topic, where every added ASCII byte costs exactly one JSON byte, so the
/// byte-boundary tests sit ON the budget rather than near it.
fn env_of_encoded_size(tag: &str, total: usize) -> Envelope {
    let probe = envelope(tag, "");
    let base = push::encode_batch(std::slice::from_ref(&probe)).unwrap().len();
    assert!(total >= base, "cannot build an envelope smaller than {base} bytes");
    let env = envelope(tag, &"x".repeat(total - base));
    assert_eq!(
        push::encode_batch(std::slice::from_ref(&env)).unwrap().len(),
        total,
        "the fixture must land exactly on the requested size"
    );
    env
}

// ---------------------------------------------------------------------------
// `split_batch` at the byte boundary.
// ---------------------------------------------------------------------------

/// A message whose encoded batch-of-one is EXACTLY the budget still ships: the check is
/// `>` the budget, not `>=`. One byte of drift here silently drops the largest legal
/// message on every batch.
#[test]
fn split_batch_keeps_a_message_that_exactly_fills_the_budget() {
    let drops = Drops::new();
    let env = env_of_encoded_size("fits", BATCH_BYTE_BUDGET);

    let chunks = split_batch(vec![env.clone()], &drops);

    assert_eq!(chunks.len(), 1, "an exactly-fitting message is one chunk");
    assert_eq!(chunks[0], vec![env]);
    assert_eq!(drops.total.load(Ordering::SeqCst), 0, "nothing is dropped at the boundary");
}

/// One byte over the budget cannot fit in ANY chunk, so it is dropped and counted here
/// and now — never carried forward, which would make every later chunk fail the same
/// way. Its neighbours travel on, in order.
#[test]
fn split_batch_drops_a_message_that_cannot_fit_alone_and_keeps_the_rest_in_order() {
    let drops = Drops::new();
    let oversized = env_of_encoded_size("huge", BATCH_BYTE_BUDGET + 1);
    let batch = vec![envelope("a", ""), oversized, envelope("b", "")];

    let chunks = split_batch(batch, &drops);

    assert_eq!(chunks.len(), 1);
    assert_eq!(
        topics(&chunks[0]),
        vec!["a".to_string(), "b".to_string()],
        "the undeliverable message is removed; the order around it is untouched"
    );
    assert_eq!(
        drops.total.load(Ordering::SeqCst),
        1,
        "the dropped message is counted, not silently discarded"
    );
}

/// A batch too large for one call becomes SEVERAL ordered chunks, and the produced
/// order survives the split — the reason the drain sends chunks sequentially.
#[test]
fn split_batch_preserves_order_across_chunks() {
    let drops = Drops::new();
    let half = BATCH_BYTE_BUDGET / 2 + 100;
    let first = env_of_encoded_size("one", half);
    let second = env_of_encoded_size("two", half);
    let third = envelope("three", "");

    let chunks = split_batch(vec![first.clone(), second.clone(), third], &drops);

    // The padded topics are megabytes long, so these assert on their tags rather than
    // on equality (a failed `assert_eq!` would print the whole payload).
    assert_eq!(chunks.len(), 2, "two half-budget messages cannot share a chunk");
    assert_eq!(chunks[0].len(), 1);
    assert!(chunks[0][0].msg.topic.starts_with("one"), "the first chunk leads");
    assert_eq!(chunks[1].len(), 2);
    assert!(
        chunks[1][0].msg.topic.starts_with("two"),
        "the second chunk starts where the first ended"
    );
    assert_eq!(chunks[1][1].msg.topic, "three", "and the tail keeps its place");
    assert_eq!(drops.total.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// The drain loop.
// ---------------------------------------------------------------------------

/// N queued messages leave as ONE call, in produced order. A per-message call would
/// show up here as a first batch of length 1 — and in the split as frames arriving
/// inverted, which the monolith's local sink can never do.
#[tokio::test]
async fn drain_sends_every_queued_message_as_one_ordered_batch() {
    let (tx, rx) = mpsc::channel(16);
    let (sent_tx, mut sent_rx) = mpsc::unbounded_channel();
    let pool = Arc::new(Pool::with_factory(list_of(&["A"]), spy_factory(sent_tx, false)));
    // Resolve up front so the drain's one-shot startup wait is not in play.
    pool.refresh_once().await;
    for i in 0..5 {
        tx.send(envelope(&format!("m{i}"), "")).await.unwrap();
    }

    let (stop_tx, stop_rx) = watch::channel(false);
    let drops = Arc::new(Drops::new());
    let drain = tokio::spawn(drain_loop(pool.clone(), rx, stop_rx, drops.clone()));

    let batch = tokio::time::timeout(Duration::from_secs(10), sent_rx.recv())
        .await
        .expect("the drain must deliver")
        .expect("the spy caller published a batch");
    assert_eq!(
        topics(&batch),
        vec!["m0", "m1", "m2", "m3", "m4"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "all five ride in one batch, in produced order"
    );

    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .expect("the drain must observe the stop signal")
        .unwrap();
    assert!(
        sent_rx.try_recv().is_err(),
        "one batch means one call — there is no second one"
    );
    assert_eq!(drops.total.load(Ordering::SeqCst), 0, "nothing was dropped");
}

/// The `resolve_waited` latch. Against a pool that never resolves, the FIRST batch pays
/// the one-shot [`RESOLVE_WAIT`] startup window (so the messages produced before the
/// first resolve are not discarded) and every later batch pays nothing.
///
/// A regression to a per-batch wait is invisible except as this throttle: the second
/// batch would take another 5s. The instances are injected directly, so the pool has
/// somewhere to deliver while `applied_gen` stays 0 — i.e. it remains `Unresolved` for
/// the whole test, which is the only state in which the wait can be re-entered.
///
/// This is the one test here that spends real seconds: `RESOLVE_WAIT` is a const with
/// no injection seam, and its timer is a real `Instant` (a paused clock would spin it
/// forever). The second-batch bound keeps 5x headroom under the value it must not pay.
#[tokio::test]
async fn drain_pays_the_resolve_wait_once_not_per_batch() {
    let (tx, rx) = mpsc::channel(16);
    let (sent_tx, mut sent_rx) = mpsc::unbounded_channel();
    let pool = Arc::new(Pool::with_factory(
        never_resolves(),
        spy_factory(sent_tx.clone(), false),
    ));
    let caller: Arc<dyn Caller> = Arc::new(SpyCaller {
        sent: sent_tx,
        block: false,
    });
    pool.instances
        .lock()
        .unwrap()
        .push(fake_instance("A", caller, Arc::new(AtomicUsize::new(0))));
    assert_eq!(
        pool.fanout_state(),
        FanoutState::Unresolved,
        "the fixture must keep the pool in the state that arms the wait"
    );

    tx.send(envelope("first", "")).await.unwrap();
    let (stop_tx, stop_rx) = watch::channel(false);
    let drops = Arc::new(Drops::new());
    let started = std::time::Instant::now();
    let drain = tokio::spawn(drain_loop(pool.clone(), rx, stop_rx, drops.clone()));

    let first = tokio::time::timeout(RESOLVE_WAIT * 4, sent_rx.recv())
        .await
        .expect("the first batch must ship after the startup window")
        .unwrap();
    let after_first = started.elapsed();
    assert_eq!(topics(&first), vec!["first".to_string()]);
    assert!(
        after_first >= RESOLVE_WAIT - Duration::from_millis(500),
        "the first batch waits for the (never-arriving) resolve: {after_first:?}"
    );

    tx.send(envelope("second", "")).await.unwrap();
    let second = tokio::time::timeout(RESOLVE_WAIT * 4, sent_rx.recv())
        .await
        .expect("the second batch must ship")
        .unwrap();
    let second_batch_cost = started.elapsed() - after_first;
    assert_eq!(topics(&second), vec!["second".to_string()]);
    assert!(
        second_batch_cost < RESOLVE_WAIT / 5,
        "the startup wait is once per process, not once per batch: {second_batch_cost:?}"
    );

    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), drain).await.unwrap().unwrap();
}

/// Stop observed DURING a delivery: `send_batch` returns `false` after counting the
/// chunks it was still holding, and `drain_loop` then counts the queue behind it. The
/// exit is ACCOUNTED — the abort path this replaced ended just as quietly but reported
/// none of the 5 messages, and a drop total wrong by an order of magnitude is worse
/// than none.
#[tokio::test]
async fn drain_stopped_mid_delivery_counts_the_batch_and_the_queue_behind_it() {
    let (tx, rx) = mpsc::channel(16);
    let (sent_tx, mut sent_rx) = mpsc::unbounded_channel();
    // The instance publishes the batch and then never returns, so the stop signal lands
    // while `deliver_all` is in flight.
    let pool = Arc::new(Pool::with_factory(list_of(&["A"]), spy_factory(sent_tx, true)));
    pool.refresh_once().await;
    for i in 0..3 {
        tx.send(envelope(&format!("inflight{i}"), "")).await.unwrap();
    }

    let (stop_tx, stop_rx) = watch::channel(false);
    let drops = Arc::new(Drops::new());
    let drain = tokio::spawn(drain_loop(pool.clone(), rx, stop_rx, drops.clone()));

    // Happens-before: the batch is on the wire, so the stop below is observed DURING
    // the delivery, not before it.
    let inflight = tokio::time::timeout(Duration::from_secs(10), sent_rx.recv())
        .await
        .expect("the drain must start delivering")
        .unwrap();
    assert_eq!(inflight.len(), 3);

    // Two more messages queue up behind the stalled delivery.
    tx.send(envelope("behind0", "")).await.unwrap();
    tx.send(envelope("behind1", "")).await.unwrap();
    stop_tx.send(true).unwrap();

    tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .expect("a stalled delivery must not outlive the stop signal")
        .unwrap();
    assert_eq!(
        drops.total.load(Ordering::SeqCst),
        5,
        "3 in-flight + 2 still queued are all accounted for at exit"
    );
}

// ---------------------------------------------------------------------------
// The sink and the module.
// ---------------------------------------------------------------------------

/// A full queue answers [`push::Error::Backlogged`] — a counted drop, not backpressure.
/// The producer may be holding a durable-delivery transaction's connection, so blocking
/// (or awaiting) here would pause a whole subscription over a best-effort message.
#[test]
fn the_sink_drops_and_counts_instead_of_blocking_when_the_queue_is_full() {
    let sender = PushSender::with_capacity(PeerSource::pooled(Vec::new(), never_resolves()), 1);
    let sink: &dyn push::Sink = &*sender.sink;
    let msg = Message::new("inbox.changed", Vec::new());

    assert_eq!(
        sink.send(&Target::Player("p".into()), &msg).unwrap(),
        Delivered::Queued,
        "an accepted message is a promise to try, never a count"
    );
    let err = sink
        .send(&Target::Player("p".into()), &msg)
        .expect_err("the second message finds the depth-1 queue full");
    assert!(matches!(err, push::Error::Backlogged), "{err}");
    assert_eq!(sender.drops.total.load(Ordering::SeqCst), 1);
}

/// `stop` lets the drain reach its COUNTED exit and then tears the pool down, inside a
/// bound: the two messages the drain was carrying are reported as drops (an exit that
/// skipped `record_stop_loss` reports none) and the pool's instance teardown runs its
/// `close`. It does NOT discriminate the graceful join from the post-grace abort — the
/// abort path also leaves the notified drain a chance to run — so what is pinned here
/// is the accounting and the teardown, not which of the two branches produced them.
#[tokio::test]
async fn push_sender_stop_joins_the_drain_and_stops_the_pool() {
    let ctx = Context::new();
    let sender = PushSender::with_capacity(PeerSource::pooled(Vec::new(), never_resolves()), 16);
    // An instance the resolver can never remove (its errors leave the set in place), so
    // `Pool::stop` has something to tear down.
    let closed = Arc::new(AtomicUsize::new(0));
    let (sent_tx, _sent_rx) = mpsc::unbounded_channel();
    let caller: Arc<dyn Caller> = Arc::new(SpyCaller {
        sent: sent_tx,
        block: false,
    });
    sender
        .pool
        .instances
        .lock()
        .unwrap()
        .push(fake_instance("A", caller, closed.clone()));

    sender.start(&ctx).await.unwrap();
    let sink: &dyn push::Sink = &*sender.sink;
    let msg = Message::new("inbox.changed", Vec::new());
    sink.send(&Target::All, &msg).unwrap();
    sink.send(&Target::All, &msg).unwrap();

    tokio::time::timeout(Duration::from_secs(10), sender.stop(&ctx))
        .await
        .expect("stop must stay inside the module stop grace")
        .unwrap();

    assert_eq!(
        sender.drops.total.load(Ordering::SeqCst),
        2,
        "a JOINED drain accounts for what it was carrying; an aborted one reports nothing"
    );
    assert_eq!(closed.load(Ordering::SeqCst), 1, "the pool's instance was torn down");
    assert!(sender.pool.instances.lock().unwrap().is_empty());
    let err = sink
        .send(&Target::All, &msg)
        .expect_err("the drain is gone, so later sends are counted drops");
    assert!(matches!(err, push::Error::Transport(_)), "{err}");
}

/// The sender contributes the SINK and nothing else. Copying `Stub`'s `PEER_SLOT`
/// contribution would tell a co-hosted gateway's route table that the push front serves
/// this process's `#[http]` ops — it serves none, it RECEIVES a backplane call.
#[test]
fn push_sender_contributes_the_sink_and_never_a_peer_address() {
    let ctx = Context::new();
    let sender = PushSender::new(PeerSource::pooled(Vec::new(), never_resolves()));

    sender.init(&ctx).unwrap();

    assert_eq!(
        ctx.contributions(push::SINK_SLOT).len(),
        1,
        "exactly one sink reaches the slot core/app installs from"
    );
    assert!(
        ctx.contributions(opsapi::PEER_SLOT).is_empty(),
        "a push front provides no ops; it must not appear in the route table"
    );
}

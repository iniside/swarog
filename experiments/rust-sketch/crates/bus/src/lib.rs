//! The asynchronous, fire-and-forget in-process pub/sub — the default glue
//! between modules. A leaf: depends on no module, importable by everyone.
//!
//! [`Bus::publish`] never blocks and never returns a result: if you need a
//! synchronous answer, that's a service interface (see [`registry`](../registry)),
//! not an event. Each subscriber gets its OWN tokio task and its OWN FIFO mailbox
//! (an unbounded mpsc channel), so:
//!   - delivery to a single subscriber preserves publish order,
//!   - a slow subscriber can't stall the publisher or other subscribers,
//!   - a panicking handler is contained ([`std::panic::catch_unwind`]), never
//!     killing the subscriber loop or anyone else.
//!
//! State built from events is therefore eventually consistent — a read right
//! after a `publish` may not see its effect yet.
//!
//! ## Step-2 seam — durable transport (NOT YET PRESENT)
//! Go's `bus` also carries a nil-able `Transport` for the durable plane
//! (`EmitTx`/`OnTx`/`OnTxRaw`, `SetTransport`, `ErrNoTransport`), implemented by
//! `modules/messaging`. That is deliberately OUT of this step: only the async
//! in-process core lives here. Step 2 adds a `transport: Mutex<Option<Arc<dyn
//! Transport>>>` field plus those methods; nothing about the async core below
//! changes when it lands.

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A topic plus its erased payload. Topics are plain strings (`"match.finished"`).
/// The payload is an `Arc<dyn Any>` so one published value can be handed to every
/// subscriber's mailbox by cloning the `Arc`, not the payload.
#[derive(Clone)]
pub struct Event {
    pub topic: String,
    pub data: Arc<dyn Any + Send + Sync>,
}

/// A subscriber's untyped handler. Takes the event by reference so the framework
/// keeps ownership (topic is read for logging after the call).
pub type Handler = Box<dyn Fn(&Event) + Send>;

#[derive(Default)]
struct Inner {
    /// topic -> one sender per subscriber (each feeds a dedicated task).
    subs: HashMap<String, Vec<mpsc::UnboundedSender<Event>>>,
    /// every subscriber task, awaited on [`Bus::close`].
    tasks: Vec<JoinHandle<()>>,
}

/// The asynchronous, fire-and-forget in-process pub/sub. See the module docs.
#[derive(Default)]
pub struct Bus {
    inner: Mutex<Inner>,
}

impl Bus {
    pub fn new() -> Self {
        Bus::default()
    }

    /// Subscribes an untyped handler to a topic. Spawns the subscriber's task and
    /// mailbox. Prefer the typed [`Bus::on`]; this is the primitive it builds on.
    pub fn subscribe<F>(&self, topic: impl Into<String>, handler: F)
    where
        F: Fn(&Event) + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        let task = tokio::spawn(async move {
            // Loop ends when every sender is dropped AND the mailbox has drained
            // (see `close`), so shutdown is lossless and ordered.
            while let Some(event) = rx.recv().await {
                let topic = event.topic.clone();
                // Contain a handler panic to this one delivery, mirroring Go's
                // per-delivery recover — the subscriber survives for the next event.
                if catch_unwind(AssertUnwindSafe(|| handler(&event))).is_err() {
                    tracing::error!(topic, "bus: event handler panicked");
                }
            }
        });

        let mut inner = self.inner.lock().unwrap();
        inner.subs.entry(topic.into()).or_default().push(tx);
        inner.tasks.push(task);
    }

    /// Hands the event to each subscriber's mailbox and returns immediately.
    /// Prefer the typed [`Bus::emit`].
    pub fn publish(&self, event: Event) {
        let inner = self.inner.lock().unwrap();
        if let Some(boxes) = inner.subs.get(&event.topic) {
            for tx in boxes {
                // Unbounded send is non-blocking; only errors if the subscriber
                // task is gone (after close), which is a benign race at shutdown.
                let _ = tx.send(event.clone());
            }
        }
    }

    /// Publishes a typed event. Non-blocking, like [`Bus::publish`].
    pub fn emit<T: Any + Send + Sync + 'static>(&self, et: &EventType<T>, v: T) {
        self.publish(Event {
            topic: et.topic.clone(),
            data: Arc::new(v),
        });
    }

    /// Subscribes a typed handler. The signature is checked at compile time against
    /// the [`EventType`]'s `T`. The internal downcast cannot fail in practice —
    /// every value on this topic was put there by [`Bus::emit`] with the same `T`.
    pub fn on<T, F>(&self, et: &EventType<T>, handler: F)
    where
        T: Clone + Send + Sync + 'static,
        F: Fn(T) + Send + 'static,
    {
        self.subscribe(et.topic.clone(), move |e| {
            match e.data.downcast_ref::<T>() {
                Some(v) => handler(v.clone()),
                None => tracing::error!(topic = %e.topic, "bus: event payload type mismatch"),
            }
        });
    }

    /// Stops every subscriber once its mailbox has drained, then waits for all
    /// tasks to finish. Dropping the stored senders (by clearing `subs`) is what
    /// lets each `rx.recv()` return `None` after its queue empties. Call after the
    /// HTTP server has stopped so no new events arrive mid-drain.
    pub async fn close(&self) {
        let tasks = {
            let mut inner = self.inner.lock().unwrap();
            inner.subs.clear();
            std::mem::take(&mut inner.tasks)
        };
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// Binds a topic to its payload type `T` in ONE place. Publishers and subscribers
/// reference the same `EventType`, so they cannot disagree on topic-vs-payload: a
/// mismatch is a compile error, not a runtime panic. Declared once, at module
/// scope, in the owning `<module>events` crate.
///
/// `PhantomData<fn() -> T>` makes `EventType<T>: Send + Sync` for any `T` and
/// keeps it usable from a `static`/`LazyLock`.
pub struct EventType<T> {
    topic: String,
    _marker: PhantomData<fn() -> T>,
}

/// Declares an event: a topic plus the payload type it always carries.
pub fn define<T>(topic: impl Into<String>) -> EventType<T> {
    EventType {
        topic: topic.into(),
        _marker: PhantomData,
    }
}

impl<T> EventType<T> {
    pub fn topic(&self) -> &str {
        &self.topic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    // Drains by closing, so every queued event is delivered before close returns —
    // no arbitrary sleeps needed for the assertions.
    async fn settle(bus: &Bus) {
        bus.close().await;
    }

    #[tokio::test]
    async fn typed_emit_on_roundtrip() {
        let bus = Bus::new();
        let seen = Arc::new(Mutex::new(Vec::<i32>::new()));
        let et = define::<i32>("nums");
        {
            let seen = seen.clone();
            bus.on(&et, move |v| seen.lock().unwrap().push(v));
        }
        bus.emit(&et, 1);
        bus.emit(&et, 2);
        bus.emit(&et, 3);
        settle(&bus).await;
        assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn per_subscriber_fifo_order() {
        let bus = Bus::new();
        let et = define::<u32>("t");
        let a = Arc::new(Mutex::new(Vec::<u32>::new()));
        let b = Arc::new(Mutex::new(Vec::<u32>::new()));
        {
            let a = a.clone();
            bus.on(&et, move |v| a.lock().unwrap().push(v));
        }
        {
            let b = b.clone();
            bus.on(&et, move |v| b.lock().unwrap().push(v));
        }
        for i in 0..100 {
            bus.emit(&et, i);
        }
        settle(&bus).await;
        let expected: Vec<u32> = (0..100).collect();
        assert_eq!(*a.lock().unwrap(), expected);
        assert_eq!(*b.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn panicking_handler_does_not_stall_others() {
        let bus = Bus::new();
        let et = define::<u32>("t");
        let good = Arc::new(AtomicU32::new(0));
        // A subscriber that panics on every delivery.
        bus.on(&et, |_v| panic!("boom"));
        // An independent subscriber that must still receive everything.
        {
            let good = good.clone();
            bus.on(&et, move |_v| {
                good.fetch_add(1, Ordering::SeqCst);
            });
        }
        for i in 0..10 {
            bus.emit(&et, i);
        }
        settle(&bus).await;
        assert_eq!(good.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_block_publisher() {
        let bus = Bus::new();
        let et = define::<u32>("t");
        let done = Arc::new(AtomicU32::new(0));
        {
            let done = done.clone();
            // Deliberately slow handler; publish must not wait on it.
            bus.on(&et, move |_v| {
                std::thread::sleep(Duration::from_millis(1));
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        // These returns are effectively instant even though the handler is slow.
        for i in 0..20 {
            bus.emit(&et, i);
        }
        settle(&bus).await; // waits for the slow subscriber to drain
        assert_eq!(done.load(Ordering::SeqCst), 20);
    }

    #[tokio::test]
    async fn close_is_idempotent_on_no_subscribers() {
        let bus = Bus::new();
        bus.close().await;
    }
}

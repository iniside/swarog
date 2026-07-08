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
//! ## Durable transport seam (the [`Transport`] plane)
//! Alongside the async in-process core, the bus carries an optional, nil-able
//! [`Transport`] for the *durable* plane ([`Bus::emit_tx`] / [`Bus::on_tx`] /
//! [`Bus::on_tx_raw`], [`Bus::set_transport`], [`Error::NoTransport`]). The
//! transport itself is implemented by `core/messaging` (outbox log + inbox
//! dedup + relay) and installed via [`Bus::set_transport`] — so the dependency
//! points module → leaf and `bus` stays free of any module import (hard
//! constraint #1). The [`Transport`] deals ONLY in topic strings + bytes; the
//! generic payload `T` collapses to JSON exactly at the emit_tx/on_tx boundary,
//! so the transport never sees a type parameter. Nothing about the async core
//! above changes because this seam exists — an installed transport is opt-in.

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde::{de::DeserializeOwned, Serialize};
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
    /// The durable-plane hook — `None` until `core/messaging` installs it in
    /// its phase-1 `register` (see [`Bus::set_transport`]). Kept in its own lock
    /// so installing/reading it never contends with the async `subs`/`tasks`.
    transport: Mutex<Option<Arc<dyn Transport>>>,
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

    // ---- Durable plane -----------------------------------------------------

    /// Installs the durable [`Transport`]. **Panics on a double-set**, so a
    /// second installer is a loud programmer error rather than a silent override
    /// (mirroring Go's `SetTransport` and `registry::provide`'s duplicate panic).
    /// `core/messaging` calls this exactly once, in its phase-1 `register`.
    pub fn set_transport(&self, t: Arc<dyn Transport>) {
        let mut slot = self.transport.lock().unwrap();
        if slot.is_some() {
            panic!("bus: transport already set");
        }
        *slot = Some(t);
    }

    /// The installed transport, or [`Error::NoTransport`] if none — the exact
    /// resolution [`Bus::emit_tx`] performs before it will marshal anything, so a
    /// durable event is never silently dropped. Cloned out of the lock so callers
    /// never hold it across an `.await`.
    fn require_transport(&self) -> Result<Arc<dyn Transport>, Error> {
        self.transport
            .lock()
            .unwrap()
            .clone()
            .ok_or(Error::NoTransport)
    }

    /// Publishes a typed event on the *durable* plane, inside the caller's
    /// transaction. Unlike [`Bus::emit`] it is `async` (the enqueue is a DB write)
    /// and returns a [`Result`]: [`Error::NoTransport`] if no transport is
    /// installed (so the event is never silently lost), or the marshal / enqueue
    /// error otherwise. The generic payload is JSON-marshalled **here** — the
    /// exact point where `T` collapses to bytes for the transport.
    ///
    /// Takes `&mut sqlx::PgConnection`, not `&mut Transaction`: a `Transaction`
    /// derefs to `PgConnection`, so a caller inside a tx passes `&mut *tx` and no
    /// second `'c` lifetime drags through this signature (BLOCKER-1).
    pub async fn emit_tx<T: Serialize>(
        &self,
        conn: &mut sqlx::PgConnection,
        et: &EventType<T>,
        v: &T,
    ) -> Result<(), Error> {
        let transport = self.require_transport()?;
        let payload = encode(v)?;
        transport.enqueue_tx(conn, et.topic(), &payload).await
    }

    /// Subscribes a typed durable handler. `subscriber` is the stable dedup name
    /// identifying this subscription for inbox `(event_id, subscriber)` dedup. The
    /// handler receives an already-deserialized `T`; the `Vec<u8>` → `T` decode is
    /// the boundary this wrapper owns.
    ///
    /// **BLOCKER-2 — panics if no transport is installed.** Go's `OnTx` silently
    /// no-ops here; this sketch refuses to, because a dropped durable subscription
    /// that builds clean and never delivers is exactly the trap the split proof
    /// must not hide. The invariant that makes the panic safe: `core/messaging`
    /// installs the transport in its phase-1 `register`, so any consumer's phase-2
    /// `on_tx` (a later phase) always finds it. A process that legitimately hosts
    /// no durable plane simply must not call `on_tx`.
    ///
    /// The handler is a closure `(&mut PgConnection, T) -> BoxFuture<Result<()>>`.
    /// It is stored as a NAMED [`TxHandler`] trait object (not a bare `Fn`),
    /// because a `Fn` whose *return* future borrows the `&mut PgConnection`
    /// argument is a higher-ranked type Rust cannot infer through a stored boxed
    /// closure — the named trait pins the `for<'a>` shape once (BLOCKER-1).
    pub fn on_tx<T, F>(&self, et: &EventType<T>, subscriber: &str, handler: F)
    where
        T: DeserializeOwned + Send + 'static,
        F: for<'a> Fn(&'a mut sqlx::PgConnection, T) -> BoxFuture<'a, Result<(), Error>>
            + Send
            + Sync
            + 'static,
    {
        let wrapped: Arc<dyn TxHandler> = Arc::new(TypedAdapter {
            handler,
            _marker: PhantomData::<fn() -> T>,
        });
        self.on_tx_raw(et.topic(), subscriber, wrapped);
    }

    /// The untyped raw-bytes durable subscribe: registers a [`TxHandler`] that is
    /// handed the raw JSON payload, for a subscriber reacting to a topic string
    /// without importing the producer's `<module>events` crate (e.g. a cross-domain
    /// audit ledger). The primitive [`Bus::on_tx`] builds on. **Panics if no
    /// transport is installed** — same rationale as [`Bus::on_tx`].
    pub fn on_tx_raw(&self, topic: &str, subscriber: &str, handler: Arc<dyn TxHandler>) {
        let transport = self.transport.lock().unwrap().clone();
        match transport {
            Some(t) => t.subscribe_tx(topic, subscriber, handler),
            None => panic!(
                "bus: on_tx({topic:?}) but no durable transport installed — messaging must \
                 set_transport in its phase-1 register before any consumer's phase-2 on_tx"
            ),
        }
    }
}

/// The `T` → bytes collapse, in ONE place: everything durable marshals through
/// this so producer and consumer agree on the wire encoding (JSON). See [`decode`].
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, Error> {
    Ok(serde_json::to_vec(v)?)
}

/// The bytes → `T` inverse of [`encode`], used by [`Bus::on_tx`]'s typed wrapper.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Errors from the durable plane. [`Error::NoTransport`] is Go's `ErrNoTransport`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No durable transport installed — [`Bus::emit_tx`] returns this rather than
    /// silently dropping a durable event.
    #[error("bus: no durable transport installed")]
    NoTransport,
    /// JSON marshal/unmarshal failure at the `T` ⇄ bytes boundary.
    #[error("bus: JSON codec error: {0}")]
    Codec(#[from] serde_json::Error),
    /// A failure from the transport itself (e.g. the durable-log INSERT), boxed so
    /// the leaf `bus` need not name `sqlx::Error` in its public error type.
    #[error("bus: transport error: {0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl Error {
    /// Wraps a concrete transport/DB error into [`Error::Transport`]. A `Transport`
    /// impl uses this to surface e.g. a `sqlx::Error` without `bus` importing it.
    pub fn transport<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Error::Transport(Box::new(e))
    }
}

/// The durable plane's hook — a nil-able seam this leaf declares but never
/// implements (`core/messaging` does, installing it via [`Bus::set_transport`]).
/// It deals ONLY in topic strings + `[u8]`: the generic payload `T` is already
/// collapsed to bytes at the [`Bus::emit_tx`]/[`Bus::on_tx`] boundary, so the
/// transport never sees a type parameter (mirrors Go's `bus.Transport`).
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Writes the encoded event to the durable log **inside the caller's
    /// transaction** (the `conn` is `&mut *tx`), so persisting the event is atomic
    /// with the domain change. `async` because it is a DB write (Go's is sync `sql`).
    async fn enqueue_tx(
        &self,
        conn: &mut sqlx::PgConnection,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), Error>;

    /// Registers a durable handler for `topic`. `subscriber` is a stable name
    /// identifying this subscription for inbox dedup `(event_id, subscriber)`. The
    /// handler is a NAMED trait object (see [`TxHandler`] / BLOCKER-1), stored by
    /// the transport and later invoked with a per-delivery connection.
    fn subscribe_tx(&self, topic: &str, subscriber: &str, handler: Arc<dyn TxHandler>);
}

/// A durable handler, stored by a [`Transport`] and invoked once per delivered
/// event. It is a **named trait** — not a bare `Fn` — on purpose (BLOCKER-1):
/// `call` returns a future that borrows its `&'a mut PgConnection` argument, a
/// higher-ranked (`for<'a>`) relationship Rust cannot infer through a stored
/// boxed closure. Naming the trait pins that `'a` once.
///
/// The handler borrows `&mut sqlx::PgConnection`, NOT `&mut sqlx::Transaction<'c>`:
/// a `Transaction` derefs (`DerefMut`) to `PgConnection`, so the transport passes
/// `&mut *tx` and no second `'c` lifetime threads through every stored handler.
///
/// **On the dropped `cx` context param (option (b)):** Go's handler is
/// `func(ctx context.Context, tx *sql.Tx, T) error`. We drop the `ctx` equivalent
/// rather than thread a `&Ctx` through `TxHandler`, because `bus` is a leaf and
/// must NOT import `lifecycle` (that would invert the dependency — `lifecycle`
/// imports `bus`). A generic `cx` type would infect `dyn TxHandler`'s object
/// safety and every stored handler; instead the user's [`Bus::on_tx`] closure
/// **captures** whatever it needs (services, config). A `context.Context`-style
/// cancellation token can be added later as a plain `&CancellationToken` param
/// with no dependency inversion; it is omitted here as unused by the sketch.
pub trait TxHandler: Send + Sync {
    fn call<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Error>>;
}

/// Adapts a typed [`Bus::on_tx`] closure into a raw [`TxHandler`]: it owns the
/// `Vec<u8>` → `T` decode (the inverse of [`encode`]) before delegating to the
/// user closure. `PhantomData<fn() -> T>` keeps the adapter `Send + Sync` for any
/// `T` without owning one.
struct TypedAdapter<T, F> {
    handler: F,
    _marker: PhantomData<fn() -> T>,
}

impl<T, F> TxHandler for TypedAdapter<T, F>
where
    T: DeserializeOwned + Send + 'static,
    F: for<'a> Fn(&'a mut sqlx::PgConnection, T) -> BoxFuture<'a, Result<(), Error>> + Send + Sync,
{
    fn call<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let v: T = decode(&payload)?;
            (self.handler)(conn, v).await
        })
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
mod tests;

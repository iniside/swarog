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
//! Alongside the async in-process core, the bus carries an optional
//! [`Transport`] for the *durable* plane ([`Bus::emit_tx`] / [`Bus::on_tx`] /
//! [`Bus::on_tx_raw`], [`Error::NoTransport`]). The transport itself is
//! implemented by `core/asyncevents` (XID-ordered shared log + consumer-owned
//! pull subscriptions with transactional checkpoints) and
//! injected at construction by the composition root (`core/app` builds
//! [`Bus::with_transport`] iff the process has a DB) — so the dependency
//! points module → leaf and `bus` stays free of any module import (hard
//! constraint #1). The [`Transport`] deals ONLY in contracts, topics + bytes; the
//! generic payload `T` collapses to JSON exactly at the emit_tx/on_tx boundary,
//! so the transport never sees a type parameter. Nothing about the async core
//! above changes because this seam exists — a transport is opt-in, and
//! immutable once the bus exists (no runtime installer, no double-set class).

use std::any::Any;
use std::collections::{HashMap, HashSet};
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

/// The bus-side record of every durable registration. The `ids` set is the
/// duplicate-`spec.id` guard — it lives HERE (not in a transport) so the
/// invariant "a subscription id names exactly one checkpoint" holds under ANY
/// transport, including the checkers' recording doubles. `list` feeds
/// [`Bus::subscriptions`] (topiccheck's durable-plane view).
#[derive(Default)]
struct DurableSubs {
    ids: HashSet<&'static str>,
    list: Vec<(&'static str, String)>,
}

/// The asynchronous, fire-and-forget in-process pub/sub. See the module docs.
#[derive(Default)]
pub struct Bus {
    inner: Mutex<Inner>,
    /// The durable-plane hook — fixed at construction ([`Bus::with_transport`],
    /// called by the composition root when the process hosts a durable-events
    /// plane) and `None` for a plane-less bus ([`Bus::new`]). Immutable, so no
    /// lock and no double-install class.
    transport: Option<Arc<dyn Transport>>,
    /// Durable registrations: the duplicate-id guard + the introspection list.
    durable: Mutex<DurableSubs>,
    /// Every `(topic, version)` subscribed in-process via the typed [`Bus::on`],
    /// in registration order. Populated ONLY by `on` (which holds the
    /// [`EventType`] and thus the contract version) — a raw
    /// [`Bus::subscribe`] call carries no version and is NOT recorded here (it
    /// stays visible in [`Bus::subscribed_topics`] instead). Introspection only,
    /// consumed by `topiccheck`'s version-aware in-process durability check.
    inprocess_contracts: Mutex<Vec<(String, u32)>>,
}

impl Bus {
    /// A bus with NO durable plane (in-process only). [`Bus::emit_tx`] returns
    /// [`Error::NoTransport`] and [`Bus::on_tx`]/[`Bus::on_tx_raw`] panic — the
    /// right shape for unit tests and for a DB-less process.
    pub fn new() -> Self {
        Bus::default()
    }

    /// A bus whose durable plane is live from birth. The composition root
    /// (`core/app`) builds this iff the process has a DB, passing
    /// `core/asyncevents`'s transport — so every module's `on_tx` (a later
    /// wiring phase) always finds it installed.
    pub fn with_transport(t: Arc<dyn Transport>) -> Self {
        Bus {
            transport: Some(t),
            ..Bus::default()
        }
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
            topic: et.topic().to_string(),
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
        // Record the versioned contract in parallel with the topic-only
        // `subscribe` below: `subscribe` erases the version (it takes a bare
        // topic string), so the version is captured HERE, the only site that
        // still holds the `EventType`.
        self.inprocess_contracts
            .lock()
            .unwrap()
            .push((et.topic().to_string(), et.contract().version));
        self.subscribe(et.topic().to_string(), move |e| {
            match e.data.downcast_ref::<T>() {
                Some(v) => handler(v.clone()),
                None => tracing::error!(topic = %e.topic, "bus: event payload type mismatch"),
            }
        });
    }

    /// Every topic that currently has at least one in-process subscriber (from
    /// [`Bus::subscribe`] / [`Bus::on`]). Introspection only — used by the
    /// `topiccheck` tool to diff defined-vs-subscribed topics across the in-process
    /// plane (the durable plane is observed at the [`Transport`] instead). The order
    /// is unspecified.
    pub fn subscribed_topics(&self) -> Vec<String> {
        self.inner.lock().unwrap().subs.keys().cloned().collect()
    }

    /// Every `(topic, version)` subscribed in-process via the typed [`Bus::on`],
    /// in registration order. The version-aware companion to
    /// [`Bus::subscribed_topics`]: `on` holds the [`EventType`] and so can record
    /// the contract version, which the bare-string [`Bus::subscribe`] cannot. Used
    /// by `topiccheck` to catch a defined contract `(topic, version)` subscribed
    /// in-process (a durability violation) without regressing the topic-level view
    /// of raw `subscribe` callers. Introspection only; duplicates are kept (one
    /// entry per `on` call). The order is registration order.
    pub fn subscribed_contracts(&self) -> Vec<(String, u32)> {
        self.inprocess_contracts.lock().unwrap().clone()
    }

    /// Every durable subscription registered on this bus, as `(id, topic)`
    /// pairs in registration order. Introspection only — the durable-plane
    /// counterpart of [`Bus::subscribed_topics`], consumed by `topiccheck`.
    pub fn subscriptions(&self) -> Vec<(String, String)> {
        self.durable
            .lock()
            .unwrap()
            .list
            .iter()
            .map(|(id, topic)| (id.to_string(), topic.clone()))
            .collect()
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

    /// The installed transport, or [`Error::NoTransport`] if none — the exact
    /// resolution [`Bus::emit_tx`] performs before it will marshal anything, so a
    /// durable event is never silently dropped.
    fn require_transport(&self) -> Result<Arc<dyn Transport>, Error> {
        self.transport.clone().ok_or(Error::NoTransport)
    }

    /// Publishes a typed event on the *durable* plane, inside the caller's
    /// transaction. Unlike [`Bus::emit`] it is `async` (the enqueue is a DB write)
    /// and returns a [`Result`]: [`Error::NoTransport`] if no transport is
    /// installed (so the event is never silently lost), or the marshal / enqueue
    /// error otherwise. The generic payload is JSON-marshalled **here** — the
    /// exact point where `T` collapses to bytes for the transport.
    ///
    /// Takes an [`AnyTx`], constructed from the deref of your concrete tx
    /// (`AnyTx::new(&mut *tx)`) — erasure after the deref, because a
    /// `Transaction<'c>` is not `'static` (only the derefed connection type is
    /// [`Any`]); the wrapper borrows only for the call, never past your commit.
    /// The seam therefore names no engine: the transport downcasts to ITS engine's
    /// connection type and returns [`Error::TxEngineMismatch`] if the caller's
    /// store is a different engine (BLOCKER-1 rationale for the erased shape:
    /// a generic `Bus<Tx>` would infect `Context`, every stored handler, and the
    /// object-safe `dyn Transport`).
    pub async fn emit_tx<T: Serialize>(
        &self,
        tx: AnyTx<'_>,
        et: &EventType<T>,
        v: &T,
    ) -> Result<(), Error> {
        let transport = self.require_transport()?;
        let payload = encode(v)?;
        transport.enqueue_tx(tx, et.contract(), &payload).await
    }

    /// Subscribes a typed durable handler. `spec` is the consumer-owned
    /// subscription descriptor: `spec.id` is the stable, globally-unique
    /// checkpoint name (convention `<consumer>.<topic-kebab>.v<version>`),
    /// `spec.start` where a NEW subscription begins reading the log. The
    /// handler receives an already-deserialized `T`; the `Vec<u8>` → `T` decode is
    /// the boundary this wrapper owns.
    ///
    /// **BLOCKER-2 — panics if no transport is installed.** Go's `OnTx` silently
    /// no-ops here; this sketch refuses to, because a dropped durable subscription
    /// that builds clean and never delivers is exactly the trap the split proof
    /// must not hide. The invariant that makes the panic safe: the transport is a
    /// constructor argument ([`Bus::with_transport`], built by `core/app` iff the
    /// process has a DB), so it exists before any module wiring runs. A bus
    /// without one belongs to a process that hosts no durable-events plane (no
    /// DB) — a durable subscriber simply cannot run there.
    ///
    /// The handler is a closure `(Delivery<'a>, T) -> BoxFuture<'a, Result<()>>`.
    /// It is stored as a NAMED [`TxHandler`] trait object (not a bare `Fn`),
    /// because a `Fn` whose *return* future borrows the [`Delivery`] argument is
    /// a higher-ranked type Rust cannot infer through a stored boxed closure —
    /// the named trait pins the `for<'a>` shape once (BLOCKER-1).
    ///
    /// The generalized contract: delivery is at-least-once with a stable
    /// [`Delivery::event_id`]; effects are exactly-once iff the dedup-check and
    /// the effect are atomic in the consumer's OWN store — via the handed
    /// delivery tx (downcast [`Delivery::tx`] to your engine's connection type)
    /// when engines match, via an idempotent `event_id`-keyed write otherwise.
    pub fn on_tx<T, F>(&self, spec: SubscriptionSpec, et: &EventType<T>, handler: F)
    where
        T: DeserializeOwned + Send + 'static,
        F: for<'a> Fn(Delivery<'a>, T) -> BoxFuture<'a, Result<(), Error>>
            + Send
            + Sync
            + 'static,
    {
        let wrapped: Arc<dyn TxHandler> = Arc::new(TypedAdapter {
            handler,
            _marker: PhantomData::<fn() -> T>,
        });
        let contract = *et.contract();
        self.subscribe_durable(
            spec,
            et.topic(),
            contract.version,
            Some(contract.history),
            wrapped,
        );
    }

    /// The untyped raw-bytes durable subscribe: registers a [`TxHandler`] that is
    /// handed the raw JSON payload, for a subscriber reacting to a topic string
    /// without importing the producer's `<module>events` crate (e.g. a cross-domain
    /// audit ledger). A raw subscription names no [`EventType`], so it pins
    /// contract version 1 — the only published version today; a raw consumer of a
    /// later version gets a versioned variant of this method then. **Panics if no
    /// transport is installed** — same rationale as [`Bus::on_tx`].
    pub fn on_tx_raw(&self, spec: SubscriptionSpec, topic: &str, handler: Arc<dyn TxHandler>) {
        // A raw subscriber names no `EventType`, so it carries NO history contract:
        // the PRODUCER owns the `history_contracts` row (native-writer path). `None`
        // tells the plane's reconcile to leave the retention policy to the producer.
        self.subscribe_durable(spec, topic, 1, None, handler);
    }

    /// The one durable-subscribe funnel: transport presence check (loud, see
    /// [`Bus::on_tx`]), then the duplicate-`spec.id` guard + introspection record
    /// (bus-owned, so they hold under ANY transport), then the transport handoff.
    fn subscribe_durable(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        version: u32,
        history: Option<HistoryPolicy>,
        handler: Arc<dyn TxHandler>,
    ) {
        let Some(t) = &self.transport else {
            panic!(
                "bus: on_tx({topic:?}) but this process hosts no durable-events plane (no DB) \
                 — a durable subscriber cannot run here"
            );
        };
        {
            let mut durable = self.durable.lock().unwrap();
            assert!(
                durable.ids.insert(spec.id),
                "bus: duplicate durable subscription id {:?} (topic {topic:?}) — a subscription \
                 id names exactly one consumer-owned checkpoint and must be globally unique",
                spec.id
            );
            durable.list.push((spec.id, topic.to_string()));
        }
        t.subscribe_tx(spec, topic, version, history, handler);
    }
}

/// A type-erased mutable borrow of the caller's transactional context.
/// Producer side ([`Bus::emit_tx`]): YOUR domain tx. Consumer side
/// ([`Delivery::tx`]): the plane's delivery tx. The events seam never names an
/// engine; the party that owns the concrete store downcasts (its own engine is
/// its own business).
///
/// Constructed from the deref of a concrete tx (`AnyTx::new(&mut *tx)`) —
/// erasure must happen after the deref because e.g. `sqlx::Transaction<'c>` is
/// not `'static` (so not [`Any`]) while the derefed connection type is. The
/// wrapper borrows only for the duration of one call, never past the caller's
/// commit.
///
/// The second field is the concrete type's name, captured at construction: a
/// `&mut dyn Any` alone yields only a `TypeId` at downcast time, so a mismatch
/// error could not name what it GOT without it.
pub struct AnyTx<'a>(&'a mut (dyn Any + Send), &'static str);

impl<'a> AnyTx<'a> {
    /// Erases a concrete transactional borrow (typically the deref of a
    /// transaction: `AnyTx::new(&mut *tx)`), remembering `T`'s type name for
    /// the mismatch error.
    pub fn new<T: Any + Send>(tx: &'a mut T) -> AnyTx<'a> {
        AnyTx(tx, std::any::type_name::<T>())
    }

    /// The concrete transaction, or [`Error::TxEngineMismatch`] naming both the
    /// expected and the supplied type. Callers need a `mut` binding
    /// (`mut delivery` / `mut tx`) to downcast.
    pub fn downcast<T: Any>(&mut self) -> Result<&mut T, Error> {
        let got = self.1;
        self.0
            .downcast_mut::<T>()
            .ok_or(Error::TxEngineMismatch {
                expected: std::any::type_name::<T>(),
                got,
            })
    }
}

/// What a durable handler receives per delivery, alongside the decoded payload.
/// A struct (not bare args) so a later field — e.g. producer `origin` — can be
/// added without re-breaking every handler signature.
pub struct Delivery<'a> {
    /// A stable, opaque idempotency key for effects in a store the plane's tx
    /// cannot reach — stable across redeliveries. Treat it as an opaque string;
    /// the plane owns its composition.
    pub event_id: &'a str,
    /// The plane's delivery transaction. Downcast it in your store layer if your
    /// store shares the plane's engine; ignore it otherwise.
    pub tx: AnyTx<'a>,
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
    /// An [`AnyTx::downcast`] found a different engine's transaction than the
    /// downcaster's: the composition wired a producer/consumer whose store engine
    /// does not match the transport's (or the handler's) — a composition-root
    /// bug, surfaced loudly with both concrete type names.
    #[error(
        "bus: transactional-context engine mismatch: expected `{expected}`, got `{got}` — \
         the caller's store engine does not match the downcaster's"
    )]
    TxEngineMismatch {
        expected: &'static str,
        got: &'static str,
    },
}

impl Error {
    /// Wraps a concrete transport/DB error into [`Error::Transport`]. A `Transport`
    /// impl uses this to surface e.g. a `sqlx::Error` without `bus` importing it.
    pub fn transport<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Error::Transport(Box::new(e))
    }
}

/// The durable plane's hook — a seam this leaf declares but never implements
/// (`core/asyncevents` does; `core/app` injects it via [`Bus::with_transport`]).
/// It deals ONLY in contracts, topic strings + `[u8]`: the generic payload `T` is
/// already collapsed to bytes at the [`Bus::emit_tx`]/[`Bus::on_tx`] boundary, so
/// the transport never sees a type parameter (mirrors Go's `bus.Transport`).
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Writes the encoded event to the durable log **inside the caller's
    /// transaction** (the [`AnyTx`] erases `&mut *tx`), so persisting the event is
    /// atomic with the domain change. The transport downcasts to its OWN engine's
    /// connection type; a caller whose store is a different engine gets
    /// [`Error::TxEngineMismatch`]. `async` because it is a DB write (Go's is
    /// sync `sql`).
    async fn enqueue_tx(
        &self,
        tx: AnyTx<'_>,
        contract: &EventContract,
        payload: &[u8],
    ) -> Result<(), Error>;

    /// Registers a durable handler for `topic` at exactly `version`. `spec` is the
    /// consumer-owned subscription descriptor — `spec.id` the stable checkpoint
    /// name, `spec.start` where a NEW subscription begins. The handler is a NAMED
    /// trait object (see [`TxHandler`] / BLOCKER-1), stored by the transport and
    /// later invoked with a per-delivery [`Delivery`] (the stable event id + the
    /// transport's erased delivery tx). Duplicate `spec.id`s never reach a
    /// transport — [`Bus`] panics first.
    ///
    /// `history` is the publisher's [`HistoryPolicy`] carried by the subscribed
    /// [`EventType`] (`Some` for a typed [`Bus::on_tx`]), or `None` for a raw
    /// [`Bus::on_tx_raw`] subscriber that names no `EventType` — the plane uses it
    /// only to seed the retention contract; the producer owns that row regardless.
    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        version: u32,
        history: Option<HistoryPolicy>,
        handler: Arc<dyn TxHandler>,
    );
}

/// A durable handler, stored by a [`Transport`] and invoked once per delivered
/// event. It is a **named trait** — not a bare `Fn` — on purpose (BLOCKER-1):
/// `call` returns a future that borrows its [`Delivery`] argument, a
/// higher-ranked (`for<'a>`) relationship Rust cannot infer through a stored
/// boxed closure. Naming the trait pins that `'a` once.
///
/// The handler receives a [`Delivery`]: a stable `event_id` plus the transport's
/// erased delivery tx (constructed by the transport as `AnyTx::new(&mut *tx)` —
/// erasure after the deref, because a `Transaction<'c>` is not `'static`; the
/// wrapper borrows only for the call, never past the transport's commit). The
/// generalized contract: delivery is at-least-once with a stable `event_id`;
/// effects are exactly-once iff the dedup-check and the effect are atomic in the
/// consumer's own store — via the handed delivery tx when engines match, via an
/// idempotent `event_id`-keyed write otherwise.
///
/// Handler futures are awaited INLINE by the plane (inside its dedup tx), never
/// spawned — an implementation must not require `'static` of the delivery
/// borrow, and a future [`Transport`] must keep the inline-await shape.
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
    fn call<'a>(&'a self, delivery: Delivery<'a>, payload: Vec<u8>)
        -> BoxFuture<'a, Result<(), Error>>;
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
    F: for<'a> Fn(Delivery<'a>, T) -> BoxFuture<'a, Result<(), Error>> + Send + Sync,
{
    fn call<'a>(
        &'a self,
        delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let v: T = decode(&payload)?;
            (self.handler)(delivery, v).await
        })
    }
}

/// What the PUBLISHER promises about a topic's history: how long delivered
/// events stay readable in the durable log. Consumed by the plane's retention
/// GC — a `StartPosition::Genesis` consumer added later can only replay what
/// the policy retained ([`HistoryPolicy::KeepForever`] is required before any
/// replay-from-genesis consumer exists).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryPolicy {
    /// Events are retained at least `days` days past every subscription's
    /// checkpoint (checkpoint-coupled: an unconsumed event is never deleted).
    MinRetention { days: u32 },
    /// Events on this topic are never deleted.
    KeepForever,
}

/// The publisher-owned contract of one durable topic: the topic string, the
/// payload-shape version (a NEW version is a NEW wire shape — consumers
/// subscribe to exactly one), and the history promise. Carried by every
/// [`EventType`] (via [`define`]) and handed whole to the transport at
/// [`Bus::emit_tx`], so the durable log records version + policy per event
/// stream without the transport importing any events crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventContract {
    pub topic: &'static str,
    pub version: u32,
    pub history: HistoryPolicy,
}

/// A position in the durable log — the plane's cursor coordinates
/// `(generation, producer_xid, tie_breaker)`. Opaque to modules; named here so
/// [`StartPosition::Explicit`] can carry one without the leaf `bus` importing
/// the plane. `xid` is a `u64` because Postgres `xid8` has no sqlx codec —
/// it crosses the boundary as text and is compared in SQL, never in Rust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventPosition {
    pub generation: i64,
    pub xid: u64,
    pub tie: i64,
}

/// Where a NEW durable subscription starts reading. No default on purpose:
/// the consumer must decide whether history matters to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartPosition {
    /// From the beginning of retained history.
    Genesis,
    /// Only events appended after this subscription was first registered.
    AfterRegistration,
    /// From an explicit log position (operator tooling / recovery).
    Explicit(EventPosition),
}

/// The consumer-owned durable subscription descriptor. `id` is the stable,
/// globally-unique checkpoint name (convention
/// `<consumer>.<topic-kebab>.v<version>`, e.g. `inventory.character-created.v1`)
/// — renaming it abandons the old checkpoint. `start` applies only when the
/// subscription row does not exist yet; an existing checkpoint always wins.
#[derive(Clone, Copy, Debug)]
pub struct SubscriptionSpec {
    pub id: &'static str,
    pub start: StartPosition,
}

/// Binds a topic to its payload type `T` in ONE place. Publishers and subscribers
/// reference the same `EventType`, so they cannot disagree on topic-vs-payload: a
/// mismatch is a compile error, not a runtime panic. Declared once, at module
/// scope, in the owning `<module>events` crate. Carries the full
/// [`EventContract`] — topic, version, history — so emit and subscribe agree on
/// the whole contract, not just the topic string.
///
/// `PhantomData<fn() -> T>` makes `EventType<T>: Send + Sync` for any `T` and
/// keeps it usable from a `static`/`LazyLock`.
pub struct EventType<T> {
    contract: EventContract,
    _marker: PhantomData<fn() -> T>,
}

/// Declares an event: a topic, the contract version of its payload shape, the
/// publisher's history promise, and (via `T`) the payload type it always carries.
pub fn define<T>(topic: &'static str, version: u32, history: HistoryPolicy) -> EventType<T> {
    EventType {
        contract: EventContract {
            topic,
            version,
            history,
        },
        _marker: PhantomData,
    }
}

impl<T> EventType<T> {
    pub fn topic(&self) -> &str {
        self.contract.topic
    }

    pub fn contract(&self) -> &EventContract {
        &self.contract
    }
}

#[cfg(test)]
mod tests;

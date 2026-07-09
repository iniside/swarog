//! The push-side transport of the LEGACY delivery path: the [`bus::Transport`]
//! over `asyncevents.outbox`, the per-subscriber inbox-dedup `consume`, the relay
//! local targets, and the `POST /events` inbound sink. Moved verbatim from
//! `lib.rs` at the Step-2 file split — behavior unchanged; this remains the live
//! delivery mechanism until the V2 pull worker cuts over (plan Step 3).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bus::{AnyTx, Delivery, EventContract, SubscriptionSpec, Transport, TxHandler};
use sqlx::PgPool;

/// One in-process durable subscription: a stable dedup name + its bytes-level handler.
type Subscription = (String, Arc<dyn TxHandler>);
/// topic -> its durable subscriptions.
type TopicHandlers = HashMap<String, Vec<Subscription>>;

/// The shared durable-plane state, held behind ONE `Arc` so the [`Transport`] impl,
/// the `POST /events` sink, and the relay's local targets all see the same
/// subscription table: a `subscribe_tx` registration is visible to every delivery
/// path. A single `Arc<Inner>` is handed to `Bus::with_transport`, the axum
/// handler, and the relay closures.
pub struct Inner {
    pool: PgPool,
    /// From `EVENTS_ORIGIN`, default `"monolith"`. Stamped on every enqueued row.
    origin: String,
    /// topic -> in-process durable subscriptions `(subscriber, handler)`.
    ///
    /// Live from [`crate::Plane::new`] — BEFORE the `Context` (and thus any module
    /// wiring) exists — so every `on_tx`, whether from a module's `init` or a stub
    /// factory's `register`, appends to a present map. [`crate::Plane::start`]
    /// snapshots it after all wiring is done.
    local_handlers: Mutex<TopicHandlers>,
}

#[async_trait::async_trait]
impl Transport for Inner {
    /// Writes one outbox row inside the PRODUCER's domain tx (the [`AnyTx`] erases
    /// `&mut *tx`), so the event is durable iff the domain change commits. Stamps
    /// `self.origin` (the producer never sets it) and does NOT commit — the caller
    /// owns the tx.
    ///
    /// The downcast is THE producer-side engine gate: this plane's outbox is
    /// Postgres, so a producer whose store tx is any other engine gets
    /// [`bus::Error::TxEngineMismatch`] — surfacing at the FIRST EMIT, which can
    /// be arbitrarily post-boot (a request-path emit fails on first use, not at
    /// startup).
    async fn enqueue_tx(
        &self,
        mut tx: AnyTx<'_>,
        contract: &EventContract,
        payload: &[u8],
    ) -> Result<(), bus::Error> {
        let conn = tx.downcast::<sqlx::PgConnection>()?;
        // Step-1 shim: this push plane predates versioned contracts, so only
        // `contract.topic` reaches the outbox; version + history land with the
        // V2 event log (`crate::store`, cutover at plan Step 3).
        // Bind the payload as text so `::jsonb` parses it (a bytea bind would try to
        // cast raw bytes). The bus already JSON-encoded it, so it is valid UTF-8.
        let text = std::str::from_utf8(payload).map_err(bus::Error::transport)?;
        sqlx::query("INSERT INTO asyncevents.outbox (origin, topic, payload) VALUES ($1, $2, $3::jsonb)")
            .bind(&self.origin)
            .bind(contract.topic)
            .bind(text)
            .execute(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
        Ok(())
    }

    /// Records an in-process durable subscription. Called during module wiring
    /// (any phase — the map is live from [`crate::Plane::new`]), so it only appends;
    /// [`crate::Plane::start`] later snapshots these into relay local targets.
    /// Step-1 shim: `spec.id` becomes the inbox-dedup subscriber string (the
    /// dedup keys change; irrelevant — the DB is wiped at the Step 3 cutover);
    /// `spec.start` and `version` are V2 vocabulary this push plane never reads.
    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        _version: u32,
        handler: Arc<dyn TxHandler>,
    ) {
        self.local_handlers
            .lock()
            .unwrap()
            .entry(topic.to_string())
            .or_default()
            .push((spec.id.to_string(), handler));
    }
}

impl Inner {
    pub(crate) fn new(pool: PgPool, origin: String) -> Inner {
        Inner {
            pool,
            origin,
            local_handlers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    /// Runs one subscriber's handler exactly once for `event_id`. In ONE tx it claims
    /// the event in the inbox keyed `(event_id, subscriber)` (`ON CONFLICT DO
    /// NOTHING`); a first delivery (1 row) runs the handler within the SAME tx before
    /// commit, a duplicate (0 rows) is a committed no-op. Any handler error rolls back
    /// (the tx drops) and propagates → the row stays unsent (local) / a 500 is
    /// returned (inbound) → redelivered next tick. Each subscriber gets its OWN tx and
    /// its OWN inbox row, so a failing subscriber can never roll back another's effect.
    ///
    /// The handler receives a [`Delivery`]: the stable `event_id` plus this dedup
    /// tx erased as an [`AnyTx`]. For an engine-matched (Postgres-store) consumer
    /// nothing changed semantically — downcasting and writing through the handed
    /// tx makes its effect atomic with the inbox marker, exactly as before. A
    /// foreign-store consumer instead keys an idempotent write in ITS store on
    /// `event_id` (the plane's inbox then only bounds redelivery).
    pub(crate) async fn consume(
        &self,
        subscriber: &str,
        event_id: &str,
        payload: &[u8],
        handler: Arc<dyn TxHandler>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            "INSERT INTO asyncevents.inbox (event_id, subscriber) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(event_id)
        .bind(subscriber)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            tx.commit().await?; // already processed by this subscriber — idempotent no-op
            return Ok(());
        }
        // Run the handler on the SAME connection/tx, so its effect + the inbox marker
        // commit or roll back together (for an engine-matched consumer writing
        // through the handed delivery tx).
        handler
            .call(
                Delivery {
                    event_id,
                    tx: AnyTx::new(&mut *tx),
                },
                payload.to_vec(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("durable handler failed: {e}"))?;
        tx.commit().await?;
        Ok(())
    }

    /// Snapshot of the subscribers for `topic` (for the inbound sink).
    pub(crate) fn subscribers_for(&self, topic: &str) -> Vec<Subscription> {
        self.local_handlers
            .lock()
            .unwrap()
            .get(topic)
            .cloned()
            .unwrap_or_default()
    }

    /// Snapshots `local_handlers` into one relay [`outbox::LocalTarget`] per
    /// (topic, subscriber). The relay delivers EVERY drained row to EVERY local
    /// target (it is not topic-scoped), so each target filters by topic: a row of a
    /// different topic is a no-op success, and only a matching row runs `consume`.
    /// Per-target = per-subscriber isolation.
    pub(crate) fn build_local_targets(self: &Arc<Self>) -> Vec<outbox::LocalTarget> {
        let handlers = self.local_handlers.lock().unwrap();
        let mut targets = Vec::new();
        for (topic, subs) in handlers.iter() {
            for (subscriber, handler) in subs {
                let inner = self.clone();
                let want_topic = topic.clone();
                let subscriber = subscriber.clone();
                let handler = handler.clone();
                targets.push(outbox::LocalTarget {
                    subscriber: subscriber.clone(),
                    deliver: Arc::new(move |delivered_topic: String, payload: Vec<u8>, event_id: String| {
                        let inner = inner.clone();
                        let want_topic = want_topic.clone();
                        let subscriber = subscriber.clone();
                        let handler = handler.clone();
                        Box::pin(async move {
                            if delivered_topic != want_topic {
                                return Ok(()); // not this subscription's topic — nothing to do
                            }
                            inner.consume(&subscriber, &event_id, &payload, handler).await
                        })
                    }),
                });
            }
        }
        targets
    }
}

/// Test helper: a bare durable transport over `(pool, origin)`, with no relay,
/// LISTEN, or housekeeping behind it. Module integration tests pass this to
/// `Context::with_db_and_transport` to get real outbox writes + `on_tx` recording
/// without booting a whole [`crate::Plane`].
pub fn transport(pool: PgPool, origin: &str) -> Arc<dyn Transport> {
    Arc::new(Inner::new(pool, origin.to_string()))
}

/// The receiver side: a peer's relay POSTs a foreign event here. Delivers to EVERY
/// local subscriber of the topic, each via its own `consume` tx. If ANY subscriber
/// fails it replies 500 so the sender retries the whole event; the per-subscriber
/// inbox makes already-succeeded subscribers a no-op on that retry.
pub(crate) async fn handle_inbound(inner: Arc<Inner>, headers: HeaderMap, body: Bytes) -> Response {
    let event_id = header(&headers, "X-Event-Id");
    let topic = header(&headers, "X-Event-Topic");
    if event_id.is_empty() || topic.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing event id or topic").into_response();
    }
    let subs = inner.subscribers_for(&topic);
    for (subscriber, handler) in subs {
        if let Err(err) = inner.consume(&subscriber, &event_id, &body, handler).await {
            tracing::error!(%subscriber, %topic, %event_id, %err, "inbound consume failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    }
    StatusCode::OK.into_response()
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

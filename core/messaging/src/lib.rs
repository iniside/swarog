//! `messaging` — the durable async plane's one and only module. It owns schema
//! `messaging` (a shared outbox log + a per-subscriber inbox dedup ledger),
//! implements [`bus::Transport`], and installs it via `ctx.bus().set_transport` — so
//! the `bus` leaf gains a durable plane WITHOUT importing any module (hard
//! constraint #1: dependency points module → leaf, never the reverse). It is the
//! ONLY module that implements [`bus::Transport`] and imports `outbox`. (Port of
//! Go's `modules/messaging`.)
//!
//! A producer reaches it purely via `bus.emit_tx` (writes one `messaging.outbox` row
//! in the producer's own domain tx); a consumer via `bus.on_tx`/`on_tx_raw` (a
//! durable handler run inside a per-subscriber inbox-dedup tx). Neither ever sees the
//! outbox, the inbox, the relay, `EVENTS_SUBSCRIBERS`, or `MESSAGING_ORIGIN` —
//! messaging owns the whole envelope. Delivery is topology-transparent: the SAME code
//! path serves the monolith (in-process local targets) and a split (HTTP `POST
//! /events` to a peer), chosen by durability intent, never by topology.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use bus::{Transport, TxHandler};
use lifecycle::{Caps, Context, Module};
use outbox::{LocalTarget, Relay};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Fallback DSN for the LISTEN connection — the same default as the shared pool.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The stable identity a monolith stamps on its outbox rows when `MESSAGING_ORIGIN`
/// is unset. It must be stable across restarts so a crashed process resumes draining
/// its OWN unsent rows — never a pid/hostname.
const DEFAULT_ORIGIN: &str = "monolith";

/// The LISTEN/NOTIFY channel the outbox insert trigger fires on.
const NOTIFY_CHANNEL: &str = "messaging_outbox";

/// Bounds each retention DELETE so a prune never takes a long lock.
const HOUSEKEEP_BATCH: i64 = 1000;

/// One in-process durable subscription: a stable dedup name + its bytes-level handler.
type Subscription = (String, Arc<dyn TxHandler>);
/// topic -> its durable subscriptions.
type TopicHandlers = HashMap<String, Vec<Subscription>>;

/// The registry marker messaging provides under `"messaging"`. It exists only so a
/// process hosting a durable producer/consumer (which declares `requires("messaging")`)
/// fails loud at `validate_requires` when messaging is absent — the REAL wiring is via
/// `set_transport`, not a method here. No consumer requires it (they use the bus).
pub trait Service: Send + Sync {}

/// Creates this module's OWN schema — full logical isolation (#10). Idempotent
/// (`IF NOT EXISTS` / `OR REPLACE`). The `AFTER INSERT` trigger fires the `pg_notify`
/// the relay's LISTEN loop wakes on; the partial index keeps the unsent scan cheap;
/// the inbox PK `(event_id, subscriber)` is what makes dedup PER SUBSCRIBER so a
/// failing subscriber never blocks another's delivery. Verbatim from Go's `schemaDDL`.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS messaging;
CREATE TABLE IF NOT EXISTS messaging.outbox (
	id         bigserial   PRIMARY KEY,
	origin     text        NOT NULL,
	topic      text        NOT NULL,
	payload    jsonb       NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	sent_at    timestamptz
);
CREATE INDEX IF NOT EXISTS outbox_unsent_idx ON messaging.outbox (id) WHERE sent_at IS NULL;
CREATE TABLE IF NOT EXISTS messaging.inbox (
	event_id     text        NOT NULL,
	subscriber   text        NOT NULL,
	processed_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (event_id, subscriber)
);
CREATE OR REPLACE FUNCTION messaging.notify_outbox() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('messaging_outbox', NEW.topic);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER outbox_notify
	AFTER INSERT ON messaging.outbox
	FOR EACH ROW EXECUTE FUNCTION messaging.notify_outbox();"#;

/// The shared durable-plane state, held behind ONE `Arc` so the [`Transport`] impl,
/// the `POST /events` sink, and the relay's local targets all see the same
/// subscription table: a `subscribe_tx` registration is visible to every delivery
/// path. (Go folds this into `*Module`; Rust splits it out so a single `Arc<Inner>`
/// can be handed to `set_transport`, the axum handler, and the relay closures.)
pub struct Inner {
    pool: PgPool,
    /// From `MESSAGING_ORIGIN`, default `"monolith"`. Stamped on every enqueued row.
    origin: String,
    /// topic -> in-process durable subscriptions `(subscriber, handler)`.
    ///
    /// MUST be allocated in phase-1 `register`, NEVER `init`: a consumer registered
    /// before messaging calls `subscribe_tx` during its phase-2 `init` (which runs
    /// BEFORE messaging's `init`, since messaging is registered last). An empty map is
    /// live from `register`, so that append never touches an absent map.
    local_handlers: Mutex<TopicHandlers>,
}

impl Service for Inner {}

#[async_trait::async_trait]
impl Transport for Inner {
    /// Writes one outbox row inside the PRODUCER's domain tx (`conn` is `&mut *tx`),
    /// so the event is durable iff the domain change commits. Stamps `self.origin`
    /// (the producer never sets it) and does NOT commit — the caller owns the tx.
    async fn enqueue_tx(
        &self,
        conn: &mut sqlx::PgConnection,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), bus::Error> {
        // Bind the payload as text so `::jsonb` parses it (a bytea bind would try to
        // cast raw bytes). The bus already JSON-encoded it, so it is valid UTF-8.
        let text = std::str::from_utf8(payload).map_err(bus::Error::transport)?;
        sqlx::query("INSERT INTO messaging.outbox (origin, topic, payload) VALUES ($1, $2, $3::jsonb)")
            .bind(&self.origin)
            .bind(topic)
            .bind(text)
            .execute(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
        Ok(())
    }

    /// Records an in-process durable subscription. Called from a consumer's `init`
    /// (phase 2, before messaging's `init` builds the relay), so it only appends;
    /// messaging's `init` later snapshots these into relay local targets.
    fn subscribe_tx(&self, topic: &str, subscriber: &str, handler: Arc<dyn TxHandler>) {
        self.local_handlers
            .lock()
            .unwrap()
            .entry(topic.to_string())
            .or_default()
            .push((subscriber.to_string(), handler));
    }
}

impl Inner {
    /// Runs one subscriber's handler exactly once for `event_id`. In ONE tx it claims
    /// the event in the inbox keyed `(event_id, subscriber)` (`ON CONFLICT DO
    /// NOTHING`); a first delivery (1 row) runs the handler within the SAME tx before
    /// commit, a duplicate (0 rows) is a committed no-op. Any handler error rolls back
    /// (the tx drops) and propagates → the row stays unsent (local) / a 500 is
    /// returned (inbound) → redelivered next tick. Each subscriber gets its OWN tx and
    /// its OWN inbox row, so a failing subscriber can never roll back another's effect.
    async fn consume(
        &self,
        subscriber: &str,
        event_id: &str,
        payload: &[u8],
        handler: Arc<dyn TxHandler>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            "INSERT INTO messaging.inbox (event_id, subscriber) VALUES ($1, $2) ON CONFLICT DO NOTHING",
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
        // commit or roll back together.
        handler
            .call(&mut tx, payload.to_vec())
            .await
            .map_err(|e| anyhow::anyhow!("durable handler failed: {e}"))?;
        tx.commit().await?;
        Ok(())
    }

    /// Snapshot of the subscribers for `topic` (for the inbound sink).
    fn subscribers_for(&self, topic: &str) -> Vec<Subscription> {
        self.local_handlers
            .lock()
            .unwrap()
            .get(topic)
            .cloned()
            .unwrap_or_default()
    }

    /// Snapshots `local_handlers` into one relay [`LocalTarget`] per (topic, subscriber).
    /// The relay delivers EVERY drained row to EVERY local target (it is not
    /// topic-scoped), so each target filters by topic: a row of a different topic is a
    /// no-op success, and only a matching row runs `consume`. Per-target =
    /// per-subscriber isolation.
    fn build_local_targets(self: &Arc<Self>) -> Vec<LocalTarget> {
        let handlers = self.local_handlers.lock().unwrap();
        let mut targets = Vec::new();
        for (topic, subs) in handlers.iter() {
            for (subscriber, handler) in subs {
                let inner = self.clone();
                let want_topic = topic.clone();
                let subscriber = subscriber.clone();
                let handler = handler.clone();
                targets.push(LocalTarget {
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

/// Runtime config resolved in `init`, consumed in `start`.
struct StartCfg {
    dsn: String,
    retention: Duration,
    house_tick: Duration,
}

/// The durable-plane module. Owns schema `messaging` and installs the transport.
pub struct Messaging {
    /// The shared state, built in phase-1 `register` (needs the pool + origin).
    inner: OnceLock<Arc<Inner>>,
    /// The relay, constructed in `init`, started in `start`.
    relay: Mutex<Option<Arc<Relay>>>,
    /// Config resolved in `init`.
    cfg: Mutex<Option<StartCfg>>,
    /// Cancellation for the relay/listen/housekeep loops; flipped by `stop`.
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    /// Every background task, awaited on `stop`.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for Messaging {
    fn default() -> Self {
        Messaging::new()
    }
}

impl Messaging {
    pub fn new() -> Messaging {
        Messaging {
            inner: OnceLock::new(),
            relay: Mutex::new(None),
            cfg: Mutex::new(None),
            stop_tx: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn inner(&self) -> Arc<Inner> {
        self.inner
            .get()
            .expect("messaging.register must run before init/start")
            .clone()
    }
}

#[async_trait::async_trait]
impl Module for Messaging {
    fn name(&self) -> &str {
        "messaging"
    }

    fn requires(&self) -> Vec<String> {
        Vec::new() // foundation-like: depends on nobody
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE | Caps::START | Caps::STOP
    }

    /// Phase 1, BEFORE any `init`. It (a) allocates `local_handlers` so a consumer's
    /// phase-2 `subscribe_tx` cannot touch an absent map, (b) installs the transport
    /// so every consumer's `on_tx` sees a LIVE durable plane (BLOCKER-2), and (c)
    /// provides the `"messaging"` registry marker so `validate_requires` can enforce
    /// `requires("messaging")`. All three must precede any `init` — hence phase 1.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("messaging requires a DB pool"))?
            .clone();
        let origin = env_or("MESSAGING_ORIGIN", DEFAULT_ORIGIN);
        let inner = Arc::new(Inner {
            pool,
            origin,
            local_handlers: Mutex::new(HashMap::new()),
        });
        self.inner
            .set(inner.clone())
            .map_err(|_| anyhow::anyhow!("messaging.register ran twice"))?;

        // (b) BLOCKER-2: a live transport before any consumer's phase-2 on_tx.
        ctx.bus().set_transport(inner.clone() as Arc<dyn Transport>);
        // (c) the registry marker for validate_requires.
        ctx.registry()
            .provide::<dyn Service>("messaging", inner as Arc<dyn Service>);
        Ok(())
    }

    /// Creates schema `messaging`. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("messaging requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Resolves config, snapshots the local
    /// subscriptions into relay targets, constructs the single relay, and mounts the
    /// one inbound sink. The relay does NOT start here (that's `start`).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let inner = self.inner();

        *self.cfg.lock().unwrap() = Some(StartCfg {
            dsn: env_or("DATABASE_URL", DEFAULT_DSN),
            retention: env_duration("MESSAGING_RETENTION", Duration::from_secs(168 * 3600)),
            house_tick: env_duration("MESSAGING_HOUSEKEEP_INTERVAL", Duration::from_secs(3600)),
        });

        let subs = outbox::parse_subscribers(&std::env::var("EVENTS_SUBSCRIBERS").unwrap_or_default());
        let local_targets = inner.build_local_targets();
        let relay = Arc::new(Relay::new(
            inner.pool.clone(),
            "messaging",
            inner.origin.clone(),
            subs,
            local_targets,
        ));
        *self.relay.lock().unwrap() = Some(relay);

        // One inbound sink for the whole durable plane. A peer relay POSTs a foreign
        // event here (topic in X-Event-Topic, id in X-Event-Id); the handler dedups
        // per subscriber and runs each local subscriber's effect in its own tx.
        let sink = inner.clone();
        let router = Router::new().route(
            "/events",
            post(move |headers: HeaderMap, body: Bytes| handle_inbound(sink.clone(), headers, body)),
        );
        ctx.mount(router);
        Ok(())
    }

    /// Launches the relay, the LISTEN loop, and the housekeeping ticker. Roots each on
    /// a shared `watch` cancel so a short start deadline can't kill them; `stop` flips
    /// the watch and awaits every task.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let inner = self.inner();
        let relay = self
            .relay
            .lock()
            .unwrap()
            .clone()
            .expect("messaging.init must run before start");
        let cfg = self
            .cfg
            .lock()
            .unwrap()
            .take()
            .expect("messaging.init must run before start");

        let (stop_tx, stop_rx) = watch::channel(false);

        let tasks = vec![
            relay.clone().spawn(stop_rx.clone()),
            tokio::spawn(listen(cfg.dsn.clone(), relay.clone(), stop_rx.clone())),
            tokio::spawn(housekeep(
                inner.pool.clone(),
                cfg.retention,
                cfg.house_tick,
                stop_rx,
            )),
        ];

        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        *self.tasks.lock().unwrap() = tasks;
        Ok(())
    }

    /// Halts delivery first (messaging is registered last, so reverse-order stop runs
    /// it before any consumer tears down), then awaits the background loops. Awaiting
    /// the relay task covers any in-flight local `consume` running inside a drain.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(true);
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for t in tasks {
            let _ = t.await;
        }
        Ok(())
    }
}

/// The receiver side: a peer's relay POSTs a foreign event here. Delivers to EVERY
/// local subscriber of the topic, each via its own `consume` tx. If ANY subscriber
/// fails it replies 500 so the sender retries the whole event; the per-subscriber
/// inbox makes already-succeeded subscribers a no-op on that retry.
async fn handle_inbound(inner: Arc<Inner>, headers: HeaderMap, body: Bytes) -> Response {
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

/// Keeps a dedicated `PgListener` on `messaging_outbox` and kicks the relay on every
/// NOTIFY so a freshly-written row drains promptly. Never dies on a DB outage: each
/// (re)connect backs off on failure. NOTIFY is best-effort — a dropped notification
/// only delays a row until the relay's ticker floor.
async fn listen(dsn: String, relay: Arc<Relay>, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        match sqlx::postgres::PgListener::connect(&dsn).await {
            Ok(mut listener) => match listener.listen(NOTIFY_CHANNEL).await {
                Ok(()) => {
                    // A row may have been written between relay start and this LISTEN;
                    // kick once so it isn't stranded until the first tick.
                    relay.kick();
                    loop {
                        tokio::select! {
                            _ = stop.changed() => return,
                            res = listener.recv() => match res {
                                Ok(_) => relay.kick(),
                                Err(err) => {
                                    tracing::error!(%err, "messaging listener wait failed");
                                    break; // reconnect via the outer loop
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "messaging listener LISTEN failed");
                }
            },
            Err(err) => {
                tracing::error!(%err, "messaging listener connect failed");
            }
        }
        // Backoff, cancellable.
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

/// Prunes the ledgers past the retention window on a ticker: sent outbox rows and
/// processed inbox rows older than `now() - retention`. Both DELETEs are batch-bounded
/// (`ctid IN (… LIMIT n)`) so a prune never takes a long lock; the window rides as a
/// bound double (`make_interval`), never string-interpolated. Self-owned.
async fn housekeep(
    pool: PgPool,
    retention: Duration,
    tick: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick — first prune is one interval in
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => prune_once(&pool, retention).await,
        }
    }
}

async fn prune_once(pool: &PgPool, retention: Duration) {
    let secs = retention.as_secs_f64();
    if let Err(err) = sqlx::query(
        "DELETE FROM messaging.inbox WHERE ctid IN (\
         SELECT ctid FROM messaging.inbox WHERE processed_at < now() - make_interval(secs => $1) LIMIT $2)",
    )
    .bind(secs)
    .bind(HOUSEKEEP_BATCH)
    .execute(pool)
    .await
    {
        tracing::error!(%err, "messaging inbox prune failed");
    }
    if let Err(err) = sqlx::query(
        "DELETE FROM messaging.outbox WHERE ctid IN (\
         SELECT ctid FROM messaging.outbox WHERE sent_at IS NOT NULL AND sent_at < now() - make_interval(secs => $1) LIMIT $2)",
    )
    .bind(secs)
    .bind(HOUSEKEEP_BATCH)
    .execute(pool)
    .await
    {
        tracing::error!(%err, "messaging outbox prune failed");
    }
}

fn env_or(key: &str, def: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => def.to_string(),
    }
}

/// Reads `key` as a Go-style duration (`168h`, `30m`, `500ms`, `10s`), falling back to
/// `def` when unset or unparseable.
fn env_duration(key: &str, def: Duration) -> Duration {
    match std::env::var(key) {
        Ok(v) => parse_go_duration(&v).unwrap_or(def),
        Err(_) => def,
    }
}

/// Minimal single-unit Go-duration parser (`h`/`m`/`s`/`ms`). Enough for the sketch's
/// retention/interval knobs; unknown/compound forms fall back to the default.
fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        return n.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(n) = s.strip_suffix('h') {
        return n.trim().parse::<u64>().ok().map(|h| Duration::from_secs(h * 3600));
    }
    if let Some(n) = s.strip_suffix('m') {
        return n.trim().parse::<u64>().ok().map(|m| Duration::from_secs(m * 60));
    }
    if let Some(n) = s.strip_suffix('s') {
        return n.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    None
}

// ============================================================================
// Integration tests — live Postgres (the local DB is the test DB). Each guarded by
// `test_pool`, which SKIPs (early-returns with a message) when Postgres is down so
// `cargo test` never hard-fails on a machine without it. In-crate (not `tests/`) so
// they can drive the private `Inner`/`consume` and the pre-`register` state directly.
// ============================================================================
#[cfg(test)]
mod tests;

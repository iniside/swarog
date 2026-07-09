//! `asyncevents` — the durable async-events plane. It owns schema `asyncevents`
//! (a shared outbox log + a per-subscriber inbox dedup ledger) and implements
//! [`bus::Transport`]. It is NOT a `lifecycle::Module`: the plane is process
//! infrastructure, like the HTTP listener — `core/app::run` constructs a
//! [`Plane`] iff the process has a DB, injects its transport at `Context`
//! construction (`Bus::with_transport`), migrates its schema before module
//! migrations, starts its relay after module starts, and halts delivery before
//! any module stops. Modules declare nothing: DB ⇒ plane.
//!
//! A producer reaches it purely via `bus.emit_tx` (writes one `asyncevents.outbox`
//! row in the producer's own domain tx); a consumer via `bus.on_tx`/`on_tx_raw` (a
//! durable handler run inside a per-subscriber inbox-dedup tx). Neither ever sees the
//! outbox, the inbox, the relay, `EVENTS_SUBSCRIBERS`, or `EVENTS_ORIGIN` — the
//! plane owns the whole envelope. Delivery is topology-transparent: the SAME code
//! path serves the monolith (in-process local targets) and a split (HTTP `POST
//! /events` to a peer), chosen by durability intent, never by topology.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use bus::{Transport, TxHandler};
use outbox::Relay;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// The stable identity a monolith stamps on its outbox rows when `EVENTS_ORIGIN`
/// is unset. It must be stable across restarts so a crashed process resumes draining
/// its OWN unsent rows — never a pid/hostname.
const DEFAULT_ORIGIN: &str = "monolith";

/// The LISTEN/NOTIFY channel the outbox insert trigger fires on.
const NOTIFY_CHANNEL: &str = "asyncevents_outbox";

/// Bounds each retention DELETE so a prune never takes a long lock.
const HOUSEKEEP_BATCH: i64 = 1000;

/// One in-process durable subscription: a stable dedup name + its bytes-level handler.
type Subscription = (String, Arc<dyn TxHandler>);
/// topic -> its durable subscriptions.
type TopicHandlers = HashMap<String, Vec<Subscription>>;

/// Creates this plane's OWN schema — full logical isolation (#10). Idempotent
/// (`IF NOT EXISTS` / `OR REPLACE`). The leading `DO` block migrates a pre-rename
/// dev DB in place: `messaging` → `asyncevents` runs exactly once (guarded on both
/// schema names), and — atomically with the rename — rewrites the inbox dedup-key
/// prefix, because the relay derives `event_id` as `"{schema}:{row.id}"`
/// (`core/outbox`): a renamed-in-place row with partial delivery would otherwise
/// re-deliver under the new prefix and re-run already-succeeded handlers.
/// The `AFTER INSERT` trigger fires the `pg_notify` the relay's LISTEN loop wakes
/// on; the partial index keeps the unsent scan cheap; the inbox PK
/// `(event_id, subscriber)` is what makes dedup PER SUBSCRIBER so a failing
/// subscriber never blocks another's delivery.
const SCHEMA_DDL: &str = r#"
DO $$ BEGIN
	IF EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'messaging')
	   AND NOT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'asyncevents')
	THEN
		ALTER SCHEMA messaging RENAME TO asyncevents;
		UPDATE asyncevents.inbox
		   SET event_id = 'asyncevents:' || substr(event_id, length('messaging:') + 1)
		 WHERE event_id LIKE 'messaging:%';
	END IF;
END $$;
CREATE SCHEMA IF NOT EXISTS asyncevents;
CREATE TABLE IF NOT EXISTS asyncevents.outbox (
	id         bigserial   PRIMARY KEY,
	origin     text        NOT NULL,
	topic      text        NOT NULL,
	payload    jsonb       NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	sent_at    timestamptz
);
CREATE INDEX IF NOT EXISTS outbox_unsent_idx ON asyncevents.outbox (id) WHERE sent_at IS NULL;
CREATE TABLE IF NOT EXISTS asyncevents.inbox (
	event_id     text        NOT NULL,
	subscriber   text        NOT NULL,
	processed_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (event_id, subscriber)
);
CREATE OR REPLACE FUNCTION asyncevents.notify_outbox() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('asyncevents_outbox', NEW.topic);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER outbox_notify
	AFTER INSERT ON asyncevents.outbox
	FOR EACH ROW EXECUTE FUNCTION asyncevents.notify_outbox();"#;

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
    /// Live from [`Plane::new`] — BEFORE the `Context` (and thus any module wiring)
    /// exists — so every `on_tx`, whether from a module's `init` or a stub factory's
    /// `register`, appends to a present map. [`Plane::start`] snapshots it after all
    /// wiring is done.
    local_handlers: Mutex<TopicHandlers>,
}

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
        sqlx::query("INSERT INTO asyncevents.outbox (origin, topic, payload) VALUES ($1, $2, $3::jsonb)")
            .bind(&self.origin)
            .bind(topic)
            .bind(text)
            .execute(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
        Ok(())
    }

    /// Records an in-process durable subscription. Called during module wiring
    /// (any phase — the map is live from [`Plane::new`]), so it only appends;
    /// [`Plane::start`] later snapshots these into relay local targets.
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

    /// Snapshots `local_handlers` into one relay [`outbox::LocalTarget`] per
    /// (topic, subscriber). The relay delivers EVERY drained row to EVERY local
    /// target (it is not topic-scoped), so each target filters by topic: a row of a
    /// different topic is a no-op success, and only a matching row runs `consume`.
    /// Per-target = per-subscriber isolation.
    fn build_local_targets(self: &Arc<Self>) -> Vec<outbox::LocalTarget> {
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

/// Runtime knobs resolved from env in [`Plane::new`], consumed in [`Plane::start`].
struct StartCfg {
    retention: Duration,
    house_tick: Duration,
}

/// The durable async-events plane of ONE process. Owned and driven by `core/app::run`
/// (never by a module or a `cmd/*` main): constructed when the process has a DB,
/// [`Plane::transport`] injected at `Context` construction, [`Plane::router`] merged
/// into the process router, [`Plane::migrate`] before module migrations,
/// [`Plane::start`] after module starts (the local-target snapshot must see every
/// wiring-time `on_tx`), [`Plane::stop`] before any module stops (delivery halts
/// first, so a stopping module never receives).
pub struct Plane {
    inner: Arc<Inner>,
    pool: PgPool,
    /// The DSN for the dedicated LISTEN connection — passed in by app (its
    /// authoritative `cfg.database_url`), never re-read from env here: the plane
    /// must LISTEN on the same DB the pool writes to.
    listen_dsn: String,
    cfg: StartCfg,
    /// topic → remote sink URLs, from `EVENTS_SUBSCRIBERS` (unchanged name).
    subscribers: HashMap<String, Vec<String>>,
    /// Cancellation + background tasks, present between `start` and `stop`.
    stop: Option<(watch::Sender<bool>, Vec<JoinHandle<()>>)>,
}

impl Plane {
    /// Reads `EVENTS_ORIGIN` (default `"monolith"`), `EVENTS_SUBSCRIBERS`,
    /// `EVENTS_RETENTION` (default `168h`), `EVENTS_HOUSEKEEP_INTERVAL` (default
    /// `1h`). No I/O — construction is wiring-safe; the first DB touch is
    /// [`Plane::migrate`].
    pub fn new(pool: PgPool, listen_dsn: String) -> anyhow::Result<Plane> {
        let origin = env_or("EVENTS_ORIGIN", DEFAULT_ORIGIN);
        let inner = Arc::new(Inner {
            pool: pool.clone(),
            origin,
            local_handlers: Mutex::new(HashMap::new()),
        });
        let subscribers =
            outbox::parse_subscribers(&std::env::var("EVENTS_SUBSCRIBERS").unwrap_or_default());
        Ok(Plane {
            inner,
            pool,
            listen_dsn,
            cfg: StartCfg {
                retention: env_duration("EVENTS_RETENTION", Duration::from_secs(168 * 3600)),
                house_tick: env_duration("EVENTS_HOUSEKEEP_INTERVAL", Duration::from_secs(3600)),
            },
            subscribers,
            stop: None,
        })
    }

    /// The [`bus::Transport`] to inject at `Context` construction
    /// (`Bus::with_transport`) — live from birth, so any wiring-time `on_tx`
    /// (module `init` or stub-factory `register`) records rather than panics.
    pub fn transport(&self) -> Arc<dyn Transport> {
        self.inner.clone()
    }

    /// The one inbound sink for the whole durable plane, merged into the process
    /// router by app. A peer relay POSTs a foreign event here (topic in
    /// X-Event-Topic, id in X-Event-Id); the handler dedups per subscriber and runs
    /// each local subscriber's effect in its own tx.
    pub fn router(&self) -> Router {
        let sink = self.inner.clone();
        Router::new().route(
            "/events",
            post(move |headers: HeaderMap, body: Bytes| handle_inbound(sink.clone(), headers, body)),
        )
    }

    /// Creates schema `asyncevents` (migrating a pre-rename `messaging` schema in
    /// place — see [`SCHEMA_DDL`]). Idempotent. Runs BEFORE module migrations so a
    /// module's first `emit_tx` always finds the outbox.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::raw_sql(SCHEMA_DDL).execute(&self.pool).await?;
        Ok(())
    }

    /// Launches delivery: origin-collision guard, local-target snapshot (all module
    /// inits AND stub registers have run — app calls this after `App::start`), then
    /// the relay, the LISTEN loop, and the housekeeping ticker. Roots each on a
    /// shared `watch` cancel; [`Plane::stop`] flips it and awaits every task.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Origin-collision guard: a process that names remote sinks
        // (EVENTS_SUBSCRIBERS) is, by definition, one side of a split — but the relay
        // drains ONLY its own `origin`'s outbox rows, so a split process left on the
        // default `"monolith"` origin would share that origin with any OTHER default
        // process on the same DB and mis-drain (or double-drain) its rows. Fail loud at
        // start rather than silently swallow another process's events.
        if origin_collision(&self.inner.origin, &self.subscribers) {
            anyhow::bail!(
                "asyncevents: EVENTS_ORIGIN is unset/\"{DEFAULT_ORIGIN}\" but EVENTS_SUBSCRIBERS \
                 names {} remote sink topic(s) — a shared-DB origin collision would mis-drain \
                 another process's outbox rows; set a distinct EVENTS_ORIGIN per split process",
                self.subscribers.len(),
            );
        }

        let local_targets = self.inner.build_local_targets();
        let relay = Arc::new(Relay::new(
            self.pool.clone(),
            "asyncevents",
            self.inner.origin.clone(),
            self.subscribers.clone(),
            local_targets,
        ));

        let (stop_tx, stop_rx) = watch::channel(false);
        let tasks = vec![
            relay.clone().spawn(stop_rx.clone()),
            tokio::spawn(listen(self.listen_dsn.clone(), relay, stop_rx.clone())),
            tokio::spawn(housekeep(
                self.pool.clone(),
                self.cfg.retention,
                self.cfg.house_tick,
                stop_rx,
            )),
        ];
        self.stop = Some((stop_tx, tasks));
        Ok(())
    }

    /// Halts delivery FIRST (app calls this before `Bus::close`/`App::stop`, so no
    /// module receives while tearing down), then awaits the background loops.
    /// Awaiting the relay task covers any in-flight local `consume` running inside
    /// a drain. Idempotent — a never-started plane is a no-op.
    pub async fn stop(&mut self) {
        if let Some((stop_tx, tasks)) = self.stop.take() {
            let _ = stop_tx.send(true);
            for t in tasks {
                let _ = t.await;
            }
        }
    }
}

/// Test helper: a bare durable transport over `(pool, origin)`, with no relay,
/// LISTEN, or housekeeping behind it. Module integration tests pass this to
/// `Context::with_db_and_transport` to get real outbox writes + `on_tx` recording
/// without booting a whole [`Plane`].
pub fn transport(pool: PgPool, origin: &str) -> Arc<dyn Transport> {
    Arc::new(Inner {
        pool,
        origin: origin.to_string(),
        local_handlers: Mutex::new(HashMap::new()),
    })
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

/// Keeps a dedicated `PgListener` on `asyncevents_outbox` and kicks the relay on every
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
                                    tracing::error!(%err, "asyncevents listener wait failed");
                                    break; // reconnect via the outer loop
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "asyncevents listener LISTEN failed");
                }
            },
            Err(err) => {
                tracing::error!(%err, "asyncevents listener connect failed");
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
        "DELETE FROM asyncevents.inbox WHERE ctid IN (\
         SELECT ctid FROM asyncevents.inbox WHERE processed_at < now() - make_interval(secs => $1) LIMIT $2)",
    )
    .bind(secs)
    .bind(HOUSEKEEP_BATCH)
    .execute(pool)
    .await
    {
        tracing::error!(%err, "asyncevents inbox prune failed");
    }
    if let Err(err) = sqlx::query(
        "DELETE FROM asyncevents.outbox WHERE ctid IN (\
         SELECT ctid FROM asyncevents.outbox WHERE sent_at IS NOT NULL AND sent_at < now() - make_interval(secs => $1) LIMIT $2)",
    )
    .bind(secs)
    .bind(HOUSEKEEP_BATCH)
    .execute(pool)
    .await
    {
        tracing::error!(%err, "asyncevents outbox prune failed");
    }
}

fn env_or(key: &str, def: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => def.to_string(),
    }
}

/// True when this process is a split participant (it names ≥1 remote HTTP sink) yet is
/// still stamping the DEFAULT shared `"monolith"` origin — the exact condition under
/// which two shared-DB processes collide on origin and one mis-drains the other's
/// outbox rows. Pure so it is unit-testable without a DB.
fn origin_collision(origin: &str, subscribers: &HashMap<String, Vec<String>>) -> bool {
    origin == DEFAULT_ORIGIN && !subscribers.is_empty()
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
// they can drive the private `Inner`/`consume` and the pre-snapshot state directly.
// ============================================================================
#[cfg(test)]
mod tests;

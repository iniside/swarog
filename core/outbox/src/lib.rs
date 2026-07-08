//! `outbox` — a transactional-outbox relay: a generic, domain-agnostic helper that
//! drains the rows a publishing module wrote (in the same DB transaction as its
//! domain change) and delivers them to in-process local targets and to remote
//! subscribers over HTTP. It is a leaf: it imports NO module implementation and
//! works purely off a schema name, an origin string, a topic→URLs config, and
//! function-typed local targets, so the same relay serves any producer. (Port of
//! Go's `outbox/relay.go`.)
//!
//! # Single-owner drain (the split regression the origin fixes)
//! Every process sharing one outbox table stamps its rows with a stable `origin`,
//! and each relay drains ONLY its own origin's rows (`WHERE origin = $1 …
//! FOR UPDATE SKIP LOCKED`). So a foreign process's relay can never mark-sent (and
//! thus silently swallow) a row it does not subscribe to; the producing process
//! alone owns delivery of its events.
//!
//! # Delivery contract
//! - **At-least-once.** A stable event id (`<schema>:<outbox.id>`) rides with each
//!   delivery (`X-Event-Id` header for remote, the `event_id` arg for local) so an
//!   idempotent subscriber (an inbox keyed on that id) dedups retries.
//! - **Local delivery is unconditional** — attempted independently of whether any
//!   remote URLs exist, so the monolith (empty subscribers) still delivers.
//! - **Per-(topic, target) ordering / poison isolation.** Rows are delivered in
//!   ascending id; on the first failure to a given `(topic, url)` or
//!   `(topic, local subscriber)` the relay stops advancing for THAT `(topic,
//!   target)` this batch (a later event of the same topic can't overtake an earlier
//!   one), so a poison event of one topic can't stall a different topic to the same
//!   peer. A row is marked sent only once EVERY local target AND EVERY remote URL
//!   for its topic accepted it. A row with no targets is delivered to nobody =
//!   success, marked sent at once.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use sqlx::{PgPool, Row};
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, MissedTickBehavior};

/// How often the relay drains the outbox on the ticker floor. NOTIFY (via [`Relay::kick`])
/// is a latency optimization ON TOP of this; the ticker is the correctness floor.
const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);

/// The future a [`LocalTarget`]'s `deliver` returns. `'static` + `Send`: the relay
/// loop lives on a spawned tokio task, so a delivery future can't borrow the caller.
pub type DeliverFuture = BoxFuture<'static, anyhow::Result<()>>;

/// An in-process delivery target's effect. Invoked (with the stable `event_id`) for
/// every drained row whose delivery this target owns; it MUST be idempotent (dedup
/// on `event_id`) since delivery is at-least-once. Owned args (`String`/`Vec<u8>`)
/// keep the returned future `'static`.
pub type DeliverFn = Arc<dyn Fn(String, Vec<u8>, String) -> DeliverFuture + Send + Sync>;

/// An in-process delivery target. `subscriber` is a stable name used only to key the
/// per-(topic, subscriber) block gate so one failing local subscriber cannot stall
/// another. The relay stays domain-agnostic: `deliver` is a plain function, no module
/// import. (Go's `outbox.LocalTarget`.)
#[derive(Clone)]
pub struct LocalTarget {
    pub subscriber: String,
    pub deliver: DeliverFn,
}

/// One unsent outbox row.
struct OutRow {
    id: i64,
    topic: String,
    payload: Vec<u8>,
}

/// Drains a schema's outbox table (only rows of its own origin) and delivers each
/// row to every local target and every remote HTTP subscriber. Construct with
/// [`Relay::new`], drive with [`Relay::spawn`] + [`Relay::kick`].
pub struct Relay {
    pool: PgPool,
    schema: String,
    /// This process's stable identity; drains ONLY rows stamped with it.
    origin: String,
    /// topic -> subscriber URLs.
    subscribers: HashMap<String, Vec<String>>,
    /// in-process delivery targets, always attempted.
    local_targets: Vec<LocalTarget>,
    client: reqwest::Client,
    interval: Duration,

    /// capacity-1 wake signal; [`Relay::kick`] coalesces a NOTIFY into an immediate
    /// drain and never blocks. The receiver is taken once by [`Relay::spawn`].
    kick_tx: mpsc::Sender<()>,
    kick_rx: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Relay {
    /// Builds a relay for `schema`'s outbox table, draining only rows stamped with
    /// `origin`. `subscribers` maps each topic to the URLs to POST it to;
    /// `local_targets` are attempted unconditionally (empty both = delivered to
    /// nobody = marked sent at once). **Panics on a non-identifier schema** — a
    /// wiring bug, loud at startup (the schema is interpolated into SQL; there is no
    /// bind parameter for an identifier).
    pub fn new(
        pool: PgPool,
        schema: impl Into<String>,
        origin: impl Into<String>,
        subscribers: HashMap<String, Vec<String>>,
        local_targets: Vec<LocalTarget>,
    ) -> Relay {
        let schema = schema.into();
        assert!(
            valid_ident(&schema),
            "outbox: invalid schema name {schema:?}"
        );
        let (kick_tx, kick_rx) = mpsc::channel(1);
        Relay {
            pool,
            schema,
            origin: origin.into(),
            subscribers,
            local_targets,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("build http client"),
            interval: DEFAULT_INTERVAL,
            kick_tx,
            kick_rx: Mutex::new(Some(kick_rx)),
        }
    }

    /// Requests an immediate drain, coalescing bursts. Never blocks (capacity-1 +
    /// non-blocking send), so a LISTEN loop is never stalled by the drain loop and
    /// NOTIFY stays a pure latency optimization on top of the ticker's floor.
    pub fn kick(&self) {
        let _ = self.kick_tx.try_send(());
    }

    /// Launches the drain loop on a tokio task and returns its handle. The loop
    /// drains once immediately (so a startup backlog isn't stranded for a full
    /// interval), then reacts to the ticker floor and to kicks, until `stop` flips
    /// to `true` (or its sender drops). Await the returned handle to know the loop —
    /// and any in-flight local delivery running inside a drain — has finished.
    pub fn spawn(self: Arc<Self>, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        let mut kick_rx = self
            .kick_rx
            .lock()
            .unwrap()
            .take()
            .expect("relay already spawned");
        tokio::spawn(async move {
            if let Err(err) = self.drain_once().await {
                tracing::error!(schema = %self.schema, %err, "outbox drain failed");
            }
            let mut ticker = interval(self.interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // consume the immediate first tick (already drained above)
            loop {
                tokio::select! {
                    _ = stop.changed() => return,
                    _ = ticker.tick() => {
                        if let Err(err) = self.drain_once().await {
                            tracing::error!(schema = %self.schema, %err, "outbox drain failed");
                        }
                    }
                    _ = kick_rx.recv() => {
                        if let Err(err) = self.drain_once().await {
                            tracing::error!(schema = %self.schema, %err, "outbox drain failed");
                        }
                    }
                }
            }
        })
    }

    /// Reads every unsent row of this relay's origin in id order, delivers them
    /// (per-(topic, target) ordering enforced by [`Relay::deliver`]), and marks the
    /// fully-delivered ones sent — all in ONE transaction.
    ///
    /// **Tx boundary:** the locking `SELECT … FOR UPDATE SKIP LOCKED` and the
    /// `mark_sent` UPDATEs run in the SAME transaction. Postgres releases row locks
    /// only at commit/rollback, so `mark_sent` MUST share the tx that took the locks —
    /// otherwise the `FOR UPDATE` lock would already be gone. Holding the locks across
    /// the batch (including delivery I/O) is deliberate: `SKIP LOCKED` means a
    /// concurrent same-origin drainer skips these rows rather than double-delivering.
    /// A `mark_sent` failure poisons the tx, so we abort and redeliver next tick
    /// (at-least-once; the inbox dedups).
    ///
    /// Public so an integration test can drive one deterministic drain without the
    /// ticker/kick loop.
    pub async fn drain_once(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let pending = self.pending(&mut tx).await?;
        if pending.is_empty() {
            tx.commit().await?; // nothing locked; release cleanly
            return Ok(());
        }
        let sent = self.deliver(&pending).await;
        for id in sent {
            self.mark_sent(&mut tx, id).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Decides which rows are fully delivered. Each row goes to EVERY local target
    /// (unconditional, the monolith path) AND every remote URL for its topic. Enforces
    /// per-(topic, target) ordering: once delivery to a `(topic, target)` fails, no
    /// further row of THAT topic reaches THAT target this batch. A row is returned (to
    /// be marked sent) only when every target accepted it; a row with no targets is
    /// delivered to nobody and counts as sent.
    async fn deliver(&self, pending: &[OutRow]) -> Vec<i64> {
        let mut blocked: HashSet<String> = HashSet::new();
        let mut sent = Vec::new();
        for row in pending {
            let event_id = format!("{}:{}", self.schema, row.id);
            let mut all_ok = true;

            // Local targets first, unconditionally (independent of remote URLs).
            for lt in &self.local_targets {
                let key = format!("L\0{}\0{}", lt.subscriber, row.topic);
                if blocked.contains(&key) {
                    all_ok = false; // can't skip ahead of an earlier undelivered row
                    continue;
                }
                let fut = (lt.deliver)(row.topic.clone(), row.payload.clone(), event_id.clone());
                if let Err(err) = fut.await {
                    tracing::warn!(subscriber = %lt.subscriber, topic = %row.topic, event_id = %event_id, %err, "outbox local delivery failed");
                    blocked.insert(key);
                    all_ok = false;
                }
            }

            // Then remote subscribers for this topic.
            if let Some(urls) = self.subscribers.get(&row.topic) {
                for url in urls {
                    let key = format!("R\0{}\0{}", url, row.topic);
                    if blocked.contains(&key) {
                        all_ok = false;
                        continue;
                    }
                    if let Err(err) = self.post(url, &row.topic, &event_id, &row.payload).await {
                        tracing::warn!(%url, topic = %row.topic, event_id = %event_id, %err, "outbox delivery failed");
                        blocked.insert(key);
                        all_ok = false;
                    }
                }
            }

            if all_ok {
                sent.push(row.id);
            }
        }
        sent
    }

    /// Reads this relay's own unsent rows, locking them `FOR UPDATE SKIP LOCKED`.
    /// The exact drain query — see [`Relay::drain_once`] for the tx boundary.
    async fn pending(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<Vec<OutRow>> {
        // Schema is validated as a bare SQL identifier in `new`, so this interpolation
        // can never carry attacker-controlled input.
        let q = format!(
            "SELECT id, topic, payload FROM {}.outbox WHERE sent_at IS NULL AND origin = $1 ORDER BY id FOR UPDATE SKIP LOCKED",
            self.schema
        );
        let rows = sqlx::query(&q)
            .bind(&self.origin)
            .fetch_all(&mut **tx)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: i64 = r.try_get("id")?;
            let topic: String = r.try_get("topic")?;
            // jsonb -> Value -> canonical compact bytes; the body delivered is exactly
            // the JSON Postgres stored.
            let payload: serde_json::Value = r.try_get("payload")?;
            out.push(OutRow {
                id,
                topic,
                payload: serde_json::to_vec(&payload)?,
            });
        }
        Ok(out)
    }

    /// Marks one row sent. MUST run in the SAME tx as [`Relay::pending`]'s locking
    /// SELECT (see [`Relay::drain_once`]).
    async fn mark_sent(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: i64,
    ) -> anyhow::Result<()> {
        let q = format!("UPDATE {}.outbox SET sent_at = now() WHERE id = $1", self.schema);
        sqlx::query(&q).bind(id).execute(&mut **tx).await?;
        Ok(())
    }

    /// Delivers one row to one subscriber over HTTP. The stable event id rides in
    /// `X-Event-Id` (dedup) and the topic in `X-Event-Topic` (single `POST /events`
    /// routing); the body is the raw event JSON. Any non-2xx (or transport error) is
    /// a failure that stops advancing this `(topic, subscriber)` (retried next tick).
    async fn post(
        &self,
        url: &str,
        topic: &str,
        event_id: &str,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let resp = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Event-Id", event_id)
            .header("X-Event-Topic", topic)
            .body(payload.to_vec())
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("outbox: subscriber {url} returned {}", resp.status().as_u16());
        }
        Ok(())
    }
}

/// Parses the `EVENTS_SUBSCRIBERS` env value into a topic→URLs map.
///
/// Shape: semicolon-separated entries, each `topic=url` (URLs may be comma-separated
/// for multiple subscribers, and a topic may repeat — both append). Whitespace is
/// trimmed; blank entries are skipped. Empty/unset input yields an empty map (the
/// monolith: no remote subscribers).
pub fn parse_subscribers(raw: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((topic, urls)) = entry.split_once('=') else {
            continue;
        };
        let topic = topic.trim();
        if topic.is_empty() {
            continue;
        }
        for u in urls.split(',') {
            let u = u.trim();
            if !u.is_empty() {
                out.entry(topic.to_string()).or_default().push(u.to_string());
            }
        }
    }
    out
}

/// True for a plain SQL identifier (first char `[A-Za-z_]`, rest `[A-Za-z0-9_]`) —
/// the guard on the schema name interpolated into SQL. (Go's `identRe`.)
fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_subscribers_shape() {
        let m = parse_subscribers(
            "character.created=http://b/events,http://c/events; character.deleted = http://b/events ;;bad;=nourl;t2=",
        );
        assert_eq!(
            m.get("character.created").unwrap(),
            &vec!["http://b/events".to_string(), "http://c/events".to_string()]
        );
        assert_eq!(m.get("character.deleted").unwrap(), &vec!["http://b/events".to_string()]);
        // "bad" has no '=', "=nourl" has empty topic, "t2=" has empty url -> none recorded.
        assert!(!m.contains_key("bad"));
        assert!(!m.contains_key("t2"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_subscribers_empty_is_empty_map() {
        assert!(parse_subscribers("").is_empty());
        assert!(parse_subscribers("   ").is_empty());
    }

    #[test]
    fn valid_ident_accepts_and_rejects() {
        assert!(valid_ident("messaging"));
        assert!(valid_ident("_x9"));
        assert!(!valid_ident("9x"));
        assert!(!valid_ident("a.b"));
        assert!(!valid_ident(""));
        assert!(!valid_ident("drop table"));
    }

    #[tokio::test]
    #[should_panic(expected = "invalid schema name")]
    async fn new_panics_on_bad_schema() {
        let pool = PgPool::connect_lazy("postgres://x/y").unwrap();
        let _ = Relay::new(pool, "bad schema", "o", HashMap::new(), Vec::new());
    }
}

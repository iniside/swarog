use super::*;
use futures::future::BoxFuture;
use lifecycle::Context;
use outbox::LocalTarget;
use std::sync::atomic::{AtomicU32, Ordering};

/// Fallback DSN when `DATABASE_URL` is unset — the same default `core/app` uses.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres, migrates the asyncevents schema, and returns `None`
/// (printing a skip line) when it's unreachable — so the suite degrades to a
/// no-op rather than failing where there's no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — asyncevents DB tests skipped");
            return None;
        }
    };
    if let Err(err) = sqlx::raw_sql(SCHEMA_DDL).execute(&pool).await {
        eprintln!("SKIP: asyncevents migrate failed: {err}");
        return None;
    }
    Some(pool)
}

/// A unique suffix so a rerun's rows never collide with a previous run's (the
/// ledger is shared across runs until housekeeping).
fn unique() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{}", std::process::id(), nanos)
}

fn inner(pool: PgPool, origin: &str) -> Arc<Inner> {
    Arc::new(Inner {
        pool,
        origin: origin.to_string(),
        local_handlers: Mutex::new(HashMap::new()),
    })
}

/// A [`TxHandler`] that counts calls and records each payload AND the delivery
/// `event_id` it received — so tests can assert the handler saw the same stable
/// id the plane deduped on (the foreign-store idempotency key).
struct RecordHandler {
    calls: Arc<AtomicU32>,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    ids: Arc<Mutex<Vec<String>>>,
}
impl TxHandler for RecordHandler {
    fn call<'a>(
        &'a self,
        delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), bus::Error>> {
        let calls = self.calls.clone();
        let seen = self.seen.clone();
        let ids = self.ids.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(payload);
            ids.lock().unwrap().push(delivery.event_id.to_string());
            Ok(())
        })
    }
}

/// BLOCKER-2 without a DB: the transport is live from `Plane::new` and injected at
/// `Context` construction, so any wiring-time `on_tx` records rather than panics —
/// the exact shape `app::run` builds for a DB-backed process.
#[tokio::test]
async fn plane_transport_is_live_at_context_construction() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap(); // never touched (no query)
    let plane = Plane::new(pool.clone(), DEFAULT_DSN.to_string()).unwrap();
    let ctx = Context::with_db_and_transport(pool, plane.transport());

    // A module's init (or a stub factory's register) calls bus.on_tx ->
    // Transport::subscribe_tx. This runs long before Plane::start's snapshot; it
    // must not panic and must land in the plane's subscription table.
    let et = bus::define::<serde_json::Value>("test.topic");
    ctx.bus().on_tx(&et, "consumer", |delivery, v: serde_json::Value| {
        Box::pin(async move {
            let _ = (delivery, v);
            Ok(())
        })
    });

    assert_eq!(plane.inner.subscribers_for("test.topic").len(), 1);
}

/// `enqueue_tx` writes a row on the caller's tx with the plane's origin.
#[tokio::test]
async fn enqueue_tx_writes_row_with_origin() {
    let Some(pool) = test_pool().await else { return };
    let origin = format!("test-enq-{}", unique());
    let inner = inner(pool.clone(), &origin);

    let mut tx = pool.begin().await.unwrap();
    inner
        .enqueue_tx(AnyTx::new(&mut *tx), "test.enqueue", br#"{"a":1}"#)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let row = sqlx::query("SELECT origin, topic FROM asyncevents.outbox WHERE origin = $1")
        .bind(&origin)
        .fetch_one(&pool)
        .await
        .unwrap();
    use sqlx::Row;
    assert_eq!(row.get::<String, _>("origin"), origin);
    assert_eq!(row.get::<String, _>("topic"), "test.enqueue");

    cleanup(&pool, &origin).await;
}

/// The split regression: a relay drains ONLY its own origin's rows, never a peer's.
#[tokio::test]
async fn relay_drains_only_its_own_origin() {
    let Some(pool) = test_pool().await else { return };
    let origin_a = format!("test-A-{}", unique());
    let origin_b = format!("test-B-{}", unique());
    let inner_a = inner(pool.clone(), &origin_a);
    let inner_b = inner(pool.clone(), &origin_b);

    // One row from each origin, both committed.
    let mut tx = pool.begin().await.unwrap();
    inner_a.enqueue_tx(AnyTx::new(&mut *tx), "t.a", br#"{"n":1}"#).await.unwrap();
    inner_b.enqueue_tx(AnyTx::new(&mut *tx), "t.b", br#"{"n":2}"#).await.unwrap();
    tx.commit().await.unwrap();

    // Relay A with a local target recording delivered event ids.
    let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
    let rec = delivered.clone();
    let target = LocalTarget {
        subscriber: "rec".into(),
        deliver: Arc::new(move |_topic, _payload, event_id| {
            let rec = rec.clone();
            Box::pin(async move {
                rec.lock().unwrap().push(event_id);
                Ok(())
            })
        }),
    };
    let relay = Relay::new(pool.clone(), "asyncevents", origin_a.clone(), HashMap::new(), vec![target]);
    relay.drain_once().await.unwrap();

    // A's row delivered + marked sent; B's row untouched (still unsent).
    assert_eq!(delivered.lock().unwrap().len(), 1, "relay delivered a foreign origin's row");
    assert_eq!(unsent_count(&pool, &origin_a).await, 0, "A's row not marked sent");
    assert_eq!(unsent_count(&pool, &origin_b).await, 1, "B's row was drained by A's relay");

    cleanup(&pool, &origin_a).await;
    cleanup(&pool, &origin_b).await;
}

/// Inbox dedup: the same `(event_id, subscriber)` consumed twice runs the handler
/// exactly once.
#[tokio::test]
async fn inbox_dedup_runs_handler_once() {
    let Some(pool) = test_pool().await else { return };
    let inner = inner(pool.clone(), "dedup");
    let event_id = format!("asyncevents:test:{}", unique());
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ids = Arc::new(Mutex::new(Vec::new()));

    for _ in 0..2 {
        let h: Arc<dyn TxHandler> = Arc::new(RecordHandler {
            calls: calls.clone(),
            seen: seen.clone(),
            ids: ids.clone(),
        });
        inner.consume("sub-a", &event_id, b"{}", h).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler ran more than once — inbox dedup broken");
    // The one run saw the exact id the plane deduped on — the stable key a
    // foreign-store consumer would use for its own idempotent effect.
    assert_eq!(*ids.lock().unwrap(), vec![event_id.clone()]);

    cleanup_inbox(&pool, &event_id).await;
}

/// Full local round-trip: enqueue → relay drain → local target → consume → the
/// handler sees the exact payload, and the inbox row is present.
#[tokio::test]
async fn local_target_round_trip() {
    let Some(pool) = test_pool().await else { return };
    let origin = format!("test-rt-{}", unique());
    let inner = inner(pool.clone(), &origin);

    // Producer enqueues one durable event.
    let mut tx = pool.begin().await.unwrap();
    inner
        .enqueue_tx(AnyTx::new(&mut *tx), "rt.topic", br#"{"item":"starter-sword","qty":1}"#)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // A subscription that records the payload + delivery id, wired through the
    // real build path.
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ids = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(RecordHandler {
        calls: calls.clone(),
        seen: seen.clone(),
        ids: ids.clone(),
    });
    inner.subscribe_tx("rt.topic", "rt-sub", h);
    let targets = inner.build_local_targets();

    let relay = Relay::new(pool.clone(), "asyncevents", origin.clone(), HashMap::new(), targets);
    relay.drain_once().await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler not run exactly once");
    let payload = seen.lock().unwrap()[0].clone();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v["item"], "starter-sword");
    assert_eq!(v["qty"], 1);
    // The handler was handed the relay-minted id ("{schema}:{outbox_id}") — the
    // same id consume() deduped on, now proven by the inbox row keyed by it.
    let event_id = ids.lock().unwrap()[0].clone();
    assert!(event_id.starts_with("asyncevents:"), "unexpected event_id shape: {event_id}");
    assert_eq!(inbox_count(&pool, &event_id, "rt-sub").await, 1, "inbox row not keyed by the delivered event_id");
    assert_eq!(unsent_count(&pool, &origin).await, 0, "row not marked sent after full delivery");
    cleanup_inbox(&pool, &event_id).await;

    cleanup(&pool, &origin).await;
}

async fn unsent_count(pool: &PgPool, origin: &str) -> i64 {
    use sqlx::Row;
    sqlx::query("SELECT count(*) AS n FROM asyncevents.outbox WHERE origin = $1 AND sent_at IS NULL")
        .bind(origin)
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>("n")
}

async fn inbox_count(pool: &PgPool, event_id: &str, subscriber: &str) -> i64 {
    use sqlx::Row;
    sqlx::query("SELECT count(*) AS n FROM asyncevents.inbox WHERE event_id = $1 AND subscriber = $2")
        .bind(event_id)
        .bind(subscriber)
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>("n")
}

async fn cleanup(pool: &PgPool, origin: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.outbox WHERE origin = $1")
        .bind(origin)
        .execute(pool)
        .await;
}

async fn cleanup_inbox(pool: &PgPool, event_id: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.inbox WHERE event_id = $1")
        .bind(event_id)
        .execute(pool)
        .await;
}

#[test]
fn parse_go_duration_units() {
    assert_eq!(parse_go_duration("168h"), Some(Duration::from_secs(168 * 3600)));
    assert_eq!(parse_go_duration("30m"), Some(Duration::from_secs(1800)));
    assert_eq!(parse_go_duration("10s"), Some(Duration::from_secs(10)));
    assert_eq!(parse_go_duration("500ms"), Some(Duration::from_millis(500)));
    assert_eq!(parse_go_duration("nonsense"), None);
}

/// The origin-collision guard: only the DEFAULT origin WITH remote sinks is a
/// collision. A distinct origin (a real split process) or no subscribers (a monolith)
/// is fine. No DB needed — the predicate is pure.
#[test]
fn origin_collision_only_default_with_remote_sinks() {
    let none = outbox::parse_subscribers("");
    let one = outbox::parse_subscribers("config.changed=http://localhost:8081/events");

    // Monolith: default origin, no remote sinks -> OK.
    assert!(!origin_collision(DEFAULT_ORIGIN, &none));
    // The bug: default origin AND remote sinks -> collision.
    assert!(origin_collision(DEFAULT_ORIGIN, &one));
    // A real split process names a distinct origin -> OK even with remote sinks.
    assert!(!origin_collision("config-svc", &one));
    // A distinct origin with no sinks (a leaf producer) -> OK.
    assert!(!origin_collision("config-svc", &none));
}

use super::*;
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicU32, Ordering};

/// Opens the local Postgres, migrates the messaging schema, and returns `None`
/// (printing a skip line) when it's unreachable — so the suite degrades to a
/// no-op rather than failing where there's no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — messaging DB tests skipped");
            return None;
        }
    };
    if let Err(err) = sqlx::raw_sql(SCHEMA_DDL).execute(&pool).await {
        eprintln!("SKIP: messaging migrate failed: {err}");
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

/// A [`TxHandler`] that counts calls and records each payload it received.
struct RecordHandler {
    calls: Arc<AtomicU32>,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl TxHandler for RecordHandler {
    fn call<'a>(
        &'a self,
        _conn: &'a mut sqlx::PgConnection,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), bus::Error>> {
        let calls = self.calls.clone();
        let seen = self.seen.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(payload);
            Ok(())
        })
    }
}

/// BLOCKER-2 without a DB: `register` installs a live transport and pre-allocates
/// the handler map, so a consumer's phase-2 `on_tx` records rather than panics.
#[tokio::test]
async fn register_installs_transport_before_init() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap(); // never touched (no query)
    let ctx = Context::with_db(pool);
    let m = Messaging::new();
    m.register(&ctx).unwrap();

    // A consumer's init would call bus.on_tx -> Transport::subscribe_tx. This runs
    // before messaging.init; it must not panic on an absent map.
    let et = bus::define::<serde_json::Value>("test.topic");
    ctx.bus().on_tx(&et, "consumer", |conn, v: serde_json::Value| {
        Box::pin(async move {
            let _ = (conn, v);
            Ok(())
        })
    });

    let inner = m.inner();
    assert_eq!(inner.subscribers_for("test.topic").len(), 1);
    // The marker is provided under "messaging" for validate_requires's boot check.
    assert!(ctx.registry().try_require::<dyn Service>("messaging").is_some());
}

/// `enqueue_tx` writes a row on the caller's tx with the module's origin.
#[tokio::test]
async fn enqueue_tx_writes_row_with_origin() {
    let Some(pool) = test_pool().await else { return };
    let origin = format!("test-enq-{}", unique());
    let inner = inner(pool.clone(), &origin);

    let mut tx = pool.begin().await.unwrap();
    inner
        .enqueue_tx(&mut tx, "test.enqueue", br#"{"a":1}"#)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let row = sqlx::query("SELECT origin, topic FROM messaging.outbox WHERE origin = $1")
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
    inner_a.enqueue_tx(&mut tx, "t.a", br#"{"n":1}"#).await.unwrap();
    inner_b.enqueue_tx(&mut tx, "t.b", br#"{"n":2}"#).await.unwrap();
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
    let relay = Relay::new(pool.clone(), "messaging", origin_a.clone(), HashMap::new(), vec![target]);
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
    let event_id = format!("messaging:test:{}", unique());
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));

    for _ in 0..2 {
        let h: Arc<dyn TxHandler> = Arc::new(RecordHandler {
            calls: calls.clone(),
            seen: seen.clone(),
        });
        inner.consume("sub-a", &event_id, b"{}", h).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler ran more than once — inbox dedup broken");

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
        .enqueue_tx(&mut tx, "rt.topic", br#"{"item":"starter-sword","qty":1}"#)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // A subscription that records the payload, wired through the real build path.
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(RecordHandler {
        calls: calls.clone(),
        seen: seen.clone(),
    });
    inner.subscribe_tx("rt.topic", "rt-sub", h);
    let targets = inner.build_local_targets();

    let relay = Relay::new(pool.clone(), "messaging", origin.clone(), HashMap::new(), targets);
    relay.drain_once().await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler not run exactly once");
    let payload = seen.lock().unwrap()[0].clone();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v["item"], "starter-sword");
    assert_eq!(v["qty"], 1);
    assert_eq!(unsent_count(&pool, &origin).await, 0, "row not marked sent after full delivery");

    cleanup(&pool, &origin).await;
}

async fn unsent_count(pool: &PgPool, origin: &str) -> i64 {
    use sqlx::Row;
    sqlx::query("SELECT count(*) AS n FROM messaging.outbox WHERE origin = $1 AND sent_at IS NULL")
        .bind(origin)
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>("n")
}

async fn cleanup(pool: &PgPool, origin: &str) {
    let _ = sqlx::query("DELETE FROM messaging.outbox WHERE origin = $1")
        .bind(origin)
        .execute(pool)
        .await;
}

async fn cleanup_inbox(pool: &PgPool, event_id: &str) {
    let _ = sqlx::query("DELETE FROM messaging.inbox WHERE event_id = $1")
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

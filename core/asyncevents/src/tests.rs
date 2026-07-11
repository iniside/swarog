use super::*;
// `bus::Transport` (the trait, for `enqueue_tx`) arrives via `use super::*`.
use bus::{AnyTx, SubscriptionSpec};
use lifecycle::Context;
use std::time::Duration;
use std::sync::atomic::{AtomicI32, Ordering};
use tokio::sync::Notify;

/// Fallback DSN when `DATABASE_URL` is unset — the same default `core/app` uses.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres and migrates the plane (V2 schema + legacy drop);
/// returns `None` (printing a skip line) when unreachable, so the suite degrades
/// to a no-op rather than failing where there's no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — asyncevents DB tests skipped");
            return None;
        }
    };
    if let Err(err) = (Plane::new(pool.clone(), dsn).unwrap()).migrate().await {
        eprintln!("SKIP: asyncevents migrate failed: {err}");
        return None;
    }
    Some(pool)
}

/// A unique, leaked topic so a rerun's rows never collide with a previous run's.
fn unique_topic(prefix: &str) -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Box::leak(format!("{prefix}.{}-{}", std::process::id(), nanos).into_boxed_str())
}

fn spec(id: &'static str) -> SubscriptionSpec {
    SubscriptionSpec {
        id,
        start: bus::StartPosition::Genesis,
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

    let et = bus::define::<serde_json::Value>(
        "test.topic",
        1,
        bus::HistoryPolicy::MinRetention { days: 7 },
    );
    ctx.bus().on_tx(spec("plane-live-consumer"), &et, |delivery, v: serde_json::Value| {
        Box::pin(async move {
            let _ = (delivery, v);
            Ok(())
        })
    });

    assert_eq!(plane.inner.snapshot().len(), 1);
}

/// `enqueue_tx` appends to the V2 log on the caller's tx: the row carries the
/// contract's topic + version and commits with the domain tx.
#[tokio::test]
async fn enqueue_tx_appends_event_on_callers_tx() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("plane.enqueue");
    let t = LogTransport::new();

    let mut tx = pool.begin().await.unwrap();
    t.enqueue_tx(
        AnyTx::new(&mut *tx),
        &transport::test_contract(topic),
        br#"{"a":1}"#,
    )
    .await
    .unwrap();

    // Uncommitted: invisible outside the tx.
    let n = testing::events_count(&pool, topic, "a", "1").await.unwrap();
    assert_eq!(n, 0, "event visible before the domain tx committed");

    tx.commit().await.unwrap();
    let n = testing::events_count(&pool, topic, "a", "1").await.unwrap();
    assert_eq!(n, 1, "event must commit with the domain tx");

    let (version,): (i32,) =
        sqlx::query_as("SELECT contract_version FROM asyncevents.events WHERE topic = $1")
            .bind(topic)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, 1);

    let _ = testing::cleanup_events(&pool, "a", "1").await;
}

/// The `testing::TestTransport` round-trip: emit through the bus seam, deliver
/// via the test-driveable worker, observe the handler effect — the shape module
/// tests lean on.
#[tokio::test]
async fn test_transport_round_trips_emit_to_delivery() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc as StdArc;

    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("plane.roundtrip");
    let sub_id: &'static str = Box::leak(format!("test.{topic}.v1").into_boxed_str());

    let tt = testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), tt.handle());

    let et = bus::define::<serde_json::Value>(topic, 1, bus::HistoryPolicy::MinRetention { days: 7 });
    let calls = StdArc::new(AtomicU32::new(0));
    let seen = calls.clone();
    ctx.bus().on_tx(spec(sub_id), &et, move |delivery, v: serde_json::Value| {
        let seen = seen.clone();
        Box::pin(async move {
            let _ = delivery;
            assert_eq!(v["k"], "v");
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });

    let mut tx = pool.begin().await.unwrap();
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &et, &serde_json::json!({"k": "v"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Frontier eligibility depends on unrelated in-flight transactions, so poll.
    let mut delivered = 0u64;
    for _ in 0..100 {
        delivered += tt.deliver_all().await.unwrap();
        if delivered >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(delivered, 1, "exactly one delivery expected");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Checkpointed: a second drain redelivers nothing.
    assert_eq!(tt.deliver_all().await.unwrap(), 0);

    let _ = testing::cleanup_events(&pool, "k", "v").await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(sub_id)
        .execute(&pool)
        .await;
}

/// The `/readyz` staleness probe's semantics (Step 2.2): never stalled without
/// workers (clock unseeded), fresh after a pass, stalled once the last healthy
/// pass ages past `max_age`, and never stalled while stopping (a controlled
/// stop is not an outage).
#[tokio::test]
async fn liveness_delivery_stalled_flags_only_a_running_plane_with_old_passes() {
    let l = Liveness::default();
    assert!(
        !l.delivery_stalled(Duration::ZERO),
        "a plane hosting no workers must never read as stalled"
    );

    l.mark_pass_ok();
    assert!(!l.delivery_stalled(Duration::from_secs(30)), "a fresh pass is not stale");

    // The coarse clock ticks in whole seconds; 2.1s guarantees age >= 1 > 0.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    assert!(
        l.delivery_stalled(Duration::ZERO),
        "a pass older than max_age must flag the plane"
    );

    l.stopping.store(true, Ordering::SeqCst);
    assert!(!l.delivery_stalled(Duration::ZERO), "a stopping plane is not a stall");
}

#[tokio::test]
async fn stop_terminates_active_backend_and_rolls_back_effect_and_checkpoint() {
    let Some(pool) = test_pool().await else { return };
    sqlx::query("CREATE TABLE IF NOT EXISTS asyncevents_stop_test_effects (tag text PRIMARY KEY)")
        .execute(&pool).await.unwrap();
    let tag = unique_topic("plane.stop-effect");
    let _ = sqlx::query("DELETE FROM asyncevents_stop_test_effects WHERE tag = $1")
        .bind(tag).execute(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = Plane::new(pool.clone(), dsn).unwrap()
        .with_stop_grace(Duration::from_millis(800));
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());
    let topic = unique_topic("plane.stop");
    let sub_id = unique_topic("plane-stop-sub");
    let event = bus::define::<serde_json::Value>(
        topic, 1, bus::HistoryPolicy::MinRetention { days: 7 },
    );
    let entered = Arc::new(Notify::new());
    let never = Arc::new(Notify::new());
    let pid = Arc::new(AtomicI32::new(0));
    ctx.bus().on_tx(spec(sub_id), &event, {
        let entered = entered.clone();
        let never = never.clone();
        let pid = pid.clone();
        move |mut delivery, _value: serde_json::Value| {
            let entered = entered.clone();
            let never = never.clone();
            let pid = pid.clone();
            Box::pin(async move {
                let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&mut *conn).await.map_err(bus::Error::transport)?;
                pid.store(backend, Ordering::SeqCst);
                sqlx::query("INSERT INTO asyncevents_stop_test_effects(tag) VALUES ($1)")
                    .bind(tag).execute(&mut *conn).await.map_err(bus::Error::transport)?;
                entered.notify_one();
                never.notified().await;
                Ok(())
            })
        }
    });
    plane.start().await.unwrap();
    // A healthy just-started plane is green: `start` seeded the staleness clock.
    assert!(
        !plane.liveness().delivery_stalled(Duration::from_secs(30)),
        "no false stall right after start"
    );
    let mut tx = pool.begin().await.unwrap();
    ctx.bus().emit_tx(AnyTx::new(&mut *tx), &event, &serde_json::json!({"tag": tag}))
        .await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), plane.stop()).await.unwrap();
    // A controlled stop never reads as a stall — even at max_age zero.
    assert!(
        !plane.liveness().delivery_stalled(Duration::ZERO),
        "controlled stop must not flag /readyz"
    );
    let backend = pid.load(Ordering::SeqCst);
    let present: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
        .bind(backend).fetch_one(&pool).await.unwrap();
    assert!(!present, "delivery backend survived stop");
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM asyncevents_stop_test_effects WHERE tag = $1")
        .bind(tag).fetch_one(&pool).await.unwrap();
    assert_eq!(effects, 0, "handler effect committed during forced stop");
    let cursor_tie: i64 = sqlx::query_scalar("SELECT cursor_tie FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(sub_id).fetch_one(&pool).await.unwrap();
    assert_eq!(cursor_tie, 0, "checkpoint advanced during forced stop");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(effects, sqlx::query_scalar::<_, i64>("SELECT count(*) FROM asyncevents_stop_test_effects WHERE tag = $1")
        .bind(tag).fetch_one(&pool).await.unwrap(), "delivery continued after stop");
    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1").bind(topic).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1").bind(sub_id).execute(&pool).await;
    let _ = sqlx::query("DROP TABLE IF EXISTS asyncevents_stop_test_effects").execute(&pool).await;
}

#[tokio::test]
async fn stale_backend_identity_cannot_terminate_live_reused_pid() {
    let Some(_pool) = test_pool().await else { return };
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut target = sqlx::PgConnection::connect(&dsn).await.unwrap();
    let (pid, backend_start): (i32, String) = sqlx::query_as(
        "SELECT pg_backend_pid(), backend_start::text FROM pg_stat_activity WHERE pid = pg_backend_pid()",
    ).fetch_one(&mut target).await.unwrap();
    let active = worker::ActiveDeliveries::default();
    let _guard = active.register(7, 11, pid, format!("stale-{backend_start}"));
    let claim = active.claim_active().pop().unwrap().1;
    assert!(!terminate_claim(&dsn, &active, 7, &claim).await.unwrap());
    let still_same_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut target).await.unwrap();
    assert_eq!(still_same_pid, pid, "stale claim terminated the live replacement backend");
}

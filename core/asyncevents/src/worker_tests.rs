use super::*;
use crate::catalog;
use crate::store;
use crate::transport::{test_contract, SubEntry};
use bus::{Delivery, StartPosition, SubscriptionSpec, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — asyncevents worker tests skipped");
            return None;
        }
    };
    if let Err(err) = crate::Plane::new(pool.clone(), dsn).unwrap().migrate().await {
        eprintln!("SKIP: asyncevents migrate failed: {err}");
        return None;
    }
    Some(pool)
}

fn unique(prefix: &str) -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Box::leak(format!("{prefix}.{}-{}", std::process::id(), nanos).into_boxed_str())
}

/// A configurable durable handler: counts calls, records payloads, optionally
/// errors for the first `fail_first` calls, optionally sleeps `delay` ON THE
/// DELIVERY CONNECTION (`pg_sleep`, so a timeout leaves a genuinely in-flight
/// statement behind).
struct TestHandler {
    calls: Arc<AtomicU32>,
    seen: Arc<Mutex<Vec<String>>>,
    fail_first: u32,
    pg_sleep_secs: f64,
}

impl TestHandler {
    fn ok(calls: Arc<AtomicU32>, seen: Arc<Mutex<Vec<String>>>) -> TestHandler {
        TestHandler { calls, seen, fail_first: 0, pg_sleep_secs: 0.0 }
    }
}

impl TxHandler for TestHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), bus::Error>> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.seen.lock().unwrap().push(String::from_utf8(payload).unwrap());
            if self.pg_sleep_secs > 0.0 {
                let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                sqlx::query("SELECT pg_sleep($1)")
                    .bind(self.pg_sleep_secs)
                    .execute(&mut *conn)
                    .await
                    .map_err(bus::Error::transport)?;
            }
            if n <= self.fail_first {
                return Err(bus::Error::transport(std::io::Error::other("poison")));
            }
            Ok(())
        })
    }
}

fn entry(sub_id: &'static str, topic: &'static str, handler: Arc<dyn TxHandler>) -> SubEntry {
    SubEntry {
        spec: SubscriptionSpec { id: sub_id, start: StartPosition::Genesis },
        topic: topic.to_string(),
        version: 1,
        history: Some(bus::HistoryPolicy::MinRetention { days: 7 }),
        handler,
    }
}

fn worker_ctx(pool: &PgPool, entries: Vec<SubEntry>, timeout: Duration) -> WorkerCtx {
    WorkerCtx {
        pool: pool.clone(),
        subs: entries,
        handler_timeout: timeout,
        wakeup: Arc::new(Notify::new()),
    }
}

async fn append_committed(pool: &PgPool, topic: &'static str, payload: &str) {
    let mut tx = pool.begin().await.unwrap();
    store::append(&mut tx, &test_contract(topic), payload.as_bytes())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Waits until at least `want` events on `topic` are frontier-eligible (unrelated
/// in-flight transactions in this shared-DB binary delay the snapshot xmin).
async fn wait_eligible(pool: &PgPool, topic: &str, want: i64) {
    for _ in 0..200 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM asyncevents.events \
             WHERE topic = $1 AND producer_xid < pg_snapshot_xmin(pg_current_snapshot())",
        )
        .bind(topic)
        .fetch_one(pool)
        .await
        .unwrap();
        if n >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("events on {topic} never became frontier-eligible");
}

/// Polls `deliver_one` until `want` cumulative deliveries (frontier eligibility
/// depends on unrelated in-flight transactions in this shared-DB test binary).
async fn deliver_until(ctx: &WorkerCtx, entry: &SubEntry, want: u32) -> u32 {
    let mut delivered = 0u32;
    for _ in 0..200 {
        match deliver_one(ctx, entry).await.unwrap() {
            Step::Delivered => {
                delivered += 1;
                if delivered >= want {
                    return delivered;
                }
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    delivered
}

async fn sub_row(pool: &PgPool, sub_id: &str) -> (String, i32, Option<String>, i64) {
    sqlx::query_as(
        "SELECT state, consecutive_failures, last_error, cursor_tie \
         FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn clear_backoff(pool: &PgPool, sub_id: &str) {
    sqlx::query(
        "UPDATE asyncevents.subscriptions SET next_attempt_at = NULL WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn cleanup(pool: &PgPool, topic: &str, sub_id: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(sub_id)
        .execute(pool)
        .await;
}

/// SKIP LOCKED single ownership: while worker A holds the subscription row
/// (handler in flight), worker B's attempt is `Skipped` — never a second
/// concurrent delivery. After A "crashes" mid-delivery (connection dropped
/// before commit), B resumes FROM THE CHECKPOINT and redelivers the same event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_locked_single_owner_and_failover_from_checkpoint() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.owner");
    let sub_id = unique("sub.owner");

    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    // A's handler holds the delivery (and the row lock) for 1.5s on the delivery
    // connection, then TIMES OUT at 1s — the crash-before-commit shape: the
    // effect never commits, the cursor never advances.
    let slow: Arc<dyn TxHandler> = Arc::new(TestHandler {
        calls: calls.clone(),
        seen: seen.clone(),
        fail_first: 0,
        pg_sleep_secs: 5.0,
    });
    let entry_a = entry(sub_id, topic, slow);
    catalog::reconcile(&pool, std::slice::from_ref(&entry_a)).await.unwrap();

    append_committed(&pool, topic, r#"{"n":1}"#).await;
    // Wait until the event is frontier-eligible before racing the two workers.
    wait_eligible(&pool, topic, 1).await;

    let fast_probe: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(
        Arc::new(AtomicU32::new(0)),
        Arc::new(Mutex::new(Vec::new())),
    ));
    let entry_b = entry(sub_id, topic, fast_probe);
    let ctx_b = worker_ctx(&pool, vec![entry_b.clone()], Duration::from_secs(10));

    // A starts a delivery that will hold the lock ~1s then poison its connection.
    let a = {
        let pool = pool.clone();
        let entry_a = entry_a.clone();
        tokio::spawn(async move {
            let ctx_a = worker_ctx(&pool, vec![entry_a.clone()], Duration::from_secs(1));
            deliver_one(&ctx_a, &entry_a).await.unwrap()
        })
    };
    // Give A time to take the row lock and enter the handler.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // B cannot double-deliver while A holds the lock.
    let step_b = deliver_one(&ctx_b, &entry_b).await.unwrap();
    assert_eq!(step_b, Step::Skipped, "second worker must skip the locked subscription");

    // A times out (crash-before-commit): backoff recorded, cursor NOT advanced.
    let step_a = a.await.unwrap();
    assert_eq!(step_a, Step::Faulted);
    let (state, failures, _, _) = sub_row(&pool, sub_id).await;
    assert_eq!(state, "active");
    assert_eq!(failures, 1);

    // Failover: B (a healthy replica) clears the backoff window and delivers the
    // SAME event from the checkpoint.
    clear_backoff(&pool, sub_id).await;
    let calls_b = Arc::new(AtomicU32::new(0));
    let seen_b = Arc::new(Mutex::new(Vec::new()));
    let ok: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(calls_b.clone(), seen_b.clone()));
    let entry_ok = entry(sub_id, topic, ok);
    let ctx_ok = worker_ctx(&pool, vec![entry_ok.clone()], Duration::from_secs(10));
    assert_eq!(deliver_until(&ctx_ok, &entry_ok, 1).await, 1);
    let delivered_payloads = seen_b.lock().unwrap().clone();
    assert_eq!(delivered_payloads.len(), 1, "exactly one failover delivery");
    assert!(delivered_payloads[0].contains("\"n\""), "unexpected payload: {delivered_payloads:?}");

    cleanup(&pool, topic, sub_id).await;
}

/// Crash-before-commit redelivers (at-least-once, handler ran twice for one
/// advance); crash-after-commit never redelivers (the checkpoint is the effect's
/// atomic sibling). Modeled with a fail-once handler: attempt 1 rolls back to
/// the savepoint (effect undone, cursor unchanged), attempt 2 commits, further
/// drains deliver nothing.
#[tokio::test]
async fn idempotence_around_the_commit_point() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.idem");
    let sub_id = unique("sub.idem");

    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(TestHandler {
        calls: calls.clone(),
        seen: seen.clone(),
        fail_first: 1,
        pg_sleep_secs: 0.0,
    });
    let e = entry(sub_id, topic, h);
    let ctx = worker_ctx(&pool, vec![e.clone()], Duration::from_secs(10));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    append_committed(&pool, topic, r#"{"n":1}"#).await;

    // First attempt fails (crash-before-commit equivalent): cursor unchanged.
    let mut first = Step::Skipped;
    for _ in 0..200 {
        first = deliver_one(&ctx, &e).await.unwrap();
        if first != Step::Empty && first != Step::Skipped {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(first, Step::Faulted);
    let (_, failures, last_error, cursor_tie) = sub_row(&pool, sub_id).await;
    assert_eq!(failures, 1);
    assert!(last_error.unwrap().contains("poison"));
    assert_eq!(cursor_tie, 0, "cursor must not advance on a failed delivery");

    // Redelivery succeeds: the handler ran twice for ONE cursor advance.
    clear_backoff(&pool, sub_id).await;
    assert_eq!(deliver_until(&ctx, &e, 1).await, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "at-least-once: redelivered after the rollback");

    // Crash-after-commit equivalent: the checkpoint is committed; a fresh worker
    // (same subscription row) delivers nothing.
    let ctx2 = worker_ctx(&pool, vec![e.clone()], Duration::from_secs(10));
    assert_eq!(deliver_one(&ctx2, &e).await.unwrap(), Step::Empty);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no redelivery past a committed checkpoint");

    cleanup(&pool, topic, sub_id).await;
}

/// Poison event: exponential backoff, pause after the threshold, and NO skip —
/// the cursor never moves past the failing event.
#[tokio::test]
async fn poison_backs_off_then_pauses_never_skips() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.poison");
    let sub_id = unique("sub.poison");

    let calls = Arc::new(AtomicU32::new(0));
    let h: Arc<dyn TxHandler> = Arc::new(TestHandler {
        calls: calls.clone(),
        seen: Arc::new(Mutex::new(Vec::new())),
        fail_first: u32::MAX,
        pg_sleep_secs: 0.0,
    });
    let e = entry(sub_id, topic, h);
    let ctx = worker_ctx(&pool, vec![e.clone()], Duration::from_secs(10));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    append_committed(&pool, topic, r#"{"n":1}"#).await;

    // First failure: failures=1, a backoff window is set, still active.
    let mut first = Step::Skipped;
    for _ in 0..200 {
        first = deliver_one(&ctx, &e).await.unwrap();
        if first == Step::Faulted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(first, Step::Faulted);
    let (state, failures, _, cursor_tie) = sub_row(&pool, sub_id).await;
    assert_eq!((state.as_str(), failures, cursor_tie), ("active", 1, 0));

    // Backing off: not due, so the next attempt skips (no busy retry).
    assert_eq!(deliver_one(&ctx, &e).await.unwrap(), Step::Skipped);

    // Fast-forward to the pause threshold: one more failure pauses the
    // subscription; the cursor still never moved.
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET consecutive_failures = 19, next_attempt_at = NULL WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(deliver_one(&ctx, &e).await.unwrap(), Step::Faulted);
    let (state, failures, _, cursor_tie) = sub_row(&pool, sub_id).await;
    assert_eq!((state.as_str(), failures, cursor_tie), ("paused", 20, 0), "must pause, never skip");

    // Paused: no more attempts.
    assert_eq!(deliver_one(&ctx, &e).await.unwrap(), Step::Skipped);

    cleanup(&pool, topic, sub_id).await;
}

/// Backoff curve: 1s doubling to the 5m cap.
#[test]
fn backoff_is_exponential_and_capped() {
    assert_eq!(backoff_secs(1), 1.0);
    assert_eq!(backoff_secs(2), 2.0);
    assert_eq!(backoff_secs(5), 16.0);
    assert_eq!(backoff_secs(10), 300.0);
    assert_eq!(backoff_secs(30), 300.0);
}

/// A handler timeout poisons ONLY its delivery connection: the pool stays
/// usable, the backoff lands via a fresh connection, the row lock is released
/// (the wedged backend is terminated), and a later healthy delivery succeeds.
#[tokio::test]
async fn timeout_poisons_only_the_delivery_connection() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.timeout");
    let sub_id = unique("sub.timeout");

    let calls = Arc::new(AtomicU32::new(0));
    let wedged: Arc<dyn TxHandler> = Arc::new(TestHandler {
        calls: calls.clone(),
        seen: Arc::new(Mutex::new(Vec::new())),
        fail_first: 0,
        pg_sleep_secs: 30.0,
    });
    let e = entry(sub_id, topic, wedged);
    let ctx = worker_ctx(&pool, vec![e.clone()], Duration::from_millis(500));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    append_committed(&pool, topic, r#"{"n":1}"#).await;

    let mut step = Step::Skipped;
    for _ in 0..200 {
        step = deliver_one(&ctx, &e).await.unwrap();
        if step == Step::Faulted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(step, Step::Faulted);

    // The pool is not poisoned; the failure was recorded on a fresh connection.
    let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(one, 1);
    let (state, failures, last_error, cursor_tie) = sub_row(&pool, sub_id).await;
    assert_eq!(state, "active");
    assert_eq!(failures, 1);
    assert!(last_error.unwrap().contains("timeout"));
    assert_eq!(cursor_tie, 0, "cursor must not advance on a timed-out delivery");

    // The row lock was released (terminated backend): a healthy handler on the
    // SAME subscription delivers the event.
    clear_backoff(&pool, sub_id).await;
    let ok_calls = Arc::new(AtomicU32::new(0));
    let ok: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(ok_calls.clone(), Arc::new(Mutex::new(Vec::new()))));
    let e_ok = entry(sub_id, topic, ok);
    let ctx_ok = worker_ctx(&pool, vec![e_ok.clone()], Duration::from_secs(10));
    assert_eq!(deliver_until(&ctx_ok, &e_ok, 1).await, 1, "row lock never released after the timeout");

    cleanup(&pool, topic, sub_id).await;
}

/// Lost NOTIFYs only delay delivery to the next poll tick: a worker whose
/// wake-up Notify NEVER fires still delivers via the global 1s poll fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_loss_is_covered_by_the_poll_fallback() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.poll");
    let sub_id = unique("sub.poll");

    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(calls.clone(), seen.clone()));
    let e = entry(sub_id, topic, h);
    let ctx = Arc::new(worker_ctx(&pool, vec![e], Duration::from_secs(10)));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    // No wakeup::listen task: the Notify never fires — poll only.
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(ctx.clone(), stop_rx));

    append_committed(&pool, topic, r#"{"n":1}"#).await;

    let mut delivered = false;
    for _ in 0..150 {
        if calls.load(Ordering::SeqCst) >= 1 {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = stop_tx.send(true);
    let _ = task.await;
    assert!(delivered, "poll fallback never delivered without a NOTIFY");

    cleanup(&pool, topic, sub_id).await;
}

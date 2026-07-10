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

/// A per-call unique `tie_breaker` (nanos + pid derived) so concurrent tests seeding
/// synthetic events at the same `(generation, producer_xid)` never collide on the
/// events PK. Positive by construction (sign bit masked off).
fn unique_tie() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    ((nanos ^ ((std::process::id() as u64) << 40)) as i64) & i64::MAX
}

/// Inserts one synthetic event directly at `(generation=0, producer_xid, tie_breaker)`.
/// `OVERRIDING SYSTEM VALUE` is required because `tie_breaker` is `GENERATED ALWAYS`;
/// pinning `generation = 0` (< the seeded `plane_meta.generation` of 1) makes the row
/// frontier-eligible deterministically, so no `wait_eligible` poll is needed.
async fn insert_synthetic_event(pool: &PgPool, topic: &str, producer_xid: &str, tie_breaker: i64) {
    sqlx::query(
        "INSERT INTO asyncevents.events \
           (generation, producer_xid, tie_breaker, topic, contract_version, payload) \
         OVERRIDING SYSTEM VALUE \
         VALUES (0, $1::xid8, $2, $3, 1, $4::jsonb)",
    )
    .bind(producer_xid)
    .bind(tie_breaker)
    .bind(topic)
    .bind(format!(r#"{{"xid":{producer_xid}}}"#))
    .execute(pool)
    .await
    .unwrap();
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

/// Regression: the worker's "next eligible event" pick must order by NUMERIC xid8, not
/// the text alias. Two synthetic events at producer_xid 999 and 1000 (generation 0,
/// both past the Genesis cursor): numerically 999 < 1000, so 999 delivers first. The
/// old `producer_xid::text AS producer_xid` + bare `ORDER BY` sorted `'1000' < '999'`
/// and delivered 1000 first — breaking per-subscription XID-order delivery.
#[tokio::test]
async fn next_pick_uses_numeric_xid_order_not_text() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.xidorder");
    let sub_id = unique("sub.xidorder");

    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(calls.clone(), seen.clone()));
    let e = entry(sub_id, topic, h);
    let ctx = worker_ctx(&pool, vec![e.clone()], Duration::from_secs(10));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    // generation 0 ⇒ frontier-eligible vs the seeded plane_meta generation (1),
    // independent of the live XID counter, so no wait_eligible is needed.
    let t999 = unique_tie();
    let t1000 = unique_tie();
    insert_synthetic_event(&pool, topic, "999", t999).await;
    insert_synthetic_event(&pool, topic, "1000", t1000).await;

    let step = deliver_one(&ctx, &e).await.unwrap();
    assert_eq!(step, Step::Delivered, "one event must deliver");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one delivery per step");
    // jsonb re-renders with a space after the colon (`{"xid": 999}`).
    assert!(
        seen[0].replace(' ', "").contains("\"xid\":999"),
        "numeric order must deliver producer_xid 999 first, got: {seen:?}"
    );

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

/// Timeout-arm race (F9): `record_failure` is CAS-guarded on the claim-time
/// cursor AND `consecutive_failures`. On the timeout arm the row lock is released
/// (terminated backend) before the failure is recorded, so a healthy replica that
/// delivered — advancing the cursor and zeroing failures — in the
/// terminate-to-record window must make the stale failure write match zero rows.
/// The subscription stays active with `consecutive_failures = 0`; no stale
/// backoff, no spurious pause.
#[tokio::test]
async fn stale_timeout_failure_does_not_pause_a_healthy_subscription() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("worker.stalefail");
    let sub_id = unique("sub.stalefail");

    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h: Arc<dyn TxHandler> = Arc::new(TestHandler::ok(calls.clone(), seen.clone()));
    let e = entry(sub_id, topic, h);
    let ctx = worker_ctx(&pool, vec![e.clone()], Duration::from_secs(10));
    catalog::reconcile(&pool, &ctx.subs).await.unwrap();

    // The claim-time state the timeout arm captured: the freshly-reconciled row
    // sits at the Genesis cursor with 0 failures.
    let (claim_gen, claim_xid, claim_tie): (i64, String, i64) = sqlx::query_as(
        "SELECT cursor_generation, cursor_xid::text, cursor_tie \
         FROM asyncevents.subscriptions WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Replica B delivers in the terminate-to-record window: the cursor advances
    // and failures stay reset. (A direct out-of-band UPDATE stands in for B.)
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
         SET cursor_generation = cursor_generation + 1, consecutive_failures = 0 \
         WHERE subscription_id = $1",
    )
    .bind(sub_id)
    .execute(&pool)
    .await
    .unwrap();

    // The stale timeout arm now records failure against the CLAIM-time cursor —
    // which no longer matches, so the CAS guard matches zero rows.
    let mut conn = pool.acquire().await.unwrap();
    record_failure(
        &mut conn,
        sub_id,
        1,
        "handler timeout (stale)",
        claim_gen,
        &claim_xid,
        claim_tie,
        0,
    )
    .await
    .unwrap();
    drop(conn);

    // Healthy subscription untouched: still active, failures 0, no error recorded.
    let (state, failures, last_error, _) = sub_row(&pool, sub_id).await;
    assert_eq!(state, "active");
    assert_eq!(failures, 0, "stale failure must not have been recorded");
    assert!(last_error.is_none(), "no error recorded on a stale CAS miss");

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

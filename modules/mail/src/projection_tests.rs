//! The durable ingress and the retention sweep, driven through REAL deliveries of
//! `mail.send_requested` and `scheduler.fired`.
//!
//! `enqueue_or_skip`'s three arms are the point: two of them answer `Ok(())` and advance the
//! checkpoint, one must propagate. Swapping the propagating arm to `Ok(())` silently LOSES
//! the event — no backoff, no `last_error`, nothing to find it by — so a decoy faulting
//! subscription runs in the same pass to prove the counters can move at all.

use std::sync::Arc;

use bus::{AnyTx, Delivery, TxHandler};
use sqlx::PgPool;

use crate::projection::{
    enqueue_conflicts, enqueue_rejected, PruneHandler, PRUNE_BATCH, PRUNE_SCHEDULE_NAME, PRUNE_SUB,
    SEND_REQUESTED_SUB,
};
use crate::store::{STATE_CANCELLED, STATE_PARKED, STATE_PENDING, STATE_SENT};
use crate::tests::{cleanup, ensure_schema, on_drop, test_pool, unique_key, DbGuard, DB_LOCK};
use crate::{Context, MailModule, Module, Service};

async fn reset_subscription(pool: &PgPool, id: &str) {
    sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// Wires the module the way `app::run` does — `register` THEN `init` — over a hand-driven
/// durable transport, and drains once BEFORE the test emits anything.
///
/// The trailing `deliver_all` is ordering-critical rather than a warm-up: both subscriptions
/// start at `Genesis`, so this pass consumes whatever the shared log already holds. Its
/// return value is NOT asserted for that reason.
async fn wired_for_delivery(
    pool: &PgPool,
) -> (Context, Arc<Service>, asyncevents::testing::TestTransport) {
    ensure_schema(pool).await;
    reset_subscription(pool, SEND_REQUESTED_SUB.id).await;
    reset_subscription(pool, PRUNE_SUB.id).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let m = MailModule::new();
    m.register(&ctx).unwrap();
    m.init(&ctx).unwrap();
    transport.deliver_all().await.unwrap();
    (ctx, m.svc(), transport)
}

async fn emit_send_requested(ctx: &Context, pool: &PgPool, key: &str, to: &str, body: &str) {
    let mut tx = pool.begin().await.unwrap();
    let e = mailevents::SendRequested {
        idempotency_key: key.to_string(),
        to: to.to_string(),
        subject: "Verify your address".to_string(),
        body: body.to_string(),
        kind: "verification".to_string(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &mailevents::SEND_REQUESTED, &e)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn emit_fired(ctx: &Context, pool: &PgPool, name: &str) {
    let mut tx = pool.begin().await.unwrap();
    let fired = schedulerevents::Fired { name: name.into() };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &schedulerevents::FIRED, &fired)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn rows_for(pool: &PgPool, key: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT state, body FROM mail.outbox WHERE idempotency_key = $1")
        .bind(key)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// THE non-poisoning proof: a handler that returned `Err` leaves `consecutive_failures = 1`
/// plus a backoff here, and at 20 it PAUSES the subscription — taking the whole outbound
/// channel down over one bad payload. "No row was written" alone is satisfied by a poisoned
/// handler; this is not.
async fn subscription_health(pool: &PgPool, id: &str) -> (String, i32, Option<String>) {
    sqlx::query_as(
        "SELECT state, consecutive_failures, last_error FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_unpoisoned(pool: &PgPool, id: &str) {
    let (state, failures, last_error) = subscription_health(pool, id).await;
    assert_eq!(state, "active", "{id} must still be active");
    assert_eq!(
        failures, 0,
        "a data-quality verdict must return Ok(()), never Err; last_error = {last_error:?}"
    );
}

async fn clear_backoff(pool: &PgPool, id: &str) {
    sqlx::query(
        "UPDATE asyncevents.subscriptions \
            SET consecutive_failures = 0, last_error = NULL, next_attempt_at = NULL \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

/// One simple-query round trip against the POOL, not a `&mut PgConnection` inside a
/// transaction handle: the drop guard's future has to be `Send + 'static`, and the borrowed
/// connection executor is what stops it from being. The bounds still apply — `SET LOCAL`
/// needs the explicit `BEGIN`/`COMMIT` around it to have any effect.
async fn alter_barrier(pool: PgPool, statement: &'static str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(&format!(
        "BEGIN; SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '30s'; \
         {statement}; COMMIT;"
    ))
    .execute(&pool)
    .await
    .map(|_| ())
}

/// Adds a `CHECK (false) NOT VALID` that makes EVERY new outbox row fail with 23514 — an
/// infrastructure-class failure raised INSIDE the plane's delivery transaction, where the
/// handler's error class decides between a retry and a lost event — and DROPS it when the
/// returned guard falls. A guard rather than a trailing call because this one is worse than
/// a stray row: a panic while the barrier is up leaves every later insert in the shared
/// cluster failing with 23514, for this suite and for any live fleet, until someone drops
/// the constraint by hand.
#[must_use = "the barrier is dropped when this guard drops — binding it to `_` drops it at once"]
async fn arm_insert_barrier(pool: &PgPool) -> DbGuard {
    const DROP: &str = "ALTER TABLE mail.outbox DROP CONSTRAINT IF EXISTS mail_test_insert_barrier";
    alter_barrier(pool.clone(), DROP).await.unwrap();
    alter_barrier(
        pool.clone(),
        "ALTER TABLE mail.outbox ADD CONSTRAINT mail_test_insert_barrier CHECK (false) NOT VALID",
    )
    .await
    .unwrap();
    let pool = pool.clone();
    on_drop(move || async move {
        alter_barrier(pool, DROP)
            .await
            .expect("the insert barrier MUST come off — it refuses every later insert");
    })
}

/// The event-log twin of [`cleanup`], and a guard for the same reason: a `mail.send_requested`
/// event that survives a panicking test is re-delivered by the NEXT run's pass, which
/// re-creates the very outbox row that run then cleans up — the leak reappears as somebody
/// else's row.
#[must_use = "the events are deleted when this guard drops — binding it to `_` drops it at once"]
fn cleanup_event(pool: &PgPool, key: &str) -> DbGuard {
    let pool = pool.clone();
    let key = key.to_string();
    on_drop(move || async move {
        let _ = asyncevents::testing::cleanup_events(&pool, "idempotency_key", &key).await;
    })
}

// ============================================================================
// 1. The three arms of the durable ingress.
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_durable_request_becomes_one_outbox_row() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "ingress").await;
    let _cleanup = cleanup(&pool, &[&key]);
    let _cleanup_events = cleanup_event(&pool, &key);

    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);

    assert_eq!(rows_for(&pool, &key).await, vec![(STATE_PENDING.to_string(), "hello".to_string())]);
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;
}

/// Delivery is at-least-once per subscription, so the SAME request arriving twice must
/// answer `Ok(())` and leave one row — an `Err` would back the subscription off toward a
/// pause over a duplicate the plane's own contract guarantees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeated_request_is_deduplicated_without_faulting_the_subscription() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "ingress-dup").await;
    let _cleanup = cleanup(&pool, &[&key]);
    let _cleanup_events = cleanup_event(&pool, &key);

    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;
    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;
    assert_eq!(transport.deliver_all().await.unwrap(), 2, "both events ARE delivered");

    assert_eq!(rows_for(&pool, &key).await.len(), 1);
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;
}

/// A reused key holding a DIFFERENT message is a producer bug and a message that was never
/// enqueued. The count must move by EXACTLY one: a `Duplicate` misclassified as `Conflict`
/// still leaves one row and the original subject, and only the exact count sees it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reused_key_holding_a_different_message_is_counted_and_skipped() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "ingress-conflict").await;
    let _cleanup = cleanup(&pool, &[&key]);
    let _cleanup_events = cleanup_event(&pool, &key);

    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    let before = enqueue_conflicts().get();

    emit_send_requested(&ctx, &pool, &key, "player@example.com", "a different body").await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the conflicting event is DELIVERED and answered Ok — the verdict is in the handler"
    );

    assert_eq!(enqueue_conflicts().get(), before + 1);
    assert_eq!(
        rows_for(&pool, &key).await,
        vec![(STATE_PENDING.to_string(), "hello".to_string())],
        "the first message must survive untouched"
    );
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;
}

/// One message's data quality answers `Ok(())`: an `Err` would pause the subscription and
/// take the whole outbound channel down over one producer's bad payload. The counter is the
/// only place these become visible at all, since the checkpoint advances.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unroutable_request_is_refused_counted_and_skipped() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "ingress-invalid").await;
    let _cleanup_events = cleanup_event(&pool, &key);
    let before = enqueue_rejected().get();

    emit_send_requested(&ctx, &pool, &key, "not-an-address", "hello").await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);

    assert_eq!(enqueue_rejected().get(), before + 1);
    assert!(rows_for(&pool, &key).await.is_empty(), "a refused request writes no row");
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;
}

/// The arm no `Ok(())` proves: anything that is NOT `Status::Invalid` is infrastructure and
/// MUST propagate. Swallowed into `Ok(())` it advances the checkpoint over an event that was
/// never applied — the request is LOST FOREVER, with no backoff and no `last_error` to find
/// it by, while `/readyz` stays green.
///
/// The failure is a 23514 raised inside the delivery transaction by a `CHECK (false)` added
/// for the duration. Nothing asserts between the two barrier calls: a panic there would
/// leave the shared table refusing every insert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_infrastructure_failure_faults_the_delivery_and_keeps_the_event() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "ingress-infra").await;
    let _cleanup = cleanup(&pool, &[&key]);
    let _cleanup_events = cleanup_event(&pool, &key);
    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;

    let delivered;
    let health;
    let rows;
    {
        let _barrier = arm_insert_barrier(&pool).await;
        delivered = transport.deliver_all().await;
        health = subscription_health(&pool, SEND_REQUESTED_SUB.id).await;
        rows = rows_for(&pool, &key).await.len();
    }

    assert_eq!(
        delivered.unwrap(),
        0,
        "an infrastructure failure must NOT be counted as a delivery — a counted one means \
         the checkpoint moved past a request that was never enqueued"
    );
    assert_eq!(rows, 0, "the barrier refused the insert");
    assert_eq!(
        health.1, 1,
        "the failure must be RECORDED so the plane retries; state = {:?}, last_error = {:?}",
        health.0, health.2
    );
    assert!(health.2.is_some(), "the failure must carry its error, not be swallowed");
    assert_eq!(health.0, "active", "one failure backs off, it does not pause yet");

    // The event was RETAINED. The backoff is cleared as explicit state, never waited out.
    clear_backoff(&pool, SEND_REQUESTED_SUB.id).await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "with the barrier gone the SAME event must still be there to deliver"
    );
    assert_eq!(rows_for(&pool, &key).await.len(), 1, "nothing was lost");
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;
}

/// The instrument itself, proven by construction: a decoy subscription on the SAME topic
/// whose handler always fails, delivered in the same pass. Without it, `delivered == 1` and
/// `consecutive_failures == 0` are assertions nobody has shown can fail — every skip test
/// above would be green by absence of errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_faulting_handler_is_uncounted_and_backed_off() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    const DECOY_SUB: &str = "mail.tests.poison-decoy.v1";
    ensure_schema(&pool).await;
    reset_subscription(&pool, SEND_REQUESTED_SUB.id).await;
    reset_subscription(&pool, PRUNE_SUB.id).await;
    reset_subscription(&pool, DECOY_SUB).await;
    // The decoy is a subscription this test INVENTS in the shared plane: left behind with its
    // backoff it shows up in `eventctl` and in any audit of the subscription graph. The reset
    // at the top of the next run is luck, not a cleanup.
    let _decoy = {
        let pool = pool.clone();
        on_drop(move || async move { reset_subscription(&pool, DECOY_SUB).await })
    };

    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let m = MailModule::new();
    m.register(&ctx).unwrap();
    m.init(&ctx).unwrap();
    ctx.bus().on_tx(
        bus::SubscriptionSpec {
            id: DECOY_SUB,
            start: bus::StartPosition::AfterRegistration,
        },
        &mailevents::SEND_REQUESTED,
        |_delivery, _e: mailevents::SendRequested| {
            Box::pin(async move {
                Err(bus::Error::transport(std::io::Error::other(
                    "decoy handler: always fails",
                )))
            })
        },
    );
    transport.deliver_all().await.unwrap();

    let key = unique_key(&pool, "decoy").await;
    let _cleanup = cleanup(&pool, &[&key]);
    let _cleanup_events = cleanup_event(&pool, &key);
    emit_send_requested(&ctx, &pool, &key, "player@example.com", "hello").await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "one event, two subscriptions: only the Ok delivery is counted — which is exactly \
         what `delivered == 1` asserts in the skip tests"
    );
    assert_eq!(rows_for(&pool, &key).await.len(), 1);
    assert_unpoisoned(&pool, SEND_REQUESTED_SUB.id).await;

    let (state, failures, last_error) = subscription_health(&pool, DECOY_SUB).await;
    assert_eq!(state, "active", "one failure backs off, it does not pause yet");
    assert_eq!(
        failures, 1,
        "a handler that returns Err DOES move consecutive_failures — so the `== 0` \
         assertions elsewhere are not vacuous"
    );
    assert!(last_error.is_some(), "the failure is recorded, not swallowed");
}

// ============================================================================
// 2. Retention.
// ============================================================================

/// Backdates one row into a state and an age no production path can produce.
async fn seed_aged(pool: &PgPool, key: &str, state: &str, age_days: i32) {
    sqlx::query(
        "INSERT INTO mail.outbox \
             (idempotency_key, recipient, subject, body, kind, state, created_at, sent_at) \
         VALUES ($1, 'player@example.com', 's', 'b', 'k', $2, \
                 now() - make_interval(days => $3), now() - make_interval(days => $3))",
    )
    .bind(key)
    .bind(state)
    .bind(age_days)
    .execute(pool)
    .await
    .unwrap();
}

async fn states_of(pool: &PgPool, keys: &[&str]) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT idempotency_key, state FROM mail.outbox WHERE idempotency_key = ANY($1) \
          ORDER BY idempotency_key",
    )
    .bind(keys.iter().map(|k| k.to_string()).collect::<Vec<String>>())
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The sweep reached through a REAL delivery of `scheduler.fired`. Both halves are asserted
/// in one statement: `parked` past retention must STAY — it is the state waiting on an
/// operator, and the one the drain will never revisit on its own — so an over-broad
/// predicate cannot pass on the deletion alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scheduler_fire_prunes_delivered_rows_and_leaves_parked_ones_alone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;

    let old_sent = unique_key(&pool, "prune-old-sent").await;
    let old_cancelled = unique_key(&pool, "prune-old-cancelled").await;
    let old_parked = unique_key(&pool, "prune-old-parked").await;
    let old_pending = unique_key(&pool, "prune-old-pending").await;
    let fresh_sent = unique_key(&pool, "prune-fresh-sent").await;
    let retention = crate::config::DEFAULT_RETENTION_DAYS;
    seed_aged(&pool, &old_sent, STATE_SENT, retention + 5).await;
    seed_aged(&pool, &old_cancelled, STATE_CANCELLED, retention + 5).await;
    seed_aged(&pool, &old_parked, STATE_PARKED, retention + 5).await;
    seed_aged(&pool, &old_pending, STATE_PENDING, retention + 5).await;
    seed_aged(&pool, &fresh_sent, STATE_SENT, 1).await;
    let all = [
        old_sent.as_str(),
        old_cancelled.as_str(),
        old_parked.as_str(),
        old_pending.as_str(),
        fresh_sent.as_str(),
    ];
    let _cleanup = cleanup(&pool, &all);
    assert_eq!(states_of(&pool, &all).await.len(), 5);

    emit_fired(&ctx, &pool, PRUNE_SCHEDULE_NAME).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);

    let mut left: Vec<String> = states_of(&pool, &all)
        .await
        .into_iter()
        .map(|(key, state)| format!("{key}={state}"))
        .collect();
    left.sort();
    let mut expected = vec![
        format!("{fresh_sent}={STATE_SENT}"),
        format!("{old_parked}={STATE_PARKED}"),
        format!("{old_pending}={STATE_PENDING}"),
    ];
    expected.sort();
    assert_eq!(
        left, expected,
        "only `sent`/`cancelled` past retention may go — a parked row is what an operator \
         still has to act on"
    );
    assert_unpoisoned(&pool, PRUNE_SUB.id).await;
}

/// The subscription is a RAW sink on the whole `scheduler.fired` topic, so the name guard is
/// the only thing standing between another module's daily fire and this module's retention.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_schedule_name_prunes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, _svc, transport) = wired_for_delivery(&pool).await;
    let key = unique_key(&pool, "prune-foreign").await;
    let _cleanup = cleanup(&pool, &[&key]);
    seed_aged(&pool, &key, STATE_SENT, crate::config::DEFAULT_RETENTION_DAYS + 5).await;

    emit_fired(&ctx, &pool, "audit-prune").await;
    assert_eq!(
        transport.deliver_all().await.unwrap(),
        1,
        "the foreign fire IS delivered to this subscription — the guard is in the handler"
    );
    assert_eq!(states_of(&pool, &[&key]).await.len(), 1, "another module's schedule must not prune mail");

    // The positive control on the SAME row: this module's own name does prune it.
    emit_fired(&ctx, &pool, PRUNE_SCHEDULE_NAME).await;
    assert_eq!(transport.deliver_all().await.unwrap(), 1);
    assert!(states_of(&pool, &[&key]).await.is_empty());
    assert_unpoisoned(&pool, PRUNE_SUB.id).await;
}

/// The watermarked LOOP, with a statement-level probe as the instrument: every DELETE this
/// module issues is counted, so the test can assert that no single statement exceeds
/// `PRUNE_BATCH` (an unbounded DELETE would seq-scan and take everything in one) and that
/// the sweep KEEPS GOING until a short batch — a per-fire CAP would leave retention
/// permanently behind any inflow above one batch per day.
///
/// Everything — the probe, the rows and the deletes — lives in ONE transaction that is
/// rolled back, so the shared table is untouched and no cleanup can be skipped by a panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_fire_loops_batched_deletes_and_never_exceeds_the_batch_per_statement() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let seeded = PRUNE_BATCH * 2 + 7;
    let tag = unique_key(&pool, "prune-probe").await;

    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        // `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on `mail.outbox`: unbounded, a
        // concurrent long transaction turns this test into a HANG that blocks every other
        // writer instead of a failure.
        "SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '60s'; \
         CREATE TEMP TABLE mail_prune_probe (seq serial, n bigint) ON COMMIT DROP; \
         CREATE FUNCTION pg_temp.mail_prune_probe_log() RETURNS trigger LANGUAGE plpgsql AS $fn$ \
           BEGIN INSERT INTO mail_prune_probe (n) SELECT count(*) FROM removed; RETURN NULL; END $fn$; \
         CREATE TRIGGER mail_prune_probe_trigger AFTER DELETE ON mail.outbox \
           REFERENCING OLD TABLE AS removed FOR EACH STATEMENT \
           EXECUTE FUNCTION pg_temp.mail_prune_probe_log();",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mail.outbox \
             (idempotency_key, recipient, subject, body, kind, state, created_at) \
         SELECT $1 || '-' || n, 'player@example.com', 's', 'b', 'k', 'sent', \
                now() - make_interval(days => 99) \
           FROM generate_series(1, $2) AS n",
    )
    .bind(&tag)
    .bind(seeded)
    .execute(&mut *tx)
    .await
    .unwrap();

    let handler = PruneHandler {
        retention_days: crate::config::DEFAULT_RETENTION_DAYS,
    };
    let payload = serde_json::to_vec(&schedulerevents::Fired {
        name: PRUNE_SCHEDULE_NAME.to_string(),
    })
    .unwrap();
    handler
        .call(
            Delivery {
                event_id: "mail-prune-probe-event",
                tx: AnyTx::new(&mut *tx),
            },
            payload,
        )
        .await
        .expect("the sweep must answer Ok");

    let counts: Vec<(i64,)> = sqlx::query_as("SELECT n FROM mail_prune_probe ORDER BY seq")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let counts: Vec<i64> = counts.into_iter().map(|(n,)| n).collect();
    assert!(
        counts.len() >= 3,
        "{seeded} stale rows at {PRUNE_BATCH} per statement must take at least 3 statements — \
         a per-fire CAP would stop after one; got {counts:?}"
    );
    assert!(
        counts.iter().all(|n| *n <= PRUNE_BATCH),
        "no single statement may delete more than PRUNE_BATCH rows; got {counts:?}"
    );
    assert!(
        *counts.last().unwrap() < PRUNE_BATCH,
        "the loop must end on a SHORT batch, not on a full one; got {counts:?}"
    );
    assert!(
        counts.iter().sum::<i64>() >= seeded,
        "every stale row must be swept in the one fire; got {counts:?} for {seeded} rows"
    );
    let (left,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM mail.outbox WHERE idempotency_key LIKE $1 || '-%'")
            .bind(&tag)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(left, 0, "the fire must leave no stale row behind");

    tx.rollback().await.unwrap();
    // The rows cannot show the rollback — they were inserted AND deleted inside it. The
    // TRIGGER can: it is the artifact that would hurt the shared database if this probe ever
    // leaked, and it exists only if the transaction committed.
    let (triggers,): (i64,) = sqlx::query_as("SELECT count(*) FROM pg_trigger WHERE tgname = $1")
        .bind("mail_prune_probe_trigger")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        triggers, 0,
        "the probe transaction rolled back — its trigger must not survive on the shared table"
    );
}

/// A malformed `scheduler.fired` payload is a DECODE failure, not a data-quality verdict:
/// the sweep has nothing to match a name against, so it propagates and the plane retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payload_without_a_schedule_name_faults_rather_than_pruning() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let key = unique_key(&pool, "prune-malformed").await;
    let _cleanup = cleanup(&pool, &[&key]);
    seed_aged(&pool, &key, STATE_SENT, crate::config::DEFAULT_RETENTION_DAYS + 5).await;

    let handler = PruneHandler {
        retention_days: crate::config::DEFAULT_RETENTION_DAYS,
    };
    let mut tx = pool.begin().await.unwrap();
    let result = handler
        .call(
            Delivery {
                event_id: "mail-prune-malformed",
                tx: AnyTx::new(&mut *tx),
            },
            b"{\"not_a_name\":1}".to_vec(),
        )
        .await;
    tx.rollback().await.unwrap();
    assert!(result.is_err(), "a payload with no name must not be silently swept over");
    assert_eq!(states_of(&pool, &[&key]).await.len(), 1);
}

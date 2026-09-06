//! The drain: the claim, the status write's CAS, the retry ladder, and the failure half of
//! a pass — the branches no other test in this workspace executes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;
use tokio::sync::watch;

use crate::providers::{Outgoing, SendError, Sender};
use crate::store::{
    Disposition, Store, STATE_CANCELLED, STATE_PARKED, STATE_PENDING, STATE_SENT,
};
use crate::tests::{
    cleanup, enqueue, expire_lease, id_of, mail_of, row_of, test_pool, unique_key, wired, DB_LOCK,
    DEFAULT_DSN,
};
use crate::worker::{
    backoff_secs, bounded_tx, claim_lease, disposition, drain_pass, pass_budget, send_cas_misses,
    stall_max, stalled_from, write_budget, Drain, Liveness, ACQUIRE_DEADLINE, BACKOFF_MAX_SECS,
};

/// A [`Sender`] that COUNTS its calls and answers what the test told it to. "Did the send
/// happen?" becomes an assertion rather than an absence of errors, and the failure arms are
/// driven without a relay.
struct FakeSender {
    calls: Arc<AtomicUsize>,
    verdict: Verdict,
}

#[derive(Clone, Copy)]
enum Verdict {
    Ok,
    Rejected,
    Infra,
}

fn faking(verdict: Verdict) -> (Arc<dyn Sender>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(FakeSender {
            calls: calls.clone(),
            verdict,
        }),
        calls,
    )
}

#[async_trait]
impl Sender for FakeSender {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn send(&self, _m: &Outgoing<'_>) -> Result<(), SendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.verdict {
            Verdict::Ok => Ok(()),
            Verdict::Rejected => Err(SendError::Rejected(anyhow::anyhow!("550 no such user"))),
            Verdict::Infra => Err(SendError::Infra(anyhow::anyhow!("connection refused"))),
        }
    }
}

/// A sender that MUTATES the row it is delivering, in the window between the claim and the
/// status write — the "an operator cancelled it during the SMTP dialogue" and "the lease
/// expired and the row moved on" scenarios, reproduced by construction rather than by racing
/// a real clock.
struct MutatingSender {
    calls: Arc<AtomicUsize>,
    pool: PgPool,
    key: String,
    statements: Vec<&'static str>,
    /// The operator's own requeue, then a fresh claim — both through the production
    /// authorities, so the row the in-flight attempt returns to is exactly the one a live
    /// drain would have left behind.
    requeue_and_reclaim: Option<(Arc<crate::Service>, String)>,
}

#[async_trait]
impl Sender for MutatingSender {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn send(&self, _m: &Outgoing<'_>) -> Result<(), SendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        for statement in &self.statements {
            sqlx::query(statement)
                .bind(&self.key)
                .execute(&self.pool)
                .await
                .expect("the mid-send mutation must apply");
        }
        if let Some((svc, id)) = &self.requeue_and_reclaim {
            assert_eq!(
                svc.requeue_parked(id).await.unwrap(),
                1,
                "the mid-send requeue must move the row"
            );
            let mut tx = bounded_tx(&self.pool, Duration::from_secs(5)).await.unwrap();
            Store
                .claim_due_tx(&mut tx, 30.0)
                .await
                .unwrap()
                .expect("a requeued row is due, so the drain re-claims it");
            tx.commit().await.unwrap();
        }
        Ok(())
    }
}

fn mutating(pool: &PgPool, key: &str, statements: Vec<&'static str>) -> (Arc<dyn Sender>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(MutatingSender {
            calls: calls.clone(),
            pool: pool.clone(),
            key: key.to_string(),
            statements,
            requeue_and_reclaim: None,
        }),
        calls,
    )
}

/// The same sender, plus the two production moves that make `attempts` useless as a CAS
/// leg: the operator requeue resets it to 0 and the fresh claim writes it straight back to
/// the value the in-flight attempt is holding.
fn mutating_then_requeueing(
    pool: &PgPool,
    key: &str,
    statements: Vec<&'static str>,
    svc: &Arc<crate::Service>,
    id: &str,
) -> (Arc<dyn Sender>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(MutatingSender {
            calls: calls.clone(),
            pool: pool.clone(),
            key: key.to_string(),
            statements,
            requeue_and_reclaim: Some((svc.clone(), id.to_string())),
        }),
        calls,
    )
}

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

fn drain_with(pool: &PgPool, sender: Arc<dyn Sender>, max_attempts: i32) -> Drain {
    Drain {
        pool: pool.clone(),
        sender,
        from: "noreply@example.com".to_string(),
        send_timeout: SEND_TIMEOUT,
        max_attempts,
    }
}

/// The drain is a GLOBAL consumer: it claims whatever is due, including a row left behind by
/// an aborted fleet run against this shared database. Foreign pending rows are pushed out of
/// the pass's reach — DELAYED, never deleted — so a call count of 1 means this test's row.
async fn quarantine_foreign_pending(pool: &PgPool, key: &str) {
    sqlx::query(
        "UPDATE mail.outbox SET next_attempt_at = now() + interval '1 hour' \
          WHERE state = 'pending' AND idempotency_key <> $1",
    )
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

/// One pass with the stop signal clear and a budget wide enough that exhaustion is never
/// what ends it.
async fn one_pass(drain: &Drain) {
    let (_tx, stop) = watch::channel(false);
    let deadline = std::time::Instant::now() + pass_budget(drain.send_timeout);
    drain_pass(drain, &Store, &stop, deadline)
        .await
        .expect("a pass over a healthy pool is infrastructure-clean");
}

// ============================================================================
// 1. The pure decision functions.
// ============================================================================

/// `attempts` comes from a column an operator can edit, so the `clamp` and `saturating_pow`
/// are overflow guards, not style: without them `2^(attempts-1)` overflows and the ladder
/// wraps to a value below its own floor.
#[test]
fn the_backoff_ladder_doubles_and_saturates_at_its_cap() {
    assert_eq!(backoff_secs(1), 1.0);
    assert_eq!(backoff_secs(2), 2.0);
    assert_eq!(backoff_secs(20), BACKOFF_MAX_SECS as f64);
    assert_eq!(backoff_secs(31), BACKOFF_MAX_SECS as f64);
    assert_eq!(backoff_secs(10_000), BACKOFF_MAX_SECS as f64);
    // The ladder is monotone up to its cap and never returns a value below its floor.
    assert!(backoff_secs(3) > backoff_secs(2));
    assert!(backoff_secs(0) >= 1.0, "a hand-edited 0 must not invert the ladder");
    for attempts in [1, 2, 3, 9, 20, 31, 1_000, i32::MAX] {
        let secs = backoff_secs(attempts);
        assert!(
            (1.0..=BACKOFF_MAX_SECS as f64).contains(&secs),
            "attempts {attempts} produced {secs}"
        );
    }
}

/// The whole point of deriving both from one function: a send budget wider than the pass
/// budget makes the "no room for a full attempt" guard true on the FIRST row of every pass,
/// so the drain claims nothing forever while every pass still stamps healthy.
#[test]
fn the_pass_budget_is_a_floor_that_always_admits_one_full_attempt() {
    for secs in [1u64, 5, 10, 30, 60, 300] {
        let send = Duration::from_secs(secs);
        let budget = pass_budget(send);
        assert!(
            budget >= send + ACQUIRE_DEADLINE,
            "{secs}s: a pass must have room for one checkout plus one attempt"
        );
        assert!(budget >= Duration::from_secs(30), "{secs}s: the floor holds");
        assert_eq!(stall_max(send), budget * 2, "{secs}s: the stall threshold is derived");
    }
}

/// The lease covers the WHOLE attempt: send, checkout, status write. Below the
/// `ACQUIRE_DEADLINE` floor the write window wins and a re-claim can overlap an in-flight
/// send — the documented at-least-once cost, which the generation CAS keeps correct.
#[test]
fn the_write_budget_keeps_a_status_write_inside_the_claim_lease() {
    for secs in [5u64, 6, 10, 30, 120, 300] {
        let send = Duration::from_secs(secs);
        assert!(
            send + ACQUIRE_DEADLINE + write_budget(send) <= claim_lease(send),
            "{secs}s: send + checkout + write must fit inside the lease"
        );
    }
    // The floor: a write window is never zero, whatever the send budget.
    for millis in [1u64, 100, 4_999] {
        assert_eq!(write_budget(Duration::from_millis(millis)), ACQUIRE_DEADLINE);
    }
}

/// Pure and total over the send taxonomy, so "the last attempt failed transiently, park it"
/// is provable with no relay and no DB.
#[test]
fn the_disposition_is_total_over_the_send_taxonomy() {
    assert_eq!(disposition(Ok(()), 1, 20), Disposition::Sent);
    // A rejection is PERMANENT: retrying hammers a relay that will never accept it.
    assert!(matches!(
        disposition(Err(SendError::Rejected(anyhow::anyhow!("550"))), 1, 20),
        Disposition::Parked { .. }
    ));
    // Infrastructure retries until the attempt ceiling — parking it earlier drops a
    // deliverable message on a socket error.
    match disposition(Err(SendError::Infra(anyhow::anyhow!("refused"))), 1, 3) {
        Disposition::Retry { backoff_secs: b, .. } => assert_eq!(b, backoff_secs(1)),
        other => panic!("attempt 1 of 3 must retry, got {other:?}"),
    }
    assert!(matches!(
        disposition(Err(SendError::Infra(anyhow::anyhow!("refused"))), 3, 3),
        Disposition::Parked { .. }
    ));
    assert!(matches!(
        disposition(Err(SendError::Infra(anyhow::anyhow!("refused"))), 9, 3),
        Disposition::Parked { .. }
    ));
    // The relay's answer rides into a bounded column.
    let long = "x".repeat(crate::store::LAST_ERROR_MAX_BYTES * 2);
    let Disposition::Parked { last_error } =
        disposition(Err(SendError::Rejected(anyhow::anyhow!("{long}"))), 1, 20)
    else {
        panic!("a rejection parks")
    };
    assert!(last_error.len() <= crate::store::LAST_ERROR_MAX_BYTES + '…'.len_utf8());
}

/// `last_ok_secs == 0` is the never-seeded sentinel and a controlled stop is not a stall —
/// either read as one would flip `/readyz` red on every cold boot and every shutdown.
#[test]
fn the_stall_predicate_answers_only_for_a_seeded_running_loop() {
    let max = Duration::from_secs(60);
    assert!(!stalled_from(0, 10_000, false, max), "never seeded is not a stall");
    assert!(!stalled_from(100, 1_000, true, max), "a controlled stop is not a stall");
    assert!(!stalled_from(100, 160, false, max), "exactly at the threshold is not yet stale");
    assert!(stalled_from(100, 161, false, max), "one second past it IS a stall");
    assert!(!stalled_from(100, 50, false, max), "a clock that went backwards is not a stall");
}

/// The check reports the loop's death regardless of the stamp — a loop that exited leaves a
/// fresh stamp behind it.
#[tokio::test]
async fn the_readiness_check_is_green_from_loop_entry_and_red_when_the_loop_dies() {
    let liveness = Liveness::default();
    let max = Duration::from_secs(30);
    // Never seeded: HTTP serves before the first pass on a cold boot.
    liveness.check(max).expect("an unseeded stamp is not a stall");
    let (_stop_tx, stop_rx) = watch::channel(true);
    let handle = crate::worker::spawn(
        Drain {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
            sender: faking(Verdict::Ok).0,
            from: "noreply@example.com".to_string(),
            send_timeout: SEND_TIMEOUT,
            max_attempts: 20,
        },
        Store,
        liveness.clone(),
        stop_rx,
    );
    // The loop exits immediately on the pre-set stop signal while the module was never told
    // it is stopping — which the supervision wrapper must report as death.
    handle.await.unwrap();
    let reason = liveness
        .check(max)
        .expect_err("a loop that exited while the module was running is not ready");
    assert!(reason.contains("died"), "got {reason:?}");
}

#[tokio::test]
async fn a_stopping_module_is_never_reported_as_a_dead_loop() {
    let liveness = Liveness::default();
    liveness.set_stopping();
    let (_stop_tx, stop_rx) = watch::channel(true);
    crate::worker::spawn(
        Drain {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
            sender: faking(Verdict::Ok).0,
            from: "noreply@example.com".to_string(),
            send_timeout: SEND_TIMEOUT,
            max_attempts: 20,
        },
        Store,
        liveness.clone(),
        stop_rx,
    )
    .await
    .unwrap();
    liveness
        .check(Duration::from_secs(30))
        .expect("a controlled stop must not flip /readyz red during shutdown");
}

// ============================================================================
// 2. The claim.
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_burns_an_attempt_bumps_the_generation_and_pushes_the_lease_out() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "claim").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let row = Store
        .claim_due_tx(&mut tx, claim_lease(SEND_TIMEOUT).as_secs_f64())
        .await
        .unwrap()
        .expect("a due row must be claimable");
    tx.commit().await.unwrap();
    assert_eq!(row.recipient, "player@example.com");
    assert_eq!(row.body, "hello");
    assert_eq!(row.attempts, 1, "the claim burns the attempt up front");
    assert_eq!(row.generation, 1);

    // The lease is what makes the row unclaimable now — a COMMITTED update, not a lock.
    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    assert!(
        Store.claim_due_tx(&mut tx, 30.0).await.unwrap().is_none(),
        "a leased row must not be claimed again while its lease holds"
    );
    tx.commit().await.unwrap();

    // Lease expiry is PERSISTED STATE, set explicitly rather than waited out.
    expire_lease(&pool, &key).await;
    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let again = Store
        .claim_due_tx(&mut tx, 30.0)
        .await
        .unwrap()
        .expect("an expired lease makes the row due again");
    tx.commit().await.unwrap();
    assert_eq!(again.id, row.id);
    assert_eq!(again.attempts, 2, "the re-claim burns a second attempt");
    assert_eq!(again.generation, 2, "and the ABA guard is monotone");
}

/// `FOR UPDATE SKIP LOCKED` makes replicas a consumer group by CONSTRUCTION. Two
/// connections, one due row, both claims begun before either commits: concurrency, not
/// speed — nothing here races a clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_claims_of_one_due_row_yield_one_winner() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "exclusive").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;
    let id = id_of(&pool, &key).await;

    let mut a = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let mut b = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let first = Store.claim_due_tx(&mut a, 30.0).await.unwrap();
    // `b` runs while `a` still holds the row lock, which is the only moment SKIP LOCKED can
    // be observed: after `a` commits the lease alone would explain the miss.
    let second = Store.claim_due_tx(&mut b, 30.0).await.unwrap();
    a.commit().await.unwrap();
    b.commit().await.unwrap();

    let winners: Vec<String> = [first, second]
        .into_iter()
        .flatten()
        .map(|c| c.id)
        .filter(|claimed| *claimed == id)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one of two concurrent claims may own the row, got {winners:?}"
    );
    let (_, _, attempts, generation, _) = row_of(&pool, &key).await;
    assert_eq!(attempts, 1, "the loser must not burn a second attempt");
    assert_eq!(generation, 1);
}

// ============================================================================
// 3. The status write's CAS — both legs.
// ============================================================================

/// THE generation leg. An operator requeue resets `attempts` to 0, so a superseded
/// attempt's `attempts` value becomes reachable again after one fresh claim: CAS'ing on it
/// would let a write from an attempt two claims old flip a row that is being delivered
/// RIGHT NOW to `sent` and blank its body.
///
/// The stale write is replayed with the FIRST claim in hand after the row has moved on, and
/// it must match zero rows, leave the row `pending` with its body intact, and be COUNTED.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_write_from_a_superseded_attempt_matches_no_row_and_is_counted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "cas-generation").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;
    let id = id_of(&pool, &key).await;

    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let stale = Store.claim_due_tx(&mut tx, 30.0).await.unwrap().unwrap();
    tx.commit().await.unwrap();

    // The operator requeue is what makes `attempts` unusable as the CAS leg: it resets the
    // counter the first claim wrote, and the next claim writes that same value again.
    park_row(&pool, &key).await;
    assert_eq!(svc.requeue_parked(&id).await.unwrap(), 1);
    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let fresh = Store.claim_due_tx(&mut tx, 30.0).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        fresh.attempts, stale.attempts,
        "the requeue makes the superseded attempt's `attempts` value reachable again — \
         which is exactly why it cannot be the CAS leg"
    );
    assert!(fresh.generation > stale.generation, "the generation is monotone");

    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let matched = Store
        .finish_tx(&mut tx, &stale, &Disposition::Sent, "fake")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(matched, 0, "the superseded attempt's write must match NO row");
    let (state, body, _, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PENDING, "the in-flight attempt still owns the row");
    assert_eq!(body, "hello", "a message still in flight must not have its body blanked");

    // The positive control on the SAME row: the CURRENT attempt's write DOES land, so the
    // zero above is not "finish_tx never matches".
    let mut tx = bounded_tx(&pool, Duration::from_secs(5)).await.unwrap();
    let matched = Store
        .finish_tx(&mut tx, &fresh, &Disposition::Sent, "fake")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(matched, 1);
    let (state, body, _, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_SENT);
    assert_eq!(body, "", "a delivered row's rendered body is blanked");
}

/// THE state leg, on the REAL path: the cancel lands DURING the dialogue, and the pass's own
/// status write must not put the row back to `sent`. Reported as delivered, a message the
/// operator stopped would also have its body blanked — which a durable replay then compares
/// `"" != body` against.
///
/// This is also the only place `mail_send_cas_misses_total` is moved: the counter belongs to
/// the pass, and it is how an operator sees an overlap that leaves no error behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_landing_mid_send_survives_the_pass_status_write_and_is_counted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "cas-state").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let (sender, calls) = mutating(
        &pool,
        &key,
        vec!["UPDATE mail.outbox SET state = 'cancelled' WHERE idempotency_key = $1"],
    );
    let before = send_cas_misses().get();
    one_pass(&drain_with(&pool, sender, 20)).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "the row WAS claimed and attempted");
    let (state, body, _, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_CANCELLED, "the cancel must not be overwritten back to sent");
    assert_eq!(body, "hello", "a cancelled body must survive for a durable replay");
    assert_eq!(
        send_cas_misses().get(),
        before + 1,
        "a status write that matched nothing is a NAMED outcome the pass counts"
    );
}

/// THE generation leg, on the REAL path: an operator parks and requeues the row DURING the
/// dialogue and the drain re-claims it, so the row the in-flight attempt is about to write
/// belongs to a later claim. Both counters are what the requeue-then-claim sequence leaves:
/// `attempts` is back at the value the superseded attempt itself holds — the ABA the errata
/// records — so `generation` is the ONLY leg that can separate them, and the in-flight
/// write must lose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_requeue_landing_mid_send_survives_the_pass_status_write_and_is_counted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "cas-generation-pass").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;
    let id = id_of(&pool, &key).await;

    let (sender, calls) = mutating_then_requeueing(
        &pool,
        &key,
        vec!["UPDATE mail.outbox SET state = 'parked' WHERE idempotency_key = $1"],
        &svc,
        &id,
    );
    let before = send_cas_misses().get();
    one_pass(&drain_with(&pool, sender, 20)).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (state, body, attempts, generation, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PENDING, "the re-claimed row is still queued for delivery");
    assert_eq!(
        attempts, 1,
        "the fresh claim rewrote `attempts` to exactly what the superseded attempt holds — \
         a CAS on it would match, which is why the leg is `generation`"
    );
    assert_eq!(generation, 3, "claim, requeue and re-claim each bumped the monotone leg");
    assert_eq!(body, "hello", "a message queued for a fresh delivery keeps its body");
    assert_eq!(send_cas_misses().get(), before + 1);
}

async fn park_row(pool: &PgPool, key: &str) {
    sqlx::query("UPDATE mail.outbox SET state = 'parked' WHERE idempotency_key = $1")
        .bind(key)
        .execute(pool)
        .await
        .unwrap();
}

// ============================================================================
// 4. The failure half of a pass, through the real `drain_pass`.
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_pass_delivers_the_row_and_blanks_its_body() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "pass-ok").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let (sender, calls) = faking(Verdict::Ok);
    one_pass(&drain_with(&pool, sender, 20)).await;

    assert!(calls.load(Ordering::SeqCst) >= 1, "the provider must actually be called");
    let (state, body, attempts, _, last_error) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_SENT);
    assert_eq!(body, "");
    assert_eq!(attempts, 1);
    assert_eq!(last_error, "");
    let (provider,): (Option<String>,) =
        sqlx::query_as("SELECT provider FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(provider.as_deref(), Some("fake"));
}

/// A permanent refusal parks on attempt ONE. Retrying it hammers a relay that will never
/// accept the message, and the ladder would burn every attempt before an operator saw it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_send_parks_the_row_on_its_first_attempt() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "park-rejected").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let (sender, calls) = faking(Verdict::Rejected);
    let drain = drain_with(&pool, sender, 20);
    one_pass(&drain).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "one row, one attempt");
    let (state, body, attempts, _, last_error) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PARKED, "a permanent refusal never retries");
    assert_eq!(attempts, 1, "with MAIL_MAX_ATTEMPTS at 20, the ceiling did not park it");
    assert_eq!(body, "hello", "the message still has to be sent once an operator fixes it");
    assert!(last_error.contains("550 no such user"), "got {last_error:?}");

    // A parked row is out of the drain's reach: a second pass must not touch it.
    one_pass(&drain).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a parked row is not re-attempted");
}

/// The infrastructure ladder, walked to its end: every attempt below `MAIL_MAX_ATTEMPTS`
/// stays `pending` and accrues, and ONLY the last one parks. The counting fake is what
/// makes each pass's attempt observable; `next_attempt_at` is reset as explicit persisted
/// state between passes rather than waited out on a real clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_infrastructure_failure_backs_off_and_parks_only_at_the_attempt_ceiling() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "park-infra").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    const MAX_ATTEMPTS: i32 = 3;
    let (sender, calls) = faking(Verdict::Infra);
    let drain = drain_with(&pool, sender, MAX_ATTEMPTS);

    for attempt in 1..MAX_ATTEMPTS {
        one_pass(&drain).await;
        let (state, body, attempts, _, last_error) = row_of(&pool, &key).await;
        assert_eq!(state, STATE_PENDING, "attempt {attempt} of {MAX_ATTEMPTS} must retry");
        assert_eq!(attempts, attempt, "the attempt count accrues");
        assert_eq!(body, "hello");
        assert!(last_error.contains("connection refused"), "got {last_error:?}");
        assert_eq!(calls.load(Ordering::SeqCst) as i32, attempt);
        // The backoff is real: the row is NOT due again until it elapses.
        let (due_later,): (bool,) = sqlx::query_as(
            "SELECT next_attempt_at > now() FROM mail.outbox WHERE idempotency_key = $1",
        )
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(due_later, "attempt {attempt} must push the row out by its backoff");
        expire_lease(&pool, &key).await;
    }

    one_pass(&drain).await;
    let (state, body, attempts, _, last_error) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PARKED, "the last transient failure parks the row");
    assert_eq!(attempts, MAX_ATTEMPTS);
    assert_eq!(body, "hello", "a parked body survives for the operator's requeue");
    assert!(last_error.contains("connection refused"), "got {last_error:?}");
    assert_eq!(calls.load(Ordering::SeqCst) as i32, MAX_ATTEMPTS);
}

/// The operator's recovery path, end to end on the SAME row: a requeue restarts the ladder,
/// and the next pass with a working provider delivers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_requeued_parked_row_is_delivered_by_the_next_pass() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "requeue-drain").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let (rejecting, _) = faking(Verdict::Rejected);
    one_pass(&drain_with(&pool, rejecting, 20)).await;
    assert_eq!(row_of(&pool, &key).await.0, STATE_PARKED);

    let id = id_of(&pool, &key).await;
    assert_eq!(svc.requeue_parked(&id).await.unwrap(), 1);
    let (working, calls) = faking(Verdict::Ok);
    one_pass(&drain_with(&pool, working, 20)).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (state, body, attempts, _, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_SENT);
    assert_eq!(body, "");
    assert_eq!(attempts, 1, "the requeue restarted the ladder");
}

/// A cancelled row is not the drain's, and a pass must not attempt it — the send would be a
/// real message the operator stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_row_is_never_attempted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "cancelled-drain").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;
    let id = id_of(&pool, &key).await;
    assert_eq!(svc.cancel_pending(&id).await.unwrap(), 1);

    let (sender, calls) = faking(Verdict::Ok);
    one_pass(&drain_with(&pool, sender, 20)).await;

    assert_eq!(calls.load(Ordering::SeqCst), 0, "a cancelled row is not due");
    assert_eq!(row_of(&pool, &key).await.0, STATE_CANCELLED);
}

/// A stop signalled between rows ends the pass at a ROW BOUNDARY with nothing claimed —
/// happens-before through the `watch` channel, never a sleep.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_pass_claims_nothing() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let svc = wired(&pool).await;
    let key = unique_key(&pool, "stopped").await;
    let _cleanup = cleanup(&pool, &[&key]);
    enqueue(&svc, &pool, &mail_of(&key, "hello")).await;
    quarantine_foreign_pending(&pool, &key).await;

    let (sender, calls) = faking(Verdict::Ok);
    let drain = drain_with(&pool, sender, 20);
    let (_tx, stop) = watch::channel(true);
    let deadline = std::time::Instant::now() + pass_budget(drain.send_timeout);
    drain_pass(&drain, &Store, &stop, deadline)
        .await
        .expect("a stopped pass is not a failure");

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (state, _, attempts, generation, _) = row_of(&pool, &key).await;
    assert_eq!(state, STATE_PENDING);
    assert_eq!(attempts, 0, "a stop before the claim must not burn an attempt");
    assert_eq!(generation, 0);
}

/// The `statement_timeout` argument is clamped into Postgres's int32 millisecond domain: an
/// out-of-range value is refused by the SERVER, which would make every pass fail before it
/// claimed a row for a reason that has nothing to do with mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_statement_budget_is_clamped_into_the_int32_millisecond_domain() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    crate::tests::ensure_schema(&pool).await;

    for budget in [
        Duration::from_millis(0),
        Duration::from_secs(5),
        Duration::from_secs(u64::MAX / 2_000),
        Duration::MAX,
    ] {
        let mut tx = bounded_tx(&pool, budget)
            .await
            .unwrap_or_else(|e| panic!("{budget:?} must produce a usable transaction: {e:#}"));
        let (applied,): (String,) = sqlx::query_as("SELECT current_setting('statement_timeout')")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_ne!(applied, "0", "{budget:?} must leave a real bound in place");
        tx.commit().await.unwrap();
    }
}

// ============================================================================
// 5. The failure half of the LOOP, and the stop path.
// ============================================================================

/// The reason `/readyz` needs a stamp and not just a died flag: a loop that is alive but
/// fails EVERY pass never exits, so `dead` stays false while the channel delivers nothing.
/// Every pass fails by construction — a CLOSED pool errors on the first checkout, no relay
/// and no timeout involved — and the clock is ADVANCED past the derived threshold rather
/// than waited out, so the only way the stamp can still read fresh is the loop marking a
/// FAILED pass healthy.
#[tokio::test(start_paused = true)]
async fn a_drain_that_errors_every_pass_goes_stale_while_the_loop_is_still_alive() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap();
    pool.close().await;
    let liveness = Liveness::default();
    let (_stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(crate::worker::run_loop(
        drain_with(&pool, faking(Verdict::Ok).0, 20),
        Store,
        liveness.clone(),
        Duration::from_secs(1),
        stop_rx,
    ));
    tokio::task::yield_now().await;
    liveness
        .check(stall_max(SEND_TIMEOUT))
        .expect("the loop seeds the stamp at entry — a cold boot is not a stall");

    tokio::time::advance(stall_max(SEND_TIMEOUT) + Duration::from_secs(5)).await;
    // The failing passes run AT the advanced clock: a loop that stamped its `Err` arm would
    // refresh the stamp here and read green below.
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }

    let reason = liveness
        .check(stall_max(SEND_TIMEOUT))
        .expect_err("a drain that has not completed a healthy pass in >2 budgets is not ready");
    assert!(
        reason.contains("no healthy mail drain pass"),
        "the STALL must be what flips readiness, not the loop dying: got {reason:?}"
    );
    assert!(!task.is_finished(), "the loop is alive — `dead` alone would keep /readyz green");
    task.abort();
}

/// A guard that records its own drop, so "the aborted task's state was released" is an
/// assertion rather than an absence of errors.
struct DropRecorder(Arc<AtomicUsize>);

impl Drop for DropRecorder {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// The grace path: a loop honouring the signal exits on the SIGNAL, inside the grace, and
/// is never aborted. The clock is paused, so the 4s grace is elapsed virtually if the
/// signal is ever missed — no wall-clock wait either way.
#[tokio::test(start_paused = true)]
async fn stop_tasks_stops_a_cooperating_loop_on_the_signal() {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let observed = Arc::new(AtomicUsize::new(0));
    let seen = observed.clone();
    let task = tokio::spawn(async move {
        stop_rx.changed().await.expect("the stop sender outlives the signal");
        seen.fetch_add(1, Ordering::SeqCst);
    });

    crate::worker::stop_tasks(Some(stop_tx), vec![task]).await;

    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "the loop must exit because it OBSERVED the signal, not because the grace aborted it"
    );
}

/// The abort path: a loop that never observes the signal is aborted at the grace, and the
/// abort is AWAITED — dropping the task's state (in production, its pool connection)
/// before `stop_tasks` returns rather than detaching it into shutdown.
#[tokio::test(start_paused = true)]
async fn a_loop_that_ignores_the_stop_signal_is_aborted_and_awaited() {
    let (stop_tx, _stop_rx) = watch::channel(false);
    let dropped = Arc::new(AtomicUsize::new(0));
    let recorder = DropRecorder(dropped.clone());
    let task = tokio::spawn(async move {
        let _held = recorder;
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    assert_eq!(dropped.load(Ordering::SeqCst), 0, "the task holds its state while it runs");

    crate::worker::stop_tasks(Some(stop_tx), vec![task]).await;

    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "an aborted task whose handle was awaited has released its state"
    );
}

/// The `Ok(Err(JoinError))` arm: the supervision wrapper catches pass panics, so a join
/// error means the wrapper itself died. `stop_tasks` must report it and CARRY ON — a
/// propagated panic or an early return would leave every later task unstopped.
#[tokio::test(start_paused = true)]
async fn a_task_that_died_is_reported_and_the_remaining_loops_are_still_stopped() {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let died = tokio::spawn(async { panic!("the supervision wrapper itself died") });
    let observed = Arc::new(AtomicUsize::new(0));
    let seen = observed.clone();
    let alive = tokio::spawn(async move {
        stop_rx.changed().await.expect("the stop sender outlives the signal");
        seen.fetch_add(1, Ordering::SeqCst);
    });
    tokio::task::yield_now().await;

    crate::worker::stop_tasks(Some(stop_tx), vec![died, alive]).await;

    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "a JoinError on the first handle must not stop the loop over the rest"
    );
}

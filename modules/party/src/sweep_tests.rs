//! The lease sweep over the live database: removal with leader handoff, dissolution, the
//! once-only removal under two concurrent sweeps, a whole pass (expired invites and parties),
//! and the supervision of the loop — a failing party withholds the liveness stamp, a panicking
//! pass marks the sweep dead.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use partyapi::Player as _;

use crate::sweep;
use crate::tests::*;
use crate::Service;

/// Once-wrong branch: the sweep applies the LEAVE rule to an expired member — the leader's
/// lease expiring hands leadership to the earliest remaining member, stamps ONE revision, and
/// pushes the new state to the rest and a "no party" view (same revision) to the removed one.
/// A second sweep finds nothing under the lock and pushes nothing; unexpired members are
/// never touched.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_removes_an_expired_leader_hands_off_and_pushes() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        // Ids descend in join order, so the earliest-joined member holds the larger id.
        let [leader, a, b] = fx.players_by_id_desc();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &a).await;
        let before = join(&fx, &leader, &b).await.revision;
        expire_lease(&fx.pool, &leader).await;
        fx.sink.clear();

        assert!(fx.svc.sweep_party(&party).await.unwrap(), "an expired member is removed");
        let (revision, new_leader) = party_row(&fx.pool, &party).await.unwrap();
        assert_eq!(new_leader, a, "earliest joined remaining member leads");
        assert!(revision > before);
        assert_eq!(members_of(&fx.pool, &party).await, vec![a.clone(), b.clone()]);
        assert_eq!(player_rev(&fx.pool, &leader).await, Some(revision));

        let gone = fx.sink.states_to(&leader);
        assert_eq!(gone.len(), 1);
        assert!(is_no_party(&gone[0]));
        assert_eq!(gone[0].revision, revision);
        for p in [&a, &b] {
            let pushed = fx.sink.states_to(p);
            assert_eq!(pushed.len(), 1);
            assert_eq!(pushed[0].revision, revision);
            assert_eq!(pushed[0].leader_id, a);
            assert_eq!(pushed[0].members.len(), 2);
        }
        assert_eq!(fx.sink.total(), 3);

        assert!(!fx.svc.sweep_party(&party).await.unwrap(), "nothing left to sweep");
        assert_eq!(fx.sink.total(), 3, "a no-op sweep pushes nothing");
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().0, revision, "a no-op sweep does not stamp");
    }).await;
}

/// Once-wrong branch: a party whose every member expired is DISSOLVED by the sweep — the
/// party row and its pending invites go, and every removed player is told at one revision.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_dissolves_a_party_whose_members_all_expired() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, invitee] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        let invite = invite_id(&fx, &leader, &invitee).await;
        expire_lease(&fx.pool, &leader).await;
        expire_lease(&fx.pool, &member).await;
        fx.sink.clear();

        assert!(fx.svc.sweep_party(&party).await.unwrap());
        assert!(party_row(&fx.pool, &party).await.is_none());
        assert!(!invite_exists(&fx.pool, &invite).await, "dissolution drops the party's invites");
        let (a, b) = (fx.sink.states_to(&leader), fx.sink.states_to(&member));
        assert_eq!((a.len(), b.len()), (1, 1));
        assert!(is_no_party(&a[0]) && is_no_party(&b[0]));
        assert_eq!(a[0].revision, b[0].revision, "one stamp covers every removed member");
        assert_eq!(player_rev(&fx.pool, &leader).await, Some(a[0].revision));
        assert_eq!(player_rev(&fx.pool, &member).await, Some(a[0].revision));
        assert_eq!(fx.sink.total(), 2);
    }).await;
}

/// Once-wrong branch: two sweeps of the same expired party (two replicas) are both parked on
/// the party's lock; released, the member is removed ONCE — one sweep answers `true`, the
/// other re-checks under the lock and answers `false` — and exactly one set of pushes goes out.
#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_sweeps_remove_an_expired_member_once() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        expire_lease(&fx.pool, &member).await;
        let tx = hold_party_lock(&fx, &party).await;

        let (r1, r2) = (fx.racer(2), fx.racer(2));
        let (s1, s2) = (r1.svc.clone(), r2.svc.clone());
        let (p1, p2) = (party.clone(), party.clone());
        let h1 = tokio::spawn(async move { s1.sweep_party(&p1).await });
        let h2 = tokio::spawn(async move { s2.sweep_party(&p2).await });
        wait_blocked(&fx.pool, &r1.app).await;
        wait_blocked(&fx.pool, &r2.app).await;
        fx.sink.clear();
        tx.rollback().await.unwrap();

        let mut outcomes = [h1.await.unwrap().unwrap(), h2.await.unwrap().unwrap()];
        outcomes.sort();
        assert_eq!(outcomes, [false, true], "exactly one sweep removes the member");
        assert_eq!(members_of(&fx.pool, &party).await, vec![leader.clone()]);
        assert_eq!(fx.sink.total(), 2, "one state push to the leader and one no-party push to the member");
        let (revision, _) = party_row(&fx.pool, &party).await.unwrap();
        assert_eq!(player_rev(&fx.pool, &member).await, Some(revision), "one stamp");
    }).await;
}

/// Once-wrong branch: the expiry re-check UNDER the lock. A sweep that selected an expired
/// member is parked on the party's lock; the member's heartbeat (which takes no party lock)
/// renews the lease meanwhile. Released, the sweep must re-evaluate `lease_until < now()` and
/// find nothing: `false`, the member still present, no push. A sweep that acted on the
/// pre-lock expiry would remove a member who had just proved liveness.
#[tokio::test(flavor = "multi_thread")]
async fn a_sweep_parked_on_the_lock_rechecks_expiry_after_a_heartbeat_renews_the_lease() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        expire_lease(&fx.pool, &member).await;
        assert!(lease_remaining(&fx.pool, &member).await < 0.0);
        let tx = hold_party_lock(&fx, &party).await;

        let racer = fx.racer(2);
        let (svc, p) = (racer.svc.clone(), party.clone());
        let sweep = tokio::spawn(async move { svc.sweep_party(&p).await });
        wait_blocked(&fx.pool, &racer.app).await;
        fx.sink.clear();

        let beat = fx.svc.heartbeat(who(&member)).await.unwrap();
        assert!(!beat.lease_until.is_empty(), "the heartbeat completed while the sweep was parked");
        assert!(!sweep.is_finished(), "the sweep must still be parked on the party lock");
        tx.rollback().await.unwrap();

        let removed = finish("sweep", sweep).await.unwrap();
        assert!(!removed, "the re-check under the lock sees the renewed lease");
        assert_eq!(members_of(&fx.pool, &party).await.len(), 2, "the live member was not removed");
        assert!(lease_remaining(&fx.pool, &member).await > 0.0);
        assert_eq!(fx.sink.total(), 0, "a no-op sweep pushes nothing");
    }).await;
}

/// Once-wrong branch: a whole pass — expired parties are swept AND `expires_at < now()`
/// invites are deleted, while an unexpired invite and unexpired members survive.
#[tokio::test(flavor = "multi_thread")]
async fn a_pass_sweeps_expired_parties_and_deletes_only_expired_invites() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, live_target, stale_target] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        let live = invite_id(&fx, &leader, &live_target).await;
        let stale = invite_id(&fx, &leader, &stale_target).await;
        expire_invite(&fx.pool, &stale).await;
        expire_lease(&fx.pool, &member).await;

        let (_stop, rx) = watch::channel(false);
        sweep::sweep_pass(&fx.svc, &rx).await.expect("a healthy pass");
        assert_eq!(members_of(&fx.pool, &party).await, vec![leader.clone()]);
        assert!(invite_exists(&fx.pool, &live).await, "an unexpired invite survives the pass");
        assert!(!invite_exists(&fx.pool, &stale).await, "an expired invite is deleted");
    }).await;
}

/// Once-wrong branch: a pass in which ONE party fails (here its lock is held, so the
/// transaction's `statement_timeout` fires) reports failure, so `run_loop` withholds the
/// healthy stamp — readiness stays red after `STALL_MAX` — and a later healthy pass of the
/// SAME loop stamps again and restores it. The clock is advanced past `STALL_MAX` between the
/// seed and the loop, so a loop that stamped despite the failure would read green.
#[tokio::test]
async fn a_pass_with_a_failing_party_withholds_the_stamp_until_a_healthy_pass() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::quiet(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        expire_lease(&fx.pool, &member).await;
        let tx = hold_party_lock(&fx, &party).await;
        let racer = fx.racer(2);

        let liveness = sweep::Liveness::default();
        liveness.mark_pass_ok();
        tokio::time::pause();
        tokio::time::advance(sweep::STALL_MAX + Duration::from_secs(21)).await;
        tokio::time::resume();
        liveness.check(sweep::STALL_MAX).expect_err("stale before the pass");

        let (stop_tx, stop_rx) = watch::channel(false);
        let looped = tokio::spawn(sweep::run_loop(
            racer.svc.clone(),
            liveness.clone(),
            Duration::from_millis(10),
            stop_rx,
        ));
        wait_blocked(&fx.pool, &racer.app).await;
        stop_tx.send(true).unwrap();
        finish("the failing sweep loop", looped).await;
        let err = liveness
            .check(sweep::STALL_MAX)
            .expect_err("a pass with a failing party must not stamp");
        assert!(err.contains("no healthy party sweep pass"), "got {err}");
        assert_eq!(members_of(&fx.pool, &party).await.len(), 2, "the failed sweep removed nobody");

        tx.rollback().await.unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let looped = tokio::spawn(sweep::run_loop(
            racer.svc.clone(),
            liveness.clone(),
            Duration::from_millis(10),
            stop_rx,
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while liveness.check(sweep::STALL_MAX).is_err() {
            assert!(std::time::Instant::now() < deadline, "a healthy pass never stamped");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop_tx.send(true).unwrap();
        finish("the healthy sweep loop", looped).await;
        assert_eq!(members_of(&fx.pool, &party).await, vec![leader.clone()], "the healthy pass swept it");
    }).await;
}

/// Once-wrong branch: a pass that PANICS (here after its commit, resolving handles without a
/// Directory) must flip the sweep to dead — the wrapper's `catch_unwind` — so `/readyz` goes
/// red instead of the loop silently never running again.
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_pass_marks_the_sweep_dead() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        expire_lease(&fx.pool, &member).await;

        let no_directory = Arc::new(Service::new(
            fx.pool.clone(),
            Arc::new(push::Push::new()),
            crate::DEFAULT_LEASE_SECS,
        ));
        let liveness = sweep::Liveness::default();
        let (_stop, rx) = watch::channel(false);
        finish("the panicking sweep pass", sweep::spawn(no_directory, liveness.clone(), rx)).await;
        let err = liveness
            .check(sweep::STALL_MAX)
            .expect_err("a panicking pass must read as dead");
        assert!(err.contains("died"), "got {err}");
        assert_eq!(
            members_of(&fx.pool, &party).await,
            vec![leader.clone()],
            "the pass really ran (and committed) before it panicked"
        );
    }).await;
}

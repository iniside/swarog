//! Concurrency tests on the live database. Each one STAGES its interleaving: a test-owned
//! transaction holds a lock (a party's advisory lock, or `party.parties`), the racers run on
//! their own tagged pools, and `wait_blocked` observes in `pg_stat_activity` that each racer
//! is parked before the lock is released — explicit happens-before, never a sleep.
//!
//! The two tests that hold `ACCESS EXCLUSIVE` on `party.parties` take `DB_LOCK`: while that
//! request is queued, any transaction that holds a `parties` lock and then reads `parties`
//! over ANOTHER connection waits behind it while the holder waits for that transaction — a
//! cycle the database cannot see. No production path spans two connections that way; the
//! only test that does (`no_party_get_before_…`) takes `DB_LOCK` as well.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use opsapi::Status;
use partyapi::{Player as _, NOT_READY, READY};

use crate::service::ALREADY_IN_PARTY;
use crate::tests::*;

async fn leader_count(pool: &sqlx::PgPool, player: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM party.parties WHERE leader_id = $1::uuid")
        .bind(player)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Once-wrong branch: the `23505` mapping in `create`. `create` parks on the PK while another
/// transaction's join of the same player is uncommitted, and when that commits the loser must
/// answer `Conflict` ("already in a party"), not `Internal`, and leave no party of its own.
/// The winner's insert is the production `join_tx`, held uncommitted in a test-owned tx.
#[tokio::test(flavor = "multi_thread")]
async fn create_losing_to_an_uncommitted_join_is_conflict_not_internal() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, player] = fx.players();
        let party = create_party(&fx, &leader).await;
        let mut tx = hold_party_lock(&fx, &party).await;
        fx.svc.store.join_tx(&mut tx, &party, &player, 60).await.unwrap();

        let racer = fx.racer(2);
        let svc = racer.svc.clone();
        let p = player.clone();
        let create = tokio::spawn(async move { svc.create(who(&p)).await });
        wait_blocked(&fx.pool, &racer.app).await;
        tx.commit().await.unwrap();

        let err = create.await.unwrap().expect_err("the PK loser must fail");
        assert_eq!(err.status, Status::Conflict, "23505 must map to Conflict, got {err:?}");
        assert_eq!(err.msg, ALREADY_IN_PARTY);
        assert_eq!(party_of(&fx.pool, &player).await.as_deref(), Some(party.as_str()));
        assert_eq!(leader_count(&fx.pool, &player).await, 0, "the loser created no party");
    }).await;
}

/// Once-wrong branch: the `23505` mapping in `accept` (the other direction). `accept` is held
/// at its party lock, the same player's `create` commits first, and the released accept's
/// `join_tx` insert hits the PK: `Conflict`, with the accept's invite consume rolled back and
/// nothing pushed by the loser.
#[tokio::test(flavor = "multi_thread")]
async fn accept_losing_to_a_create_is_conflict_and_keeps_its_invite() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, player] = fx.players();
        let party = create_party(&fx, &leader).await;
        let invite = invite_id(&fx, &leader, &player).await;
        let tx = hold_party_lock(&fx, &party).await;

        let racer = fx.racer(2);
        let svc = racer.svc.clone();
        let (p, i) = (player.clone(), invite.clone());
        let accept = tokio::spawn(async move { svc.accept(who(&p), i).await });
        wait_blocked(&fx.pool, &racer.app).await;

        let created = fx.svc.create(who(&player)).await.unwrap();
        tx.rollback().await.unwrap();

        let err = accept.await.unwrap().expect_err("the PK loser must fail");
        assert_eq!(err.status, Status::Conflict, "23505 must map to Conflict, got {err:?}");
        assert_eq!(err.msg, ALREADY_IN_PARTY);
        assert!(invite_exists(&fx.pool, &invite).await, "the failed accept rolled its consume back");
        assert_eq!(party_of(&fx.pool, &player).await.as_deref(), Some(created.party_id.as_str()));
        assert_eq!(fx.sink.states_to(&leader).len(), 1, "the loser pushed nothing to the party it failed to join");
    }).await;
}

/// Once-wrong branch: the advisory-lock capacity gate. Two accepts, each past its invite
/// check, are both parked on a 7-member party's lock; released, exactly ONE is admitted
/// (the count is taken under the lock) and the other is `Conflict` ("party is full"). Without
/// the lock both would count 7 and the party would reach 9.
#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_accepts_into_a_seven_member_party_admit_exactly_one() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let leader = fx.player();
        let party = create_party(&fx, &leader).await;
        for _ in 0..6 {
            let p = fx.player();
            join(&fx, &leader, &p).await;
        }
        assert_eq!(members_of(&fx.pool, &party).await.len(), 7);
        let [x, y] = fx.players();
        let invite_x = invite_id(&fx, &leader, &x).await;
        let invite_y = invite_id(&fx, &leader, &y).await;
        let tx = hold_party_lock(&fx, &party).await;

        let (rx, ry) = (fx.racer(2), fx.racer(2));
        let (svc_x, svc_y) = (rx.svc.clone(), ry.svc.clone());
        let (px, ix) = (x.clone(), invite_x.clone());
        let (py, iy) = (y.clone(), invite_y.clone());
        let hx = tokio::spawn(async move { svc_x.accept(who(&px), ix).await });
        let hy = tokio::spawn(async move { svc_y.accept(who(&py), iy).await });
        wait_blocked(&fx.pool, &rx.app).await;
        wait_blocked(&fx.pool, &ry.app).await;
        fx.sink.clear();
        tx.rollback().await.unwrap();

        let results = [hx.await.unwrap(), hy.await.unwrap()];
        let admitted: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
        assert_eq!(admitted.len(), 1, "exactly one accept is admitted: {results:?}");
        let refused = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert_eq!(refused.status, Status::Conflict);
        assert!(refused.msg.contains("full"), "got {}", refused.msg);
        assert_eq!(members_of(&fx.pool, &party).await.len(), 8, "the cap held under concurrency");
        let kept = [invite_exists(&fx.pool, &invite_x).await, invite_exists(&fx.pool, &invite_y).await];
        assert_eq!(kept.iter().filter(|k| **k).count(), 1, "only the loser's invite survives");
        assert_eq!(fx.sink.total(), 8, "one state push per member, from the winner only");
    }).await;
}

/// Once-wrong branch: join stamped BELOW a concurrent leave (the nextval-ordering branch).
/// The leave has DELETEd the player's row and is parked at its leader-pick SELECT — BEFORE it
/// draws its revision — while the same player's `create` is parked on the PK behind that
/// uncommitted DELETE. A `create` that drew `nextval` before its INSERT would have drawn
/// first and finished below the leave; drawn after the INSERT, it waits out the leave and
/// stamps strictly above it. The client rule then ends on the new party in either arrival order.
#[tokio::test(flavor = "multi_thread")]
async fn leave_x_racing_create_stamps_the_create_above_the_leave() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, player] = fx.players();
        let party_x = create_party(&fx, &leader).await;
        join(&fx, &leader, &player).await;
        fx.sink.clear();

        let gate = hold_parties_table(&fx.pool).await;
        let (rl, rc) = (fx.racer(2), fx.racer(2));
        let (svc_l, p) = (rl.svc.clone(), player.clone());
        let leave = tokio::spawn(async move { svc_l.leave(who(&p)).await });
        wait_blocked(&fx.pool, &rl.app).await;
        let (svc_c, p) = (rc.svc.clone(), player.clone());
        let create = tokio::spawn(async move { svc_c.create(who(&p)).await });
        wait_blocked(&fx.pool, &rc.app).await;
        gate.commit().await.unwrap();

        let left = leave.await.unwrap().unwrap();
        let created = create.await.unwrap().unwrap();
        assert!(is_no_party(&left));
        assert!(
            created.revision > left.revision,
            "the join ({}) must be stamped above the concurrent leave ({})",
            created.revision,
            left.revision
        );
        assert_eq!(player_rev(&fx.pool, &player).await, Some(created.revision), "the join is the player's latest stamp");
        assert_eq!(party_row(&fx.pool, &party_x).await.unwrap(), (left.revision, leader.clone()));

        let seen = fx.sink.states_to(&player);
        assert_eq!(seen.len(), 2);
        for order in [seen.clone(), seen.into_iter().rev().collect()] {
            assert_eq!(apply_client_rule(order).unwrap().party_id, created.party_id);
        }
    }).await;
}

/// Once-wrong branch: the same ordering for `accept` into Y while the player leaves X: the
/// accept's `join_tx` INSERT parks behind the leave's uncommitted DELETE of the player's row,
/// so its revision is drawn after the leave's and lands strictly above it.
#[tokio::test(flavor = "multi_thread")]
async fn leave_x_racing_accept_into_y_stamps_the_join_above_the_leave() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader_x, leader_y, player] = fx.players();
        let party_x = create_party(&fx, &leader_x).await;
        let party_y = create_party(&fx, &leader_y).await;
        join(&fx, &leader_x, &player).await;
        let invite = invite_id(&fx, &leader_y, &player).await;
        fx.sink.clear();

        let gate = hold_parties_table(&fx.pool).await;
        let (rl, ra) = (fx.racer(2), fx.racer(2));
        let (svc_l, p) = (rl.svc.clone(), player.clone());
        let leave = tokio::spawn(async move { svc_l.leave(who(&p)).await });
        wait_blocked(&fx.pool, &rl.app).await;
        let (svc_a, p) = (ra.svc.clone(), player.clone());
        let accept = tokio::spawn(async move { svc_a.accept(who(&p), invite).await });
        wait_blocked(&fx.pool, &ra.app).await;
        gate.commit().await.unwrap();

        let left = leave.await.unwrap().unwrap();
        let joined = accept.await.unwrap().unwrap();
        assert!(
            joined.revision > left.revision,
            "the join ({}) must be stamped above the concurrent leave ({})",
            joined.revision,
            left.revision
        );
        assert_eq!(joined.party_id, party_y);
        assert_eq!(members_of(&fx.pool, &party_x).await, vec![leader_x.clone()]);
        assert_eq!(player_rev(&fx.pool, &player).await, Some(joined.revision));
        let seen = fx.sink.states_to(&player);
        for order in [seen.clone(), seen.into_iter().rev().collect()] {
            assert_eq!(apply_client_rule(order).unwrap().party_id, party_y);
        }
    }).await;
}

/// Once-wrong branch: `leave`'s retry. A caller whose first read shows no party but who joins
/// before the second read must be REMOVED from the party just joined and answered the
/// "no party" view stamped with that removal's revision — never a party view, never a stale
/// no-party answer while still a member. The racer's pool has one connection and its second
/// checkout (the `player_view` read) is paused until the accept commits.
#[tokio::test(flavor = "multi_thread")]
async fn leave_in_no_party_racing_an_accept_retries_and_removes_the_player() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, player] = fx.players();
        let party = create_party(&fx, &leader).await;
        let invite = invite_id(&fx, &leader, &player).await;

        let gate = Gate::new();
        let racer = fx.racer_with(1, Some(gate.clone()));
        let (svc, p) = (racer.svc.clone(), player.clone());
        let leave = tokio::spawn(async move { svc.leave(who(&p)).await });
        tokio::time::timeout(std::time::Duration::from_secs(30), gate.reached.notified())
            .await
            .expect("leave never reached its second read");

        let joined = fx.svc.accept(who(&player), invite).await.unwrap();
        assert_eq!(joined.party_id, party);
        gate.go.notify_one();

        let left = leave.await.unwrap().unwrap();
        assert!(is_no_party(&left), "leave never answers a party view: {left:?}");
        assert!(left.revision > joined.revision, "the answer carries the removal's own revision");
        assert!(party_of(&fx.pool, &player).await.is_none(), "the player was removed from the party just joined");
        assert_eq!(members_of(&fx.pool, &party).await, vec![leader.clone()]);
        assert_eq!(party_row(&fx.pool, &party).await.unwrap(), (left.revision, leader.clone()));
        assert!(
            fx.sink.states_to(&player).iter().any(|s| is_no_party(s) && s.revision == left.revision),
            "the removal pushed the no-party view at its revision"
        );
        assert_eq!(apply_client_rule(fx.sink.states_to(&player)).unwrap().revision, left.revision);
    }).await;
}

/// Once-wrong branch: `begin_locked`'s re-read under the lock. `set_ready` reads its party
/// (X), parks on X's lock, and by the time it gets the lock the player has left X and joined
/// Y. It must notice, roll back and retry against Y; applying X's write would be an
/// `Internal` "membership vanished".
#[tokio::test(flavor = "multi_thread")]
async fn a_member_who_changes_party_while_waiting_for_the_lock_is_retried_against_the_new_party() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader_x, leader_y, player] = fx.players();
        let party_x = create_party(&fx, &leader_x).await;
        let party_y = create_party(&fx, &leader_y).await;
        join(&fx, &leader_x, &player).await;

        let mut tx = hold_party_lock(&fx, &party_x).await;
        let racer = fx.racer(2);
        let (svc, p) = (racer.svc.clone(), player.clone());
        let set_ready = tokio::spawn(async move { svc.set_ready(who(&p), READY.into()).await });
        wait_blocked(&fx.pool, &racer.app).await;

        fx.svc.store.remove_member_tx(&mut tx, &party_x, &player).await.unwrap().unwrap();
        fx.svc.store.lock_party_tx(&mut tx, &party_y).await.unwrap();
        fx.svc.store.join_tx(&mut tx, &party_y, &player, 60).await.unwrap();
        tx.commit().await.unwrap();

        let view = set_ready.await.unwrap().expect("the retry must apply to the new party");
        assert_eq!(view.party_id, party_y);
        assert_eq!(view.members.iter().find(|m| m.player_id == player).unwrap().ready, READY);
        assert_eq!(members_of(&fx.pool, &party_x).await, vec![leader_x.clone()]);
    }).await;
}

/// Once-wrong branch: snapshot atomicity of the view statement. While one member's
/// `set_ready` toggles continuously, concurrent readers load the party; every view must pair
/// its `revision` with the ready state THAT revision committed — a torn read (new revision,
/// old state) would map one revision to two states. Reader revisions are also monotone.
///
/// Stress, not a staged interleaving: a torn read cannot be interposed between the
/// statements of a single SQL statement, so this detects a regression to a multi-statement
/// view probabilistically (recorded as a known gap), while the equal-revision invariant is
/// exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_view_never_pairs_a_revision_with_another_revisions_ready_state() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, subject] = fx.players();
        create_party(&fx, &leader).await;
        let joined = join(&fx, &leader, &subject).await;
        let mut truth: HashMap<i64, String> = HashMap::new();
        truth.insert(joined.revision, NOT_READY.to_string());

        let writer = fx.racer(2);
        let readers = fx.racer(4);
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(i64, String)>::new()));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let (svc, done, observed) = (readers.svc.clone(), done.clone(), observed.clone());
            let (leader, subject) = (leader.clone(), subject.clone());
            handles.push(tokio::spawn(async move {
                let mut last = 0;
                while !done.load(Ordering::SeqCst) {
                    let view = svc.get(who(&leader)).await.unwrap();
                    assert!(view.revision >= last, "a reader's revision went backwards");
                    last = view.revision;
                    let ready = view.members.iter().find(|m| m.player_id == subject).unwrap().ready.clone();
                    observed.lock().unwrap().push((view.revision, ready));
                }
            }));
        }
        for i in 0..200 {
            let state = if i % 2 == 0 { READY } else { NOT_READY };
            let view = writer.svc.set_ready(who(&subject), state.into()).await.unwrap();
            let seen = &view.members.iter().find(|m| m.player_id == subject).unwrap().ready;
            assert_eq!(seen, state, "the writer reads its own committed state");
            truth.insert(view.revision, state.to_string());
        }
        done.store(true, Ordering::SeqCst);
        for h in handles {
            h.await.unwrap();
        }
        let observed = observed.lock().unwrap();
        assert!(!observed.is_empty(), "the readers observed nothing");
        for (revision, ready) in observed.iter() {
            assert_eq!(
                truth.get(revision),
                Some(ready),
                "revision {revision} was observed with ready={ready}, but it committed {:?}",
                truth.get(revision)
            );
        }
    }).await;
}

/// Control for the stress above and for the leave/join ordering tests: the same racers
/// staged WITHOUT any held lock would interleave arbitrarily, so the staging helpers
/// themselves are proven to hold — a racer parked on a held party lock really is blocked
/// (Lock wait) and really proceeds only on release.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_party_lock_parks_a_racer_until_it_is_released() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        let tx = hold_party_lock(&fx, &party).await;
        let racer = fx.racer(2);
        let (svc, p) = (racer.svc.clone(), member.clone());
        let ready = tokio::spawn(async move { svc.set_ready(who(&p), READY.into()).await });
        wait_blocked(&fx.pool, &racer.app).await;
        assert!(!ready.is_finished(), "set_ready must not finish while the party lock is held");
        tx.rollback().await.unwrap();
        ready.await.unwrap().unwrap();
    }).await;
}

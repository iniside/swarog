//! Sequential `Service`-level tests over the live database: every rule of the contract, the
//! revision/push behaviour of each stamping and non-stamping op, and the post-commit
//! Directory behaviour. Each test names the branch that used to be (or could be) wrong.

use opsapi::Status;
use partyapi::{Player as _, MAX_MEMBERS, NOT_READY, READY};

use crate::service::ALREADY_IN_PARTY;
use crate::tests::*;

const UNKNOWN_UUID: &str = "11111111-1111-4111-8111-111111111111";

/// Once-wrong branch: the leader gate. A member who does not lead gets `NotFound` (never
/// `Forbidden` — a 403 confirms the party is real) from invite/kick/promote, and the invite
/// is refused BEFORE the directory is asked, so a non-leader cannot probe handle existence.
#[tokio::test(flavor = "multi_thread")]
async fn non_leader_invite_kick_promote_are_not_found_and_invite_never_touches_the_directory() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, target, loner] = fx.players();
        create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        let before = fx.dir.by_handle();

        let invited = fx.svc.invite(who(&member), fx.handle(&target)).await;
        assert_status(invited, Status::NotFound, "member invite");
        assert_eq!(fx.dir.by_handle(), before, "a non-leader invite must not resolve the handle");
        let loner_invite = fx.svc.invite(who(&loner), fx.handle(&target)).await;
        assert_status(loner_invite, Status::NotFound, "no-party invite");
        assert_eq!(fx.dir.by_handle(), before);

        assert_status(fx.svc.kick(who(&member), leader.clone()).await, Status::NotFound, "member kick");
        assert_status(fx.svc.promote(who(&member), leader.clone()).await, Status::NotFound, "member promote");
        assert_status(fx.svc.kick(who(&loner), leader.clone()).await, Status::NotFound, "no-party kick");
        assert_status(fx.svc.promote(who(&loner), member.clone()).await, Status::NotFound, "no-party promote");
    }).await;
}

/// Once-wrong branch: `remove_member_tx` is scoped to the LEADER'S party — a leader cannot
/// kick a member of another party; a non-member target of kick/promote is `NotFound`.
#[tokio::test(flavor = "multi_thread")]
async fn kick_and_promote_of_a_foreign_or_non_member_are_not_found() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader_a, leader_b, member_b, stranger] = fx.players();
        create_party(&fx, &leader_a).await;
        let party_b = create_party(&fx, &leader_b).await;
        join(&fx, &leader_b, &member_b).await;

        assert_status(fx.svc.kick(who(&leader_a), member_b.clone()).await, Status::NotFound, "cross-party kick");
        assert_status(fx.svc.promote(who(&leader_a), member_b.clone()).await, Status::NotFound, "cross-party promote");
        assert_status(fx.svc.kick(who(&leader_a), stranger.clone()).await, Status::NotFound, "kick non-member");
        assert_status(fx.svc.promote(who(&leader_a), stranger.clone()).await, Status::NotFound, "promote non-member");
        assert_eq!(members_of(&fx.pool, &party_b).await.len(), 2, "the foreign party is untouched");
    }).await;
}

/// Once-wrong branch: invite ownership and expiry. Another player's invite and an expired
/// invite are both `NotFound` for accept; a foreign decline is `NotFound` and leaves the
/// invite; a malformed id is `Invalid`; an unknown well-formed id is `NotFound`.
#[tokio::test(flavor = "multi_thread")]
async fn foreign_expired_and_malformed_invites() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, invitee, other] = fx.players();
        create_party(&fx, &leader).await;
        let invite = invite_id(&fx, &leader, &invitee).await;

        assert_status(fx.svc.accept(who(&other), invite.clone()).await, Status::NotFound, "foreign accept");
        assert_status(fx.svc.decline(who(&other), invite.clone()).await, Status::NotFound, "foreign decline");
        assert!(invite_exists(&fx.pool, &invite).await, "a foreign decline must not delete the invite");
        assert_status(fx.svc.accept(who(&invitee), UNKNOWN_UUID.to_string()).await, Status::NotFound, "unknown invite");
        assert_status(fx.svc.accept(who(&invitee), "not-a-uuid".into()).await, Status::Invalid, "malformed accept");
        assert_status(fx.svc.decline(who(&invitee), "not-a-uuid".into()).await, Status::Invalid, "malformed decline");
        assert_status(fx.svc.kick(who(&leader), "not-a-uuid".into()).await, Status::Invalid, "malformed kick");
        assert_status(fx.svc.promote(who(&leader), "not-a-uuid".into()).await, Status::Invalid, "malformed promote");

        expire_invite(&fx.pool, &invite).await;
        assert_status(fx.svc.accept(who(&invitee), invite.clone()).await, Status::NotFound, "expired accept");
        assert!(fx.svc.invites(who(&invitee)).await.unwrap().invites.is_empty(), "an expired invite is not listed");
        assert!(party_of(&fx.pool, &invitee).await.is_none());
    }).await;
}

/// Once-wrong branch: invite target rules — self → `Invalid`, an existing member →
/// `Conflict`, an unknown handle → `NotFound`, a Directory outage → `Unavailable`, and a
/// handle over `MAX_HANDLE_BYTES` → `Invalid` before any directory call, even for a caller
/// who could not invite at all (the cap precedes the leader gate).
#[tokio::test(flavor = "multi_thread")]
async fn invite_target_rules() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, loner] = fx.players();
        create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;

        assert_status(fx.svc.invite(who(&leader), fx.handle(&leader)).await, Status::Invalid, "self invite");
        assert_status(fx.svc.invite(who(&leader), fx.handle(&member)).await, Status::Conflict, "invite of a member");
        assert_status(fx.svc.invite(who(&leader), "Ghost#0000".into()).await, Status::NotFound, "unknown handle");

        let before = fx.dir.by_handle();
        let long = "a".repeat(accountsapi::MAX_HANDLE_BYTES + 1);
        assert_status(fx.svc.invite(who(&leader), long.clone()).await, Status::Invalid, "over-cap handle");
        assert_status(fx.svc.invite(who(&loner), long).await, Status::Invalid, "over-cap handle, no party");
        assert_eq!(fx.dir.by_handle(), before, "the cap is checked before the directory is asked");

        fx.dir.fail_handle.store(true, std::sync::atomic::Ordering::SeqCst);
        let outage = fx.svc.invite(who(&leader), fx.handle(&loner)).await;
        assert_status(outage, Status::Unavailable, "directory outage on the target");
    }).await;
}

/// Once-wrong branch: the self checks run BEFORE the leader check — a non-leader's (or a
/// no-party caller's) self-kick/self-promote is `Invalid`, not `NotFound`.
#[tokio::test(flavor = "multi_thread")]
async fn self_kick_and_self_promote_are_invalid_before_the_leader_check() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, loner] = fx.players();
        create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        fx.sink.clear();
        for (who_, label) in [(&leader, "leader"), (&member, "non-leader"), (&loner, "no party")] {
            assert_status(fx.svc.kick(who(who_), who_.clone()).await, Status::Invalid, &format!("{label} self-kick"));
            assert_status(fx.svc.promote(who(who_), who_.clone()).await, Status::Invalid, &format!("{label} self-promote"));
        }
        assert_eq!(fx.sink.total(), 0, "a rejected self-op pushes nothing");
    }).await;
}

/// Once-wrong branch: `leave` by a caller in no party is the "no party" view, not an error
/// and not a push; its revision is the caller's own last membership change (0 if none).
#[tokio::test(flavor = "multi_thread")]
async fn leave_with_no_party_answers_the_no_party_view_with_the_players_own_revision() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let virgin = fx.svc.leave(who(&member)).await.unwrap();
        assert!(is_no_party(&virgin));
        assert_eq!(virgin.revision, 0, "a player who never changed membership has revision 0");

        create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        let left = fx.svc.leave(who(&member)).await.unwrap();
        assert!(is_no_party(&left));
        fx.sink.clear();
        let again = fx.svc.leave(who(&member)).await.unwrap();
        assert!(is_no_party(&again));
        assert_eq!(again.revision, left.revision, "no-party revision is the leave's own stamp");
        assert_eq!(player_rev(&fx.pool, &member).await, Some(left.revision));
        assert_eq!(fx.sink.total(), 0, "a leave in no party pushes nothing");
    }).await;
}

/// Once-wrong branch: `set_ready` in no party is `NotFound`; an unknown `state` is `Invalid`
/// (checked before the membership read).
#[tokio::test(flavor = "multi_thread")]
async fn set_ready_validates_state_and_requires_a_party() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, loner] = fx.players();
        create_party(&fx, &leader).await;
        assert_status(fx.svc.set_ready(who(&loner), READY.into()).await, Status::NotFound, "no party");
        assert_status(fx.svc.set_ready(who(&loner), "maybe".into()).await, Status::Invalid, "bad state, no party");
        assert_status(fx.svc.set_ready(who(&leader), "maybe".into()).await, Status::Invalid, "bad state");
        assert_status(fx.svc.set_ready(who(&leader), "READY".into()).await, Status::Invalid, "state is case-sensitive");
    }).await;
}

/// Once-wrong branch: `create` by a member answers `Conflict` ("already in a party") through
/// the `members` primary key, and the loser's call changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn create_while_in_a_party_is_conflict() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        for p in [&leader, &member] {
            let err = fx.svc.create(who(p)).await.unwrap_err();
            assert_eq!(err.status, Status::Conflict);
            assert_eq!(err.msg, ALREADY_IN_PARTY);
        }
        assert_eq!(party_of(&fx.pool, &member).await.as_deref(), Some(party.as_str()));
    }).await;
}

/// Once-wrong branch: the `MAX_MEMBERS` cap and its rollback. The ninth player is `Conflict`
/// and the invite SURVIVES (the consume rolled back with the tx), so the player can accept
/// once a slot frees; accept is single-use.
#[tokio::test(flavor = "multi_thread")]
async fn accept_into_a_full_party_is_conflict_keeps_the_invite_and_a_free_slot_admits() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let leader = fx.player();
        let party = create_party(&fx, &leader).await;
        for _ in 1..MAX_MEMBERS - 1 {
            let p = fx.player();
            join(&fx, &leader, &p).await;
        }
        let [late, last] = fx.players();
        let late_invite = invite_id(&fx, &leader, &late).await;
        join(&fx, &leader, &last).await;
        assert_eq!(members_of(&fx.pool, &party).await.len(), MAX_MEMBERS as usize);

        fx.sink.clear();
        let err = fx.svc.accept(who(&late), late_invite.clone()).await.unwrap_err();
        assert_eq!(err.status, Status::Conflict);
        assert!(err.msg.contains("full"), "got {}", err.msg);
        assert!(invite_exists(&fx.pool, &late_invite).await, "the rolled-back accept keeps its invite");
        assert!(party_of(&fx.pool, &late).await.is_none());
        assert_eq!(fx.sink.total(), 0, "a rolled-back accept pushes nothing");

        fx.svc.leave(who(&last)).await.unwrap();
        let joined = fx.svc.accept(who(&late), late_invite.clone()).await.unwrap();
        assert_eq!(joined.members.len(), MAX_MEMBERS as usize);
        assert!(!invite_exists(&fx.pool, &late_invite).await, "accept consumes the invite");
        assert_status(fx.svc.accept(who(&late), late_invite).await, Status::NotFound, "second accept");
    }).await;
}

/// Once-wrong branch: the invite upsert — re-inviting the same player keeps ONE row with the
/// SAME id and a refreshed TTL, and each invite is pushed to the invitee.
#[tokio::test(flavor = "multi_thread")]
async fn reinviting_reuses_the_invite_row_and_refreshes_the_ttl() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, target] = fx.players();
        let party = create_party(&fx, &leader).await;
        let first = fx.svc.invite(who(&leader), fx.handle(&target)).await.unwrap();
        sqlx::query("UPDATE party.invites SET expires_at = now() + interval '5 seconds' WHERE id = $1::uuid")
            .bind(&first.invite_id)
            .execute(&fx.pool)
            .await
            .unwrap();
        let second = fx.svc.invite(who(&leader), fx.handle(&target)).await.unwrap();
        assert_eq!(second.invite_id, first.invite_id);
        assert_eq!(invite_count_for_party(&fx.pool, &party).await, 1);
        let listed = fx.svc.invites(who(&target)).await.unwrap().invites;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].invite_id, first.invite_id);
        assert_eq!(listed[0].inviter_handle, fx.handle(&leader));
        let ttl: f64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM (expires_at - now()))::float8 FROM party.invites WHERE id = $1::uuid",
        )
        .bind(&second.invite_id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert!(ttl > 100.0, "the reissue must refresh the TTL, got {ttl}s");
        assert_eq!(fx.sink.invites_to(&target).len(), 2, "each invite is pushed to the invitee");
    }).await;
}

/// Once-wrong branch: handoff order. A non-leader leaving keeps the leader; a leader leaving
/// hands over to the earliest `(joined_at, player_id)` — including the `player_id` tie-break
/// when two members share a `joined_at`; the last member leaving deletes the party AND its
/// pending invites.
#[tokio::test(flavor = "multi_thread")]
async fn leader_handoff_goes_to_the_earliest_joined_and_the_last_leave_dissolves() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        // Ids descend in join order: the earliest-joined member holds the LARGEST id, so a
        // handoff that ordered by player_id alone would pick the wrong member.
        let [leader, a, b, c] = fx.players_by_id_desc();
        let party = create_party(&fx, &leader).await;
        for p in [&a, &b, &c] {
            join(&fx, &leader, p).await;
        }
        fx.svc.leave(who(&c)).await.unwrap();
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().1, leader, "a member leaving keeps the leader");

        fx.svc.leave(who(&leader)).await.unwrap();
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().1, a, "earliest joined leads");

        // Tie on joined_at: the smaller player_id must win the handoff, although the larger
        // id joined (and was inserted) first, so scan order and id order disagree.
        let [lead, high, low, z] = fx.players_by_id_desc();
        assert!(high > low);
        let tie_party = create_party(&fx, &lead).await;
        join(&fx, &lead, &high).await;
        join(&fx, &lead, &low).await;
        sqlx::query(
            "UPDATE party.members SET joined_at = '2030-01-01T00:00:00Z' \
              WHERE party_id = $1::uuid AND player_id <> $2::uuid",
        )
        .bind(&tie_party)
        .bind(&lead)
        .execute(&fx.pool)
        .await
        .unwrap();
        let z_invite = invite_id(&fx, &lead, &z).await;
        fx.svc.leave(who(&lead)).await.unwrap();
        assert_eq!(party_row(&fx.pool, &tie_party).await.unwrap().1, low, "tie on joined_at breaks on player_id");

        fx.svc.leave(who(&low)).await.unwrap();
        assert_eq!(party_row(&fx.pool, &tie_party).await.unwrap().1, high);
        fx.svc.leave(who(&high)).await.unwrap();
        assert!(party_row(&fx.pool, &tie_party).await.is_none(), "the last leave deletes the party");
        assert!(members_of(&fx.pool, &tie_party).await.is_empty());
        assert!(!invite_exists(&fx.pool, &z_invite).await, "the last leave deletes the party's invites");
        assert_status(fx.svc.accept(who(&z), z_invite).await, Status::NotFound, "accept into a dissolved party");
    }).await;
}

/// Once-wrong branch: the `(m.player_id = p.leader_id) DESC` key of the leave rule. When a
/// promoted member (NOT the earliest joined) leads, every removal path — a non-leader leaving,
/// a kick, a lease expiry swept — must leave that member the leader; without the key the
/// earliest remaining member by `(joined_at, player_id)` would be picked instead.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_non_leader_keeps_a_promoted_leader_on_every_path() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        for path in ["leave", "kick", "sweep"] {
            let [original, gone, promoted] = fx.players();
            let party = create_party(&fx, &original).await;
            join(&fx, &original, &gone).await;
            join(&fx, &original, &promoted).await;
            let view = fx.svc.promote(who(&original), promoted.clone()).await.unwrap();
            assert_eq!(view.leader_id, promoted);

            match path {
                "leave" => {
                    fx.svc.leave(who(&gone)).await.unwrap();
                }
                "kick" => {
                    fx.svc.kick(who(&promoted), gone.clone()).await.unwrap();
                }
                _ => {
                    expire_lease(&fx.pool, &gone).await;
                    assert!(fx.svc.sweep_party(&party).await.unwrap());
                }
            }
            assert_eq!(members_of(&fx.pool, &party).await, vec![original.clone(), promoted.clone()], "{path}");
            assert_eq!(
                party_row(&fx.pool, &party).await.unwrap().1,
                promoted,
                "{path}: removing a non-leader must not move leadership to the earliest joined"
            );
        }
    }).await;
}

/// Once-wrong branch: kick stamps ONE new revision and fans it out — remaining members get
/// the new `party.state`, the kicked player gets a "no party" view stamped with the SAME
/// revision (and that is their `player_revisions`), with ONE directory call for the op.
#[tokio::test(flavor = "multi_thread")]
async fn kick_stamps_pushes_state_to_the_rest_and_no_party_to_the_kicked() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, a, b] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &a).await;
        let before = join(&fx, &leader, &b).await.revision;
        fx.sink.clear();
        let calls = fx.dir.by_id();

        let view = fx.svc.kick(who(&leader), a.clone()).await.unwrap();
        assert!(view.revision > before);
        assert_eq!(view.party_id, party);
        assert_eq!(view.members.iter().map(|m| m.player_id.clone()).collect::<Vec<_>>(), vec![leader.clone(), b.clone()]);
        assert_eq!(fx.dir.by_id() - calls, 1, "one batched directory call per op");
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().0, view.revision);

        let kicked = fx.sink.states_to(&a);
        assert_eq!(kicked.len(), 1);
        assert!(is_no_party(&kicked[0]));
        assert_eq!(kicked[0].revision, view.revision);
        assert_eq!(player_rev(&fx.pool, &a).await, Some(view.revision));
        for p in [&leader, &b] {
            let pushed = fx.sink.states_to(p);
            assert_eq!(pushed.len(), 1);
            assert_eq!(pushed[0], view);
        }
        assert_eq!(fx.sink.total(), 3);
    }).await;
}

/// Once-wrong branch: promote and set_ready are STAMPING ops — each takes a new revision,
/// changes exactly what it claims, and pushes the new snapshot to every member.
#[tokio::test(flavor = "multi_thread")]
async fn promote_and_set_ready_stamp_a_new_revision_and_push_to_every_member() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        let joined = join(&fx, &leader, &member).await;
        fx.sink.clear();

        let promoted = fx.svc.promote(who(&leader), member.clone()).await.unwrap();
        assert!(promoted.revision > joined.revision);
        assert_eq!(promoted.leader_id, member);
        assert_eq!(party_row(&fx.pool, &party).await.unwrap(), (promoted.revision, member.clone()));
        for p in [&leader, &member] {
            let pushed = fx.sink.states_to(p);
            assert_eq!(pushed.len(), 1, "promote pushes once to each member");
            assert_eq!(pushed[0].revision, promoted.revision);
            assert_eq!(pushed[0].leader_id, member);
        }

        fx.sink.clear();
        let ready = fx.svc.set_ready(who(&leader), READY.into()).await.unwrap();
        assert!(ready.revision > promoted.revision);
        let mine = ready.members.iter().find(|m| m.player_id == leader).unwrap();
        assert_eq!(mine.ready, READY);
        let theirs = ready.members.iter().find(|m| m.player_id == member).unwrap();
        assert_eq!(theirs.ready, NOT_READY);
        for p in [&leader, &member] {
            let pushed = fx.sink.states_to(p);
            assert_eq!(pushed.len(), 1);
            assert_eq!(pushed[0].revision, ready.revision);
        }
        let unready = fx.svc.set_ready(who(&leader), NOT_READY.into()).await.unwrap();
        assert!(unready.revision > ready.revision, "every set_ready stamps, even back to the same state");
    }).await;
}

/// Once-wrong branch: heartbeat is NOT a stamping op. It returns the party's CURRENT view
/// revision and a lease, never bumps `parties.revision`, never pushes; in no party it answers
/// `lease_until == ""` and the player's own revision.
#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_returns_the_current_revision_and_never_stamps_or_pushes() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, loner] = fx.players();
        let party = create_party(&fx, &leader).await;
        let joined = join(&fx, &leader, &member).await;
        fx.sink.clear();

        for p in [&leader, &member] {
            let beat = fx.svc.heartbeat(who(p)).await.unwrap();
            assert_eq!(beat.revision, joined.revision);
            assert!(beat.lease_until.ends_with('Z') && !beat.lease_until.is_empty());
        }
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().0, joined.revision, "heartbeat never stamps");
        assert_eq!(fx.sink.total(), 0, "heartbeat never pushes");

        let none = fx.svc.heartbeat(who(&loner)).await.unwrap();
        assert_eq!(none.lease_until, "");
        assert_eq!(none.revision, 0);
        fx.svc.leave(who(&member)).await.unwrap();
        let left = fx.svc.heartbeat(who(&member)).await.unwrap();
        assert_eq!(left.lease_until, "");
        assert_eq!(left.revision, player_rev(&fx.pool, &member).await.unwrap());
    }).await;
}

/// Once-wrong branch: `get`/`invites`/`decline` are non-stamping, non-pushing reads that
/// still renew the lease; and EVERY member op (the stamping ones too) renews it.
#[tokio::test(flavor = "multi_thread")]
async fn every_member_op_renews_the_lease_and_reads_never_stamp_or_push() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, victim, target] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        join(&fx, &leader, &victim).await;
        let stamped = party_row(&fx.pool, &party).await.unwrap().0;
        fx.sink.clear();

        set_lease_near_expiry(&fx.pool, &member).await;
        fx.svc.get(who(&member)).await.unwrap();
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "get renews");
        set_lease_near_expiry(&fx.pool, &member).await;
        fx.svc.heartbeat(who(&member)).await.unwrap();
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "heartbeat renews");
        set_lease_near_expiry(&fx.pool, &member).await;
        fx.svc.invites(who(&member)).await.unwrap();
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "invites renews");
        set_lease_near_expiry(&fx.pool, &member).await;
        let _ = fx.svc.decline(who(&member), UNKNOWN_UUID.into()).await;
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "decline renews");
        assert_eq!(party_row(&fx.pool, &party).await.unwrap().0, stamped, "no read stamps");
        assert_eq!(fx.sink.total(), 0, "no read pushes");

        set_lease_near_expiry(&fx.pool, &member).await;
        fx.svc.set_ready(who(&member), READY.into()).await.unwrap();
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "set_ready renews");
        set_lease_near_expiry(&fx.pool, &leader).await;
        fx.svc.invite(who(&leader), fx.handle(&target)).await.unwrap();
        assert!(lease_remaining(&fx.pool, &leader).await > 50.0, "invite renews the leader");
        set_lease_near_expiry(&fx.pool, &leader).await;
        fx.svc.promote(who(&leader), member.clone()).await.unwrap();
        assert!(lease_remaining(&fx.pool, &leader).await > 50.0, "promote renews the caller");
        set_lease_near_expiry(&fx.pool, &member).await;
        fx.svc.kick(who(&member), victim.clone()).await.unwrap();
        assert!(lease_remaining(&fx.pool, &member).await > 50.0, "kick renews the caller");
    }).await;
}

/// Once-wrong branch: the "no party" view's revision. A `get` answered before an `accept`
/// commits carries the player's previous revision, STRICTLY below the accept's; and leaving
/// X then accepting into Y gives a Y snapshot above the "left X" revision. The accept is
/// held uncommitted (the production `join_tx` in a test-owned tx) so the read provably
/// precedes its commit.
#[tokio::test(flavor = "multi_thread")]
async fn no_party_get_before_a_concurrent_accept_is_strictly_below_the_accepts_revision() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader_x, leader_y, player] = fx.players();
        create_party(&fx, &leader_x).await;
        let party_y = create_party(&fx, &leader_y).await;
        join(&fx, &leader_x, &player).await;
        let left_x = fx.svc.leave(who(&player)).await.unwrap();
        assert!(is_no_party(&left_x));

        let mut tx = hold_party_lock(&fx, &party_y).await;
        let accept_revision = fx.svc.store.join_tx(&mut tx, &party_y, &player, 60).await.unwrap();

        let stale = fx.svc.get(who(&player)).await.unwrap();
        assert!(is_no_party(&stale), "the uncommitted join is invisible");
        assert_eq!(stale.revision, left_x.revision);
        assert!(stale.revision < accept_revision, "no-party revision is strictly below the accept's");
        tx.commit().await.unwrap();

        let after = fx.svc.get(who(&player)).await.unwrap();
        assert_eq!(after.party_id, party_y);
        assert_eq!(after.revision, accept_revision);
        assert!(apply_client_rule([after.clone(), stale.clone()]).unwrap().party_id == party_y,
            "a stale no-party answer arriving after the join loses to the client rule");
    }).await;
}

/// Once-wrong branch: sequential ordering — leave X then accept into Y puts the Y snapshot
/// strictly above the "left X" revision, so a client applying both in order ends in Y.
#[tokio::test(flavor = "multi_thread")]
async fn leave_x_then_accept_into_y_gives_a_higher_snapshot_revision() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader_x, leader_y, player] = fx.players();
        create_party(&fx, &leader_x).await;
        create_party(&fx, &leader_y).await;
        join(&fx, &leader_x, &player).await;
        let left = fx.svc.leave(who(&player)).await.unwrap();
        let joined = join(&fx, &leader_y, &player).await;
        assert!(joined.revision > left.revision);
        assert_eq!(player_rev(&fx.pool, &player).await, Some(joined.revision));
        let seen = apply_client_rule(fx.sink.states_to(&player)).unwrap();
        assert_eq!(seen.party_id, joined.party_id);
    }).await;
}

/// Once-wrong branch: the post-commit Directory failure. The mutation committed, so the op
/// still succeeds with EMPTY handles in both the response and the push (failing it would make
/// a retry answer `Conflict`); a `get` and an accept degrade the same way; and the response
/// costs exactly ONE `players_by_id` call.
#[tokio::test(flavor = "multi_thread")]
async fn a_directory_failure_after_commit_still_succeeds_with_empty_handles() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let invite = {
            create_party(&fx, &leader).await;
            invite_id(&fx, &leader, &member).await
        };
        fx.dir.fail_lookup.store(true, std::sync::atomic::Ordering::SeqCst);
        fx.sink.clear();
        let calls = fx.dir.by_id();

        let joined = fx.svc.accept(who(&member), invite).await.expect("the committed accept must succeed");
        assert_eq!(fx.dir.by_id() - calls, 1, "one directory call, which failed");
        assert_eq!(joined.members.len(), 2);
        assert!(joined.members.iter().all(|m| m.handle.is_empty()), "handles are empty, not an error");
        for p in [&leader, &member] {
            let pushed = fx.sink.states_to(p);
            assert_eq!(pushed.len(), 1);
            assert!(pushed[0].members.iter().all(|m| m.handle.is_empty()));
            assert_eq!(pushed[0].revision, joined.revision);
        }

        let got = fx.svc.get(who(&member)).await.unwrap();
        assert_eq!(got.revision, joined.revision);
        assert!(got.members.iter().all(|m| m.handle.is_empty()));

        let [solo] = fx.players();
        let created = fx.svc.create(who(&solo)).await.expect("create commits despite the outage");
        assert_eq!(created.members.len(), 1);
        assert!(created.members[0].handle.is_empty());
        assert_status(fx.svc.create(who(&solo)).await, Status::Conflict, "a retry of the committed create");

        let invited = fx.svc.invite(who(&solo), "x".into()).await;
        assert_status(invited, Status::NotFound, "unknown handle is still a miss, not an outage");

        fx.dir.fail_lookup.store(false, std::sync::atomic::Ordering::SeqCst);
        let [other] = fx.players();
        let ok = fx.svc.invite(who(&solo), fx.handle(&other)).await.unwrap();
        assert_eq!(ok.inviter_handle, fx.handle(&solo));
        fx.dir.fail_lookup.store(true, std::sync::atomic::Ordering::SeqCst);
        let again = fx.svc.invite(who(&solo), fx.handle(&other)).await.expect("invite commits despite a failing hydrate");
        assert_eq!(again.inviter_handle, "", "the inviter handle degrades to empty");
        assert_eq!(again.invite_id, ok.invite_id);
    }).await;
}

/// Once-wrong branch: a rolled-back mutation pushes NOTHING. Every rollback path of the
/// service (full party, foreign invite, non-member kick, lost 23505 race) leaves the sink
/// empty; the probing sink additionally proves, for every push the other tests make, that the
/// pushed revision was already committed on an independent connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_rolled_back_mutation_pushes_nothing() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member, stranger] = fx.players();
        create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;
        fx.sink.clear();

        assert_status(fx.svc.kick(who(&leader), stranger.clone()).await, Status::NotFound, "kick non-member");
        assert_status(fx.svc.promote(who(&leader), stranger.clone()).await, Status::NotFound, "promote non-member");
        assert_status(fx.svc.create(who(&member)).await, Status::Conflict, "create in a party");
        assert_status(fx.svc.accept(who(&stranger), UNKNOWN_UUID.into()).await, Status::NotFound, "unknown invite");
        assert_status(fx.svc.set_ready(who(&stranger), READY.into()).await, Status::NotFound, "set_ready in no party");
        assert_status(fx.svc.invite(who(&leader), fx.handle(&member)).await, Status::Conflict, "invite of a member");
        assert_eq!(fx.sink.total(), 0, "no rolled-back or rejected op may push");
    }).await;
}

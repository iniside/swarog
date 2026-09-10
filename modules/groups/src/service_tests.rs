//! Live-Postgres `Service`-level tests: the `NotFound`-not-`Forbidden` answer, the
//! `MAX_MEMBERS` cap under genuine concurrency, `decide`'s kick and self-kick-guard
//! branches, `role_of`'s uniform empty answer, and the admin promote submit. Every
//! fixture drives the REAL `Service` (never a hand-rolled proxy) over a fake
//! `accountsapi::Directory` and a real durable plane, so an emitted event is a real
//! `asyncevents.events` row.

use super::*;

use std::sync::Arc;

use adminapi::{AdminData as _, AdminSubmit as _};
use groupsapi::{
    Membership as _, Player as _, JOIN_OPEN, JOIN_REQUEST, MAX_MEMBERS, ROLE_ADMIN, ROLE_MEMBER,
    STATE_MEMBER, STATE_REQUESTED,
};
use groupsevents::{REASON_KICKED, ROLE_CHANGED};
use opsapi::{Identity, Status};
use sqlx::PgPool;

use crate::admin::{Rejection, ACTION_FIELD, ACTION_PROMOTE, GROUP_FIELD, PLAYER_FIELD};
use crate::service::NOT_FOUND;
use crate::tests::{fixed_uuid, test_pool, unique_uuid, with_cleanup, wired, FakeDirectory};

async fn event_count(pool: &PgPool, topic: &str, key: &str, value: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM asyncevents.events WHERE topic = $1 AND payload->>$2 = $3",
    )
    .bind(topic)
    .bind(key)
    .bind(value)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

async fn latest_member_left_reason(pool: &PgPool, group_id: &str, player_id: &str) -> (String, String) {
    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM asyncevents.events \
          WHERE topic = $1 AND payload->>'group_id' = $2 AND payload->>'player_id' = $3 \
          ORDER BY tie_breaker DESC LIMIT 1",
    )
    .bind(groupsevents::MEMBER_LEFT.topic())
    .bind(group_id)
    .bind(player_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        payload["reason"].as_str().unwrap().to_string(),
        payload["actor_id"].as_str().unwrap().to_string(),
    )
}

async fn insert_member(pool: &PgPool, group_id: &str, player_id: &str, state: &str, role: &str) {
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role) \
         VALUES ($1::uuid, $2::uuid, $3, $4)",
    )
    .bind(group_id)
    .bind(player_id)
    .bind(state)
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

async fn row_count(pool: &PgPool, group_id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(group_id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

// ============================================================================
// `NotFound`-not-`Forbidden`: a foreign group and an absent group are the SAME
// verdict, asserted equal to EACH OTHER rather than each in isolation.
// ============================================================================

#[tokio::test]
async fn a_foreign_group_and_an_absent_group_answer_the_identical_verdict() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    let outsider = unique_uuid(&pool).await;
    dir.insert(&admin, "GroupAdmin#0001");
    dir.insert(&outsider, "Outsider#0002");

    let created = svc.create(Identity::player(&admin), "Foreign".into(), JOIN_OPEN.into()).await.unwrap();
    let foreign_group = created.id.clone();
    let absent_group = unique_uuid(&pool).await; // never created — no row exists

    let ids = vec![foreign_group.clone(), absent_group.clone()];
    with_cleanup(&pool, ids, async move {
        let err_foreign = svc
            .members(Identity::player(&outsider), foreign_group, String::new(), 0)
            .await
            .unwrap_err();
        let err_absent = svc
            .members(Identity::player(&outsider), absent_group, String::new(), 0)
            .await
            .unwrap_err();

        assert_eq!(err_foreign.status, Status::NotFound);
        assert_eq!(err_foreign.status, err_absent.status);
        assert_eq!(
            err_foreign.msg, err_absent.msg,
            "a foreign group and an absent one must be INDISTINGUISHABLE — a difference \
             here is an enumeration oracle over group ids"
        );
        assert_eq!(err_foreign.msg, NOT_FOUND);
    })
    .await;
}

// ============================================================================
// `MAX_MEMBERS` under genuine concurrency: two REAL, concurrently-running `join`
// calls race the SAME group; the advisory lock is what serializes them, so a
// sequential test proves nothing about this branch.
// ============================================================================

/// Task A manually drives the SAME store calls `Service::join` makes, holding its
/// transaction OPEN (uncommitted) — which holds the per-group advisory xact-lock —
/// while task B runs the REAL `Service::join` concurrently on a separate connection.
/// Without the lock, both would read `counts.rows == MAX_MEMBERS - 1` under READ
/// COMMITTED and both would commit, landing the group at 501 members. With it, B must
/// BLOCK until A commits, then see the roster at the cap and answer `Conflict`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_members_admits_exactly_one_of_two_concurrent_joins() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    dir.insert(&admin, "CapAdmin#0001");
    let created = svc.create(Identity::player(&admin), "Cap Test".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();

    let a = unique_uuid(&pool).await;
    let b = unique_uuid(&pool).await;
    dir.insert(&a, "CandA#0002");
    dir.insert(&b, "CandB#0003");

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        // Fill to MAX_MEMBERS - 2: the creator's own row already counts as one, so
        // (MAX_MEMBERS - 2) more rows plus task A's pending insert lands exactly at the cap.
        for _ in 0..(MAX_MEMBERS - 2) as usize {
            let pid = unique_uuid(&pool2).await;
            insert_member(&pool2, &group_id, &pid, STATE_MEMBER, ROLE_MEMBER).await;
        }
        assert_eq!(
            row_count(&pool2, &group_id).await,
            MAX_MEMBERS - 1,
            "fixture must seed exactly one short of the cap"
        );

        // Task A: the store calls `join` itself makes, manually — NOT committed.
        let mut tx_a = pool2.begin().await.unwrap();
        svc.store.lock_group_tx(&mut tx_a, &group_id).await.unwrap();
        let counts = svc.store.counts_tx(&mut tx_a, &group_id, STATE_MEMBER, ROLE_ADMIN).await.unwrap();
        assert!(counts.rows < MAX_MEMBERS, "A must still be admitted — this proves the seed, not the cap");
        let inserted = svc
            .store
            .insert_membership_tx(&mut tx_a, &group_id, &a, STATE_MEMBER, ROLE_MEMBER)
            .await
            .unwrap();
        assert!(inserted.is_some());

        // Task B: the REAL production path, on a separate connection, started while A's
        // tx is still open — genuine concurrency, not a manual race simulation.
        let svc_b = svc.clone();
        let group_b = group_id.clone();
        let b2 = b.clone();
        let task_b = tokio::spawn(async move { svc_b.join(Identity::player(&b2), group_b).await });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !task_b.is_finished(),
            "B must BLOCK on the per-group advisory lock while A's tx is open — without the \
             lock, B would race A's uncommitted read and both would land under the cap"
        );

        tx_a.commit().await.unwrap();
        let result_b = tokio::time::timeout(std::time::Duration::from_secs(5), task_b)
            .await
            .expect("B must complete once A releases the lock")
            .unwrap();

        let err_b = result_b.expect_err("B must be rejected — the group is now at MAX_MEMBERS");
        assert_eq!(err_b.status, Status::Conflict);
        assert!(
            err_b.msg.contains(&MAX_MEMBERS.to_string()),
            "B's rejection must name the cap, got: {}",
            err_b.msg
        );

        let total = row_count(&pool2, &group_id).await;
        assert_eq!(total, MAX_MEMBERS, "exactly A's row must have committed — never both, never neither");
    })
    .await;
}

/// The cap counts EVERY live row, not just `state = 'member'` ones: seeded at
/// `MAX_MEMBERS - 1` PENDING (`requested`) rows plus the creator's one member row —
/// `MAX_MEMBERS` total, all pending states — a further `join` must still answer
/// `Conflict`. This is what lets `respond`/`decide`'s accept path promote a pending
/// row to `member` with no re-check: a promotion never changes the total row count,
/// so the invariant this test pins is the reason that's safe.
#[tokio::test]
async fn max_members_counts_pending_rows_not_only_members() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    dir.insert(&admin, "PendingCapAdmin#0001");
    let created = svc
        .create(Identity::player(&admin), "Pending Cap".into(), JOIN_REQUEST.into())
        .await
        .unwrap();
    let group_id = created.id.clone();

    for _ in 0..(MAX_MEMBERS - 1) as usize {
        let pid = unique_uuid(&pool).await;
        insert_member(&pool, &group_id, &pid, STATE_REQUESTED, "").await;
    }
    assert_eq!(row_count(&pool, &group_id).await, MAX_MEMBERS, "the group must already BE at the cap, entirely via pending rows");

    let newcomer = unique_uuid(&pool).await;
    dir.insert(&newcomer, "Newcomer#0002");

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        let err = svc
            .join(Identity::player(&newcomer), group_id.clone())
            .await
            .expect_err("a group already at the cap via pending rows alone must still refuse a join");
        assert_eq!(err.status, Status::Conflict);
        assert!(err.msg.contains(&MAX_MEMBERS.to_string()));
        assert_eq!(row_count(&pool2, &group_id).await, MAX_MEMBERS, "the rejected join must not have landed a row");
    })
    .await;
}

// ============================================================================
// `decide`'s reject arm on a `member` row is a KICK.
// ============================================================================

#[tokio::test]
async fn decide_reject_on_a_member_row_is_a_kick_with_the_canonical_actor() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    let subject = unique_uuid(&pool).await;
    dir.insert(&admin, "KickAdmin#0001");
    dir.insert(&subject, "KickTarget#0002");

    let created = svc.create(Identity::player(&admin), "Kick Test".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();
    insert_member(&pool, &group_id, &subject, STATE_MEMBER, ROLE_MEMBER).await;

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        svc.decide(Identity::player(&admin), group_id.clone(), subject.clone(), "reject".into())
            .await
            .unwrap();

        assert_eq!(row_count(&pool2, &group_id).await, 1, "only the admin's own row must remain");
        let (reason, actor_id) = latest_member_left_reason(&pool2, &group_id, &subject).await;
        assert_eq!(reason, REASON_KICKED);
        assert_eq!(
            actor_id, admin,
            "a kick's actor_id must name the admin who did it — never empty, never the subject"
        );
    })
    .await;
}

/// The once-wrong branch: `subject_id == identity` was compared as RAW TEXT, so an
/// admin spelling its OWN id differently (braced, or with a hyphen shifted — both
/// forms Postgres's `::uuid` cast accepts as the SAME value) than the bearer identity
/// slipped past the self-kick guard and reached the delete path. The fix compares the
/// database's OWN two canonical reads instead, so this must answer `Conflict`, not a
/// silent self-kick.
#[tokio::test]
async fn decide_against_an_admins_own_differently_spelled_id_is_a_conflict_not_a_self_kick() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = fixed_uuid(0x11);
    dir.insert(&admin, "SelfKickAdmin#0001");
    let created = svc.create(Identity::player(&admin), "Self Kick".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();

    // A braced spelling of the SAME uuid — Postgres's `::uuid` cast accepts it, and
    // production's `is_uuid_text`/HTTP-layer validation never runs on `subject_id`,
    // which reaches the store as a caller-supplied string.
    let braced = format!("{{{admin}}}");
    // A hyphen-shifted spelling of the SAME 32 hex digits (one fewer hyphen) — also
    // accepted by the `::uuid` cast, and the OTHER of the two spellings the fix's own
    // comment calls out (never `urn:uuid:`, which Postgres rejects outright with
    // 22P02, folding to `NotFound` before the guard is ever reached).
    let hyphen_shifted = {
        let no_dashes: String = admin.chars().filter(|c| *c != '-').collect();
        format!("{}-{}", &no_dashes[..12], &no_dashes[12..])
    };

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        for spelling in [braced, hyphen_shifted] {
            let err = svc
                .decide(Identity::player(&admin), group_id.clone(), spelling.clone(), "reject".into())
                .await
                .expect_err(&format!("self-kick via {spelling:?} must be refused"));
            assert_eq!(
                err.status,
                Status::Conflict,
                "spelling {spelling:?} must answer Conflict — got {:?} {:?}",
                err.status,
                err.msg
            );
            assert_eq!(row_count(&pool2, &group_id).await, 1, "the admin's row must survive spelling {spelling:?}");
        }
    })
    .await;
}

// ============================================================================
// `role_of` — the ONE empty answer for every non-admitting case, and the two
// distinct real answers.
// ============================================================================

#[tokio::test]
async fn role_of_answers_the_same_empty_string_for_every_non_member_case() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    let outsider = unique_uuid(&pool).await;
    let pending = unique_uuid(&pool).await;
    dir.insert(&admin, "RoleAdmin#0001");

    let created = svc.create(Identity::player(&admin), "Role Of".into(), JOIN_REQUEST.into()).await.unwrap();
    let group_id = created.id.clone();
    insert_member(&pool, &group_id, &pending, STATE_REQUESTED, "").await;
    let absent_group = unique_uuid(&pool).await;

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        assert_eq!(svc.role_of(group_id.clone(), outsider).await.unwrap(), "", "a non-member");
        assert_eq!(svc.role_of(group_id.clone(), pending).await.unwrap(), "", "a pending (requested) row");
        assert_eq!(svc.role_of(absent_group, unique_uuid(&pool2).await).await.unwrap(), "", "an absent group");
        assert_eq!(svc.role_of(group_id.clone(), "not-a-uuid".into()).await.unwrap(), "", "a malformed player id");
        assert_eq!(svc.role_of(group_id.clone(), admin.clone()).await.unwrap(), ROLE_ADMIN, "the real admin");
    })
    .await;
}

#[tokio::test]
async fn role_of_distinguishes_admin_from_plain_member() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    let member = unique_uuid(&pool).await;
    dir.insert(&admin, "PlainRoleAdmin#0001");

    let created = svc.create(Identity::player(&admin), "Plain Role".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();
    insert_member(&pool, &group_id, &member, STATE_MEMBER, ROLE_MEMBER).await;

    let ids = vec![group_id.clone()];
    with_cleanup(&pool, ids, async move {
        assert_eq!(svc.role_of(group_id.clone(), admin).await.unwrap(), ROLE_ADMIN);
        assert_eq!(svc.role_of(group_id, member).await.unwrap(), ROLE_MEMBER);
    })
    .await;
}

// ============================================================================
// `admin::apply_submit` — `Stale` vs `Rejected`, foreign/malformed-param and
// dead-pool tolerance, and the promote/`AlreadyAdmin` event-emission split.
// ============================================================================

fn params(pairs: &[(&str, &str)]) -> adminapi::Params {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A vanished group (never created, or already deleted) is `Stale` — the remedy is
/// "reload the page". Checked on BOTH authorities: the local `apply_submit` (the
/// `Rejection` variant) and the wire `admin_submit` (the `opsapi::Status` it maps to),
/// which must NEVER be `NotFound` — the edge makes that indistinguishable from
/// `UnknownMethod` and would silently degrade this page to read-only.
#[tokio::test]
async fn apply_submit_promote_on_a_vanished_group_is_stale() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let group_id = unique_uuid(&pool).await; // never created
    let player_id = unique_uuid(&pool).await;

    let outcome = crate::admin::apply_submit(
        &svc,
        params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, &player_id)]),
    )
    .await;
    assert!(matches!(outcome, Err(Rejection::Stale)), "a group that does not exist must be Stale");

    let wire_err = svc
        .admin_submit(
            "groups".into(),
            params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, &player_id)]),
        )
        .await
        .unwrap_err();
    assert_eq!(wire_err.status, Status::Conflict);
    assert_ne!(wire_err.status, Status::NotFound);
}

/// A malformed player id is `Rejected` — the operator's own input, 400-class, with a
/// message. Distinct from `Stale`: this is the same authority `apply_submit` runs for
/// a remote submit, and the two verdicts must never collapse into one.
#[tokio::test]
async fn apply_submit_promote_with_a_bad_player_id_is_rejected_not_stale() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    dir.insert(&admin, "SubmitAdmin#0001");
    let created = svc.create(Identity::player(&admin), "Submit".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();

    let ids = vec![group_id.clone()];
    with_cleanup(&pool, ids, async move {
        let outcome = crate::admin::apply_submit(
            &svc,
            params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, "not-a-uuid")]),
        )
        .await;
        let label = match &outcome {
            Ok(_) => "Ok".to_string(),
            Err(Rejection::Stale) => "Stale".to_string(),
            Err(Rejection::Rejected(m)) => format!("Rejected({m})"),
            Err(Rejection::Internal(m)) => format!("Internal({m})"),
        };
        assert!(
            matches!(outcome, Err(Rejection::Rejected(_))),
            "a malformed player id must be Rejected, got {label}"
        );

        let wire_err = svc
            .admin_submit(
                "groups".into(),
                params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, "not-a-uuid")]),
            )
            .await
            .unwrap_err();
        assert_eq!(wire_err.status, Status::Invalid);
        assert_ne!(wire_err.status, Status::NotFound);
    })
    .await;
}

fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .expect("lazy pool from a well-formed DSN")
}

/// The THIRD `Rejection` class — an infrastructure failure (here: the store cannot be
/// reached at all) — also stays clear of `Status::NotFound` on the wire. Together with
/// the two tests above, every arm `apply_submit`/`admin_submit` can take is now pinned:
/// deleting any one of the three `Rejection::into_ops` match arms would either fail to
/// compile (non-exhaustive match) or collapse two classes into the same wire status —
/// this test only catches the latter, which is why the other two tests check their OWN
/// specific status too, not just "not NotFound".
#[tokio::test]
async fn apply_submit_promote_against_an_unreachable_store_is_internal_not_not_found() {
    let svc = Service::new(dead_pool(), Arc::new(bus::Bus::new()));
    let group_id = "00000000-0000-4000-8000-000000000000";
    let player_id = "00000000-0000-4000-8000-000000000001";

    let wire_err = svc
        .admin_submit(
            "groups".into(),
            params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, group_id), (PLAYER_FIELD, player_id)]),
        )
        .await
        .unwrap_err();
    assert_eq!(wire_err.status, Status::Internal);
    assert_ne!(wire_err.status, Status::NotFound);
}

/// `admin_data` NEVER `Err`s — not on a foreign param, not on a malformed one, and not
/// even when the store cannot be reached at all. The portal forwards every page's
/// params to every resolved provider, so an `Err` here would degrade an UNRELATED
/// page in the split.
#[tokio::test]
async fn admin_data_never_errs_on_foreign_params_malformed_params_or_a_dead_pool() {
    let svc = Service::new(dead_pool(), Arc::new(bus::Bus::new()));

    let foreign = svc.admin_data(params(&[("owner", "character:not-groups-business")])).await;
    assert!(foreign.is_ok(), "a foreign param must not fault this page");

    let malformed = svc.admin_data(params(&[("group", "not-a-uuid")])).await;
    assert!(malformed.is_ok(), "a malformed group ref must render an error card, not Err");

    let overview_against_dead_pool = svc.admin_data(adminapi::Params::new()).await;
    assert!(
        overview_against_dead_pool.is_ok(),
        "a store failure (here: total unreachability) must render an error card, not Err"
    );
}

/// A promote emits EXACTLY one `group.role_changed`; the `AlreadyAdmin` re-submit — the
/// SAME action against a subject already holding `ROLE_ADMIN` — emits none. Without a
/// decoy/count-based proof, "one event" and "no event" are indistinguishable from
/// "the emit call was deleted".
#[tokio::test]
async fn promote_emits_exactly_one_role_changed_and_a_resubmit_emits_none() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = unique_uuid(&pool).await;
    let member = unique_uuid(&pool).await;
    dir.insert(&admin, "PromoteAdmin#0001");

    let created = svc.create(Identity::player(&admin), "Promote".into(), JOIN_OPEN.into()).await.unwrap();
    let group_id = created.id.clone();
    insert_member(&pool, &group_id, &member, STATE_MEMBER, ROLE_MEMBER).await;

    let ids = vec![group_id.clone()];
    let pool2 = pool.clone();
    with_cleanup(&pool, ids, async move {
        let before = event_count(&pool2, ROLE_CHANGED.topic(), "player_id", &member).await;

        let first = match crate::admin::apply_submit(
            &svc,
            params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, &member)]),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => panic!("the first promotion must succeed"),
        };
        assert!(first.notice.unwrap().contains("is now"));
        let after_first = event_count(&pool2, ROLE_CHANGED.topic(), "player_id", &member).await;
        assert_eq!(after_first, before + 1, "the promotion must emit EXACTLY one role_changed");

        let second = match crate::admin::apply_submit(
            &svc,
            params(&[(ACTION_FIELD, ACTION_PROMOTE), (GROUP_FIELD, &group_id), (PLAYER_FIELD, &member)]),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => panic!("an AlreadyAdmin resubmit must still be Ok, not a rejection"),
        };
        assert!(second.notice.unwrap().contains("was already"));
        let after_second = event_count(&pool2, ROLE_CHANGED.topic(), "player_id", &member).await;
        assert_eq!(after_second, after_first, "an AlreadyAdmin resubmit must emit NO new event");

        let role = svc.role_of(group_id.clone(), member.clone()).await.unwrap();
        assert_eq!(role, ROLE_ADMIN);
    })
    .await;
}

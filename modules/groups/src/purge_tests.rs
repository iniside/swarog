//! Steps 5-7 of the account-deletion plan for `groups`: the purge tombstone, its
//! scheduled sweep (last-admin/last-member dissolution, the pending-relation
//! non-dissolution guard, the per-fire `GROUPS_PER_FIRE` cap), and the `create`
//! write guard. A sibling of `service_tests`/`store_tests`/`projection_tests`,
//! reusing `crate::tests`' shared live-DB harness.

use std::sync::Arc;

use groupsapi::{Player as _, JOIN_OPEN};
use opsapi::{Identity, Status};
use sqlx::PgPool;

use crate::tests::{fixed_uuid, test_pool, with_cleanup, wired, FakeDirectory, DB_LOCK};

async fn purge_swept_at(pool: &PgPool, player_id: &str) -> Option<Option<String>> {
    sqlx::query_as::<_, (Option<String>,)>(
        "SELECT swept_at::text FROM groups.purged_players WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|(s,)| s)
}

async fn stamp_purge_swept(pool: &PgPool, player_id: &str) {
    sqlx::query("UPDATE groups.purged_players SET swept_at = now() WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn cleanup_purge_row(pool: &PgPool, player_id: &str) {
    let _ = sqlx::query("DELETE FROM groups.purged_players WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await;
}

async fn group_exists(pool: &PgPool, group_id: &str) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM groups.groups WHERE id = $1::uuid")
        .bind(group_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
}

async fn membership_exists(pool: &PgPool, group_id: &str, player_id: &str) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM groups.memberships WHERE group_id = $1::uuid AND player_id = $2::uuid",
    )
    .bind(group_id)
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .is_some()
}

async fn member_left_count(pool: &PgPool, player_id: &str, reason: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM asyncevents.events WHERE topic = 'group.member_left' \
          AND payload->>'player_id' = $1 AND payload->>'reason' = $2",
    )
    .bind(player_id)
    .bind(reason)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The tombstone insert is idempotent, and a redelivery after the sweep stamped
/// `swept_at` must NOT reset it.
#[tokio::test]
async fn plant_purged_player_is_idempotent_and_never_resets_swept_at() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir).await;
    let pid = fixed_uuid(0x01);

    let mut conn = pool.acquire().await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();

    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM groups.purged_players WHERE player_id = $1::uuid")
            .bind(&pid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "a redelivered tombstone must not insert a second row");

    stamp_purge_swept(&pool, &pid).await;
    svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
    assert!(
        purge_swept_at(&pool, &pid).await.flatten().is_some(),
        "a redelivery after the sweep stamped swept_at must NOT reset it"
    );

    cleanup_purge_row(&pool, &pid).await;
}

/// D7: a group purged down to its LAST admin (or last member) is DISSOLVED — but a
/// group with a SECOND admin survives the first admin's purge (the sweep's
/// last-admin invariant is resolved, never reported as a `Conflict`, per F3).
#[tokio::test]
async fn sweep_dissolves_a_sole_admins_group_but_not_a_two_admin_group() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let solo_admin = fixed_uuid(0x02);
    let two_admin_a = fixed_uuid(0x03);
    let two_admin_b = fixed_uuid(0x04);
    dir.insert(&solo_admin, "SoloAdmin#0001");
    dir.insert(&two_admin_a, "TwoAdminA#0002");
    dir.insert(&two_admin_b, "TwoAdminB#0003");

    let solo_group = svc
        .create(Identity::player(&solo_admin), "SoloAdminGroup".into(), JOIN_OPEN.into())
        .await
        .unwrap()
        .id;
    let two_group = svc
        .create(Identity::player(&two_admin_a), "TwoAdminGroup".into(), JOIN_OPEN.into())
        .await
        .unwrap()
        .id;
    svc.join(Identity::player(&two_admin_b), two_group.clone()).await.unwrap();
    // Promote the second member to admin directly — this test targets the SWEEP's
    // dissolution logic, not the promote contract.
    sqlx::query(
        "UPDATE groups.memberships SET role = 'admin' WHERE group_id = $1::uuid AND player_id = $2::uuid",
    )
    .bind(&two_group)
    .bind(&two_admin_b)
    .execute(&pool)
    .await
    .unwrap();

    let group_ids = vec![solo_group.clone(), two_group.clone()];
    with_cleanup(&pool, group_ids, {
        let pool = pool.clone();
        let svc = svc.clone();
        let solo_admin = solo_admin.clone();
        let two_admin_a = two_admin_a.clone();
        let two_admin_b = two_admin_b.clone();
        let solo_group = solo_group.clone();
        let two_group = two_group.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &solo_admin, "SoloAdmin#0001").await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &two_admin_a, "TwoAdminA#0002").await.unwrap();
            drop(conn);

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            assert!(!group_exists(&pool, &solo_group).await, "a sole admin's group must be DISSOLVED");
            assert!(
                group_exists(&pool, &two_group).await,
                "a group with a SURVIVING second admin must NOT be dissolved"
            );
            assert!(
                !membership_exists(&pool, &two_group, &two_admin_a).await,
                "the purged admin's own membership must be gone"
            );
            assert!(
                membership_exists(&pool, &two_group, &two_admin_b).await,
                "the surviving admin's membership must be untouched"
            );
            assert_eq!(
                member_left_count(&pool, &solo_admin, groupsevents::REASON_ACCOUNT_DELETED).await,
                1,
                "the sole admin's own leave must be emitted"
            );
            assert_eq!(
                member_left_count(&pool, &two_admin_a, groupsevents::REASON_ACCOUNT_DELETED).await,
                1,
                "the surviving group's purged admin leave must be emitted"
            );

            cleanup_purge_row(&pool, &solo_admin).await;
            cleanup_purge_row(&pool, &two_admin_a).await;
        }
    })
    .await;
}

/// THE PREVIOUSLY-WRONG SHAPE: a PENDING relation (invited/requested, never a
/// `member` row) must NEVER trigger dissolution — even when the group's remaining
/// `member`-state rows happen to number exactly one, which a counts-only check
/// (ignoring the deleted row's OWN state) would have misread as "the last member
/// left".
#[tokio::test]
async fn sweep_never_dissolves_a_group_over_a_purged_players_pending_relation() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;

    let admin = fixed_uuid(0x05);
    let pending = fixed_uuid(0x06);
    dir.insert(&admin, "PendingAdmin#0001");
    dir.insert(&pending, "PendingGuest#0002");

    let group_id = svc
        .create(Identity::player(&admin), "PendingGroup".into(), JOIN_OPEN.into())
        .await
        .unwrap()
        .id;
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role) \
         VALUES ($1::uuid, $2::uuid, 'invited', '')",
    )
    .bind(&group_id)
    .bind(&pending)
    .execute(&pool)
    .await
    .unwrap();

    with_cleanup(&pool, vec![group_id.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let admin = admin.clone();
        let pending = pending.clone();
        let group_id = group_id.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pending, "PendingGuest#0002").await.unwrap();
            drop(conn);

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            assert!(
                group_exists(&pool, &group_id).await,
                "a pending (never-a-member) row must never dissolve the group"
            );
            assert!(
                membership_exists(&pool, &group_id, &admin).await,
                "the real admin's membership must survive untouched"
            );
            assert!(
                !membership_exists(&pool, &group_id, &pending).await,
                "the purged player's own pending row must still be removed"
            );
            assert_eq!(
                member_left_count(&pool, &pending, groupsevents::REASON_ACCOUNT_DELETED).await,
                1
            );

            cleanup_purge_row(&pool, &pending).await;
        }
    })
    .await;
}

/// [`projection::GROUPS_PER_FIRE`] (64) bounds one fire's dissolved-group count: with
/// 65 disjoint sole-admin groups tombstoned for one player, a single fire dissolves
/// exactly 64 and leaves the tombstone UNSTAMPED (`swept_at IS NULL`) so the next
/// fire resumes and finishes the last one — pre-fix (no cap) a single fire would
/// have tried all 65 in one transaction with no resumption boundary.
#[tokio::test]
async fn sweep_hits_the_per_fire_group_cap_leaving_swept_at_null_until_the_next_fire() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir).await;
    let player = fixed_uuid(0x07);

    const N: i64 = (crate::projection::GROUPS_PER_FIRE as i64) + 1;
    let group_ids: Vec<String> = sqlx::query_scalar(
        "INSERT INTO groups.groups (name, join_policy, creator_id) \
         SELECT 'CapGroup ' || g, 'open', $1::uuid FROM generate_series(1, $2) g \
         RETURNING id::text",
    )
    .bind(&player)
    .bind(N)
    .fetch_all(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role) \
         SELECT gid, $1::uuid, 'member', 'admin' FROM unnest($2::uuid[]) AS gid",
    )
    .bind(&player)
    .bind(&group_ids)
    .execute(&pool)
    .await
    .unwrap();

    with_cleanup(&pool, group_ids.clone(), {
        let pool = pool.clone();
        let svc = svc.clone();
        let player = player.clone();
        let group_ids = group_ids.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &player, "CapPlayer#0001").await.unwrap();
            drop(conn);

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            let (remaining,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM groups.groups WHERE id = ANY($1::uuid[])",
            )
            .bind(&group_ids)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(remaining, 1, "one fire must dissolve exactly GROUPS_PER_FIRE groups, not all");
            assert!(
                purge_swept_at(&pool, &player).await.flatten().is_none(),
                "the per-fire cap must leave the tombstone UNSTAMPED"
            );

            let mut tx2 = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx2).await.unwrap();
            tx2.commit().await.unwrap();

            let (remaining2,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM groups.groups WHERE id = ANY($1::uuid[])",
            )
            .bind(&group_ids)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(remaining2, 0, "the next fire must finish the last group");
            assert!(
                purge_swept_at(&pool, &player).await.flatten().is_some(),
                "swept_at must now be stamped"
            );

            cleanup_purge_row(&pool, &player).await;
        }
    })
    .await;
}

/// `create` is blocked by the tombstone PREDICATE, not "a tombstone exists
/// somewhere": a purged player's create is Conflict with no group row, while a
/// different, non-tombstoned player still succeeds.
#[tokio::test]
async fn create_is_blocked_by_tombstone_predicate_not_presence() {
    let Some(pool) = test_pool().await else { return };
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let purged = fixed_uuid(0x08);
    let other = fixed_uuid(0x09);
    dir.insert(&purged, "Purged#0001");
    dir.insert(&other, "Other#0002");

    let mut conn = pool.acquire().await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, &purged, "Purged#0001").await.unwrap();
    drop(conn);

    let e = svc
        .create(Identity::player(&purged), "Ghost".into(), JOIN_OPEN.into())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Conflict);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM groups.groups WHERE creator_id = $1::uuid")
        .bind(&purged)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "no group row for a tombstoned creator");

    let created = svc
        .create(Identity::player(&other), "Alive".into(), JOIN_OPEN.into())
        .await
        .expect("a different, non-tombstoned player must still be able to create");

    cleanup_purge_row(&pool, &purged).await;
    with_cleanup(&pool, vec![created.id], async move {}).await;
}

//! Steps 5-7 of the account-deletion plan for `groups`: the purge tombstone, its
//! scheduled sweep (last-admin/last-member dissolution, the pending-relation
//! non-dissolution guard, the per-fire `GROUPS_PER_FIRE` cap), and the `create`
//! write guard. A sibling of `service_tests`/`store_tests`/`projection_tests`,
//! reusing `crate::tests`' shared live-DB harness.

use std::sync::Arc;
use std::time::Duration;

use groupsapi::{Player as _, JOIN_OPEN, MAX_MEMBERS};
use opsapi::{Identity, Status};
use sqlx::PgPool;

use crate::projection::GROUPS_PLAYERS_PER_FIRE;
use crate::service::{NO_SUCH_PLAYER, PLAYER_DELETED};
use crate::store::Store;
use crate::tests::{
    cleanup_groups, fixed_uuid, test_pool, with_cleanup, wired, FakeDirectory, DB_LOCK,
};
use crate::Service;

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
    let _serialized = DB_LOCK.lock().await;
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
            assert_claimed_by_the_next_fire(&pool, &solo_admin).await;
            assert_claimed_by_the_next_fire(&pool, &two_admin_a).await;

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
            assert_claimed_by_the_next_fire(&pool, &pending).await;

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
            assert_claimed_by_the_next_fire(&pool, &player).await;

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
    let _serialized = DB_LOCK.lock().await;
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

async fn claimed_by_the_next_fire(pool: &PgPool) -> Vec<String> {
    let store = Store { pool: pool.clone() };
    let mut tx = pool.begin().await.unwrap();
    let mut claimed = store.claim_due_tx(&mut tx, GROUPS_PLAYERS_PER_FIRE).await.unwrap();
    tx.rollback().await.unwrap();
    claimed.sort();
    claimed
}

async fn assert_claimed_by_the_next_fire(pool: &PgPool, player_id: &str) {
    let claimed = claimed_by_the_next_fire(pool).await;
    assert!(
        claimed.iter().any(|id| id == player_id),
        "precondition: the next fire does not claim {player_id}"
    );
}

async fn assert_next_fire_claims_only(pool: &PgPool, ours: &[String], expected_len: usize) {
    let claimed = claimed_by_the_next_fire(pool).await;
    assert!(
        claimed.iter().all(|id| ours.contains(id)),
        "precondition: the next fire claims a foreign tombstone: {claimed:?}"
    );
    assert_eq!(claimed.len(), expected_len, "precondition: the next fire claims {claimed:?}");
}

async fn unstamped_tombstones(pool: &PgPool, ids: &[String]) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT player_id::text FROM groups.purged_players \
          WHERE player_id = ANY($1::uuid[]) AND swept_at IS NULL ORDER BY player_id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn cleanup_players(pool: &PgPool, players: &[String]) {
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM groups.groups WHERE creator_id = ANY($1::uuid[]) \
         UNION SELECT group_id::text FROM groups.memberships WHERE player_id = ANY($1::uuid[])",
    )
    .bind(players)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    cleanup_groups(pool, &groups).await;
    let _ = sqlx::query(
        "DELETE FROM asyncevents.events \
          WHERE payload->>'player_id' = ANY($1) OR payload->>'creator_id' = ANY($1)",
    )
    .bind(players)
    .execute(pool)
    .await;
    for id in players {
        cleanup_purge_row(pool, id).await;
    }
}

async fn with_purge_cleanup<Fut>(pool: &PgPool, apps: Vec<String>, players: Vec<String>, body: Fut)
where
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut task = tokio::spawn(body);
    let outcome = tokio::time::timeout(Duration::from_secs(60), &mut task).await;
    if outcome.is_err() {
        task.abort();
        let _ = (&mut task).await;
    }
    if !matches!(outcome, Ok(Ok(()))) {
        for app in &apps {
            sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1",
            )
            .bind(app)
            .execute(pool)
            .await
            .unwrap();
        }
    }
    cleanup_players(pool, &players).await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Ok(Err(e)) => panic!("groups purge test task ended without completing: {e}"),
        Err(_) => panic!("groups purge test body did not finish within 60s"),
    }
}

fn racer_pool(app: &str) -> PgPool {
    let options = testdb::dsn()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("DATABASE_URL parses")
        .application_name(app);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(60))
        .connect_lazy_with(options)
}

fn racer_app(what: &str, player_id: &str) -> String {
    format!("groups-{what}-{player_id}")
}

fn racer_service(svc: &Service, dir: &Arc<FakeDirectory>, app: &str) -> Arc<Service> {
    let racer = Service::new(racer_pool(app), svc.bus.clone());
    racer.directory.set(dir.clone()).ok().expect("directory set once");
    Arc::new(racer)
}

async fn finish<T>(what: &str, handle: tokio::task::JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("{what} task did not complete: {e}"),
        Err(_) => panic!("{what} did not finish within 30s"),
    }
}

async fn parked_on_advisory<T>(
    pool: &PgPool,
    app: &str,
    keys: &[i64],
    racer: &tokio::task::JoinHandle<T>,
) -> Option<i64> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: Option<i64> = sqlx::query_scalar(
            "SELECT (l.classid::bigint << 32) | l.objid::bigint \
               FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'advisory' \
                AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = ANY($2) \
              LIMIT 1",
        )
        .bind(app)
        .bind(keys)
        .fetch_optional(pool)
        .await
        .unwrap();
        if parked.is_some() {
            return parked;
        }
        if racer.is_finished() {
            return None;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the advisory locks {keys:?} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_advisory_parked<T>(
    pool: &PgPool,
    app: &str,
    key: i64,
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    parked_on_advisory(pool, app, &[key], racer).await.is_some()
}

async fn wait_xact_parked<T>(
    pool: &PgPool,
    app: &str,
    xid: &str,
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'transactionid' \
                AND l.transactionid::text = $2",
        )
        .bind(app)
        .bind(xid)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked > 0 {
            return true;
        }
        if racer.is_finished() {
            return false;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the transaction {xid} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn current_xid(conn: &mut sqlx::PgConnection) -> String {
    sqlx::query_scalar("SELECT (pg_current_xact_id()::text::bigint % 4294967296)::text")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

async fn gate_membership(gate: &mut sqlx::PgConnection, group_id: &str, player_id: &str) -> String {
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role) \
         VALUES ($1::uuid, $2::uuid, 'invited', '')",
    )
    .bind(group_id)
    .bind(player_id)
    .execute(&mut *gate)
    .await
    .unwrap();
    current_xid(gate).await
}

async fn held_advisory_keys(conn: &mut sqlx::PgConnection) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT (classid::bigint << 32) | objid::bigint FROM pg_locks \
          WHERE locktype = 'advisory' AND objsubid = 1 AND pid = pg_backend_pid()",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap()
}

async fn taken_player_key(pool: &PgPool, svc: &Service, spelling: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_player_tx(&mut tx, spelling).await.unwrap();
    let keys = held_advisory_keys(&mut tx).await;
    tx.rollback().await.unwrap();
    assert_eq!(keys.len(), 1, "exactly one advisory lock must be held, got {keys:?}");
    keys[0]
}

async fn taken_group_key(pool: &PgPool, svc: &Service, group_id: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_group_tx(&mut tx, group_id).await.unwrap();
    let keys = held_advisory_keys(&mut tx).await;
    tx.rollback().await.unwrap();
    assert_eq!(keys.len(), 1, "exactly one advisory lock must be held, got {keys:?}");
    keys[0]
}

async fn plant(svc: &Service, pool: &PgPool, player_id: &str, handle: &str) {
    let mut conn = pool.acquire().await.unwrap();
    svc.store.plant_purged_player_tx(&mut conn, player_id, handle).await.unwrap();
}

async fn swept(pool: &PgPool, player_id: &str) -> bool {
    purge_swept_at(pool, player_id).await.flatten().is_some()
}

fn spawn_sweep(svc: &Arc<Service>, app: &str) -> tokio::task::JoinHandle<Result<(), String>> {
    let svc = svc.clone();
    let racer = racer_pool(app);
    tokio::spawn(async move {
        let mut tx = racer.begin().await.map_err(|e| e.to_string())?;
        svc.sweep_purged_players(&mut tx).await.map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())
    })
}

#[tokio::test]
async fn player_lock_key_is_one_per_uuid_across_spellings() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let canon = "a2b7e8c1-0000-4000-8000-0000000000f1";
    let hex = canon.replace('-', "");

    let key = taken_player_key(&pool, &svc, canon).await;
    assert_eq!(key, taken_player_key(&pool, &svc, &canon.to_uppercase()).await, "case must not change the key");
    assert_eq!(
        key,
        taken_player_key(&pool, &svc, &format!("{{{canon}}}")).await,
        "braces must not change the key"
    );
    assert_eq!(key, taken_player_key(&pool, &svc, &hex).await, "hyphens must not change the key");
    assert_ne!(
        key,
        taken_player_key(&pool, &svc, "a2b7e8c1-0000-4000-8000-0000000000f2").await,
        "a genuinely different uuid must take a different key"
    );
    assert_ne!(
        key,
        taken_group_key(&pool, &svc, canon).await,
        "the player lock must not be the group lock of the same uuid"
    );
    taken_player_key(&pool, &svc, "not-a-uuid").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_create_and_dissolves_its_group() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let pid = fixed_uuid(0x0a);
    let lock_key = taken_player_key(&pool, &svc, &pid).await;
    let gate_app = racer_app("race1-gate", &pid);
    let create_app = racer_app("race1-create", &pid);
    let sweep_app = racer_app("race1-sweep", &pid);

    let apps = vec![gate_app.clone(), create_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, vec![pid.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(asyncevents::store::WRITER_LOCK_KEY)
                .execute(&mut *gate)
                .await
                .unwrap();

            let create = tokio::spawn({
                let racer = racer_service(&svc, &dir, &create_app);
                let pid = pid.clone();
                async move {
                    racer
                        .create(Identity::player(&pid), "Inflight".into(), JOIN_OPEN.into())
                        .await
                        .map_err(|e| (e.status, e.msg))
                }
            });
            assert!(
                wait_advisory_parked(&pool, &create_app, asyncevents::store::WRITER_LOCK_KEY, &create)
                    .await,
                "create must be held at its event append, past its guard and its INSERTs"
            );

            plant(&svc, &pool, &pid, "Race#0001").await;
            assert_claimed_by_the_next_fire(&pool, &pid).await;
            let sweep = spawn_sweep(&svc, &sweep_app);
            let parked = wait_advisory_parked(&pool, &sweep_app, lock_key, &sweep).await;

            gate.commit().await.unwrap();
            let group = finish("race-1 create", create)
                .await
                .expect("the create passed its guard before the tombstone");
            assert_eq!(finish("race-1 sweep", sweep).await, Ok(()));

            assert!(!group_exists(&pool, &group.id).await, "the in-flight create's group survived the erasure");
            assert!(!membership_exists(&pool, &group.id, &pid).await);
            assert_eq!(member_left_count(&pool, &pid, groupsevents::REASON_ACCOUNT_DELETED).await, 1);
            assert!(swept(&pool, &pid).await, "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the player's advisory lock");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_join_and_purges_its_membership() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let owner = fixed_uuid(0x0b);
    let joiner = fixed_uuid(0x0b);
    dir.insert(&owner, "JoinOwner#0001");
    dir.insert(&joiner, "Joiner#0002");
    let lock_key = taken_player_key(&pool, &svc, &joiner).await;
    let gate_app = racer_app("race1-gate", &joiner);
    let join_app = racer_app("race1-join", &joiner);
    let sweep_app = racer_app("race1-sweep", &joiner);

    let apps = vec![gate_app.clone(), join_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, vec![owner.clone(), joiner.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&owner), "JoinRace".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            let gate_xid = gate_membership(&mut gate, &group_id, &joiner).await;

            let join = tokio::spawn({
                let racer = racer_service(&svc, &dir, &join_app);
                let (joiner, group_id) = (joiner.clone(), group_id.clone());
                async move {
                    racer
                        .join(Identity::player(&joiner), group_id)
                        .await
                        .map(|m| m.state)
                        .map_err(|e| (e.status, e.msg))
                }
            });
            assert!(
                wait_xact_parked(&pool, &join_app, &gate_xid, &join).await,
                "the join must be held at its INSERT, past its locks and its tombstone guard"
            );

            plant(&svc, &pool, &joiner, "Race#0002").await;
            assert_claimed_by_the_next_fire(&pool, &joiner).await;
            let sweep = spawn_sweep(&svc, &sweep_app);
            let parked = wait_advisory_parked(&pool, &sweep_app, lock_key, &sweep).await;

            gate.rollback().await.unwrap();
            assert_eq!(
                finish("race-1 join", join).await,
                Ok("member".to_string()),
                "the join passed its guard before the tombstone"
            );
            assert_eq!(finish("race-1 sweep", sweep).await, Ok(()));

            assert!(
                !membership_exists(&pool, &group_id, &joiner).await,
                "the in-flight join's membership survived the erasure"
            );
            assert!(membership_exists(&pool, &group_id, &owner).await, "the owner must stay");
            assert!(group_exists(&pool, &group_id).await, "a group with its admin left must stay");
            assert_eq!(member_left_count(&pool, &joiner, groupsevents::REASON_ACCOUNT_DELETED).await, 1);
            assert!(swept(&pool, &joiner).await, "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the joiner's advisory lock");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_invite_and_purges_the_invitation() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let admin = fixed_uuid(0x0c);
    let target = fixed_uuid(0x0c);
    dir.insert(&admin, "InviteAdmin#0001");
    dir.insert(&target, "InviteTarget#0002");
    let lock_key = taken_player_key(&pool, &svc, &target).await;
    let gate_app = racer_app("race1-gate", &target);
    let invite_app = racer_app("race1-invite", &target);
    let sweep_app = racer_app("race1-sweep", &target);

    let apps = vec![gate_app.clone(), invite_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, vec![admin.clone(), target.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&admin), "InviteRace".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            let gate_xid = gate_membership(&mut gate, &group_id, &target).await;

            let invite = tokio::spawn({
                let racer = racer_service(&svc, &dir, &invite_app);
                let (admin, group_id) = (admin.clone(), group_id.clone());
                async move {
                    racer
                        .invite(Identity::player(&admin), group_id, "InviteTarget#0002".into())
                        .await
                        .map_err(|e| (e.status, e.msg))
                }
            });
            assert!(
                wait_xact_parked(&pool, &invite_app, &gate_xid, &invite).await,
                "the invite must be held at its INSERT, past its locks and the target's guard"
            );

            plant(&svc, &pool, &target, "InviteTarget#0002").await;
            assert_claimed_by_the_next_fire(&pool, &target).await;
            let sweep = spawn_sweep(&svc, &sweep_app);
            let parked = wait_advisory_parked(&pool, &sweep_app, lock_key, &sweep).await;

            gate.rollback().await.unwrap();
            assert_eq!(
                finish("race-1 invite", invite).await,
                Ok(()),
                "the invite passed the target's guard before the tombstone"
            );
            assert_eq!(finish("race-1 sweep", sweep).await, Ok(()));

            assert!(
                !membership_exists(&pool, &group_id, &target).await,
                "the in-flight invitation survived the erasure"
            );
            assert!(group_exists(&pool, &group_id).await);
            assert_eq!(member_left_count(&pool, &target, groupsevents::REASON_ACCOUNT_DELETED).await, 1);
            assert!(swept(&pool, &target).await, "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the target's advisory lock");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_parks_behind_an_open_sweep_then_answers_player_deleted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let owner = fixed_uuid(0x0d);
    let joiner = fixed_uuid(0x0d);
    dir.insert(&owner, "LateOwner#0001");
    dir.insert(&joiner, "LateJoiner#0002");
    let lock_key = taken_player_key(&pool, &svc, &joiner).await;
    let sweep_app = racer_app("race2-sweep", &joiner);
    let join_app = racer_app("race2-join", &joiner);

    let apps = vec![sweep_app.clone(), join_app.clone()];
    with_purge_cleanup(&pool, apps, vec![owner.clone(), joiner.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&owner), "LateJoin".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            plant(&svc, &pool, &joiner, "LateJoiner#0002").await;
            assert_claimed_by_the_next_fire(&pool, &joiner).await;

            let sweep_pool = racer_pool(&sweep_app);
            let mut sweep_tx = sweep_pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut sweep_tx).await.unwrap();
            let stamped: bool = sqlx::query_scalar(
                "SELECT swept_at IS NOT NULL FROM groups.purged_players WHERE player_id = $1::uuid",
            )
            .bind(&joiner)
            .fetch_one(&mut *sweep_tx)
            .await
            .unwrap();
            assert!(stamped, "the open sweep must have locked and swept this player");

            let join = tokio::spawn({
                let racer = racer_service(&svc, &dir, &join_app);
                let (joiner, group_id) = (joiner.clone(), group_id.clone());
                async move {
                    racer
                        .join(Identity::player(&joiner), group_id)
                        .await
                        .map(|m| m.state)
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked = wait_advisory_parked(&pool, &join_app, lock_key, &join).await;
            assert!(parked, "the join must park on the joiner's lock while the sweep is open");

            sweep_tx.commit().await.unwrap();
            assert_eq!(
                finish("race-2 join", join).await,
                Err((Status::Conflict, PLAYER_DELETED.to_string()))
            );
            assert!(!membership_exists(&pool, &group_id, &joiner).await, "the refused join must add no row");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invite_parks_behind_an_open_sweep_then_answers_no_such_player() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let admin = fixed_uuid(0x0d);
    let target = fixed_uuid(0x0d);
    dir.insert(&admin, "LateAdmin#0001");
    dir.insert(&target, "LateTarget#0002");
    let lock_key = taken_player_key(&pool, &svc, &target).await;
    let sweep_app = racer_app("race2-sweep", &target);
    let invite_app = racer_app("race2-invite", &target);

    let apps = vec![sweep_app.clone(), invite_app.clone()];
    with_purge_cleanup(&pool, apps, vec![admin.clone(), target.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&admin), "LateInvite".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            plant(&svc, &pool, &target, "LateTarget#0002").await;
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let sweep_pool = racer_pool(&sweep_app);
            let mut sweep_tx = sweep_pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut sweep_tx).await.unwrap();
            let stamped: bool = sqlx::query_scalar(
                "SELECT swept_at IS NOT NULL FROM groups.purged_players WHERE player_id = $1::uuid",
            )
            .bind(&target)
            .fetch_one(&mut *sweep_tx)
            .await
            .unwrap();
            assert!(stamped, "the open sweep must have locked and swept this player");

            let invite = tokio::spawn({
                let racer = racer_service(&svc, &dir, &invite_app);
                let (admin, group_id) = (admin.clone(), group_id.clone());
                async move {
                    racer
                        .invite(Identity::player(&admin), group_id, "LateTarget#0002".into())
                        .await
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked = wait_advisory_parked(&pool, &invite_app, lock_key, &invite).await;
            assert!(parked, "the invite must park on the target's lock while the sweep is open");

            sweep_tx.commit().await.unwrap();
            assert_eq!(
                finish("race-2 invite", invite).await,
                Err((Status::NotFound, NO_SUCH_PLAYER.to_string())),
                "a purged target must get exactly the directory-miss answer"
            );
            assert!(!membership_exists(&pool, &group_id, &target).await, "the refused invite must add no row");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_takes_the_player_lock_before_it_reads_the_tombstone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let pid = fixed_uuid(0x12);
    let lock_key = taken_player_key(&pool, &svc, &pid).await;
    let gate_app = racer_app("guard-gate", &pid);
    let create_app = racer_app("guard-create", &pid);

    let apps = vec![gate_app.clone(), create_app.clone()];
    with_purge_cleanup(&pool, apps, vec![pid.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            assert_eq!(purge_swept_at(&pool, &pid).await, None, "precondition: no tombstone yet");
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            svc.store.lock_player_tx(&mut gate, &pid).await.unwrap();

            let create = tokio::spawn({
                let racer = racer_service(&svc, &dir, &create_app);
                let pid = pid.clone();
                async move {
                    racer
                        .create(Identity::player(&pid), "GuardCreate".into(), JOIN_OPEN.into())
                        .await
                        .map(|g| g.id)
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked = wait_advisory_parked(&pool, &create_app, lock_key, &create).await;
            assert!(parked, "the create must park on the player's lock before its tombstone read");

            plant(&svc, &pool, &pid, "Guard#0001").await;
            gate.rollback().await.unwrap();

            assert_eq!(
                finish("guard-order create", create).await,
                Err((Status::Conflict, PLAYER_DELETED.to_string()))
            );
            let created: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM groups.groups WHERE creator_id = $1::uuid",
            )
            .bind(&pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(created, 0, "a create that read the tombstone before the lock would insert after the release");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_takes_the_player_lock_before_it_reads_the_tombstone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let owner = fixed_uuid(0x13);
    let joiner = fixed_uuid(0x13);
    dir.insert(&owner, "GuardOwner#0001");
    dir.insert(&joiner, "GuardJoiner#0002");
    let lock_key = taken_player_key(&pool, &svc, &joiner).await;
    let gate_app = racer_app("guard-gate", &joiner);
    let join_app = racer_app("guard-join", &joiner);

    let apps = vec![gate_app.clone(), join_app.clone()];
    with_purge_cleanup(&pool, apps, vec![owner.clone(), joiner.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&owner), "GuardJoin".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            assert_eq!(purge_swept_at(&pool, &joiner).await, None, "precondition: no tombstone yet");
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            svc.store.lock_player_tx(&mut gate, &joiner).await.unwrap();

            let join = tokio::spawn({
                let racer = racer_service(&svc, &dir, &join_app);
                let (joiner, group_id) = (joiner.clone(), group_id.clone());
                async move {
                    racer
                        .join(Identity::player(&joiner), group_id)
                        .await
                        .map(|m| m.state)
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked = wait_advisory_parked(&pool, &join_app, lock_key, &join).await;
            assert!(parked, "the join must park on the joiner's lock before its tombstone read");

            plant(&svc, &pool, &joiner, "GuardJoiner#0002").await;
            gate.rollback().await.unwrap();

            assert_eq!(
                finish("guard-order join", join).await,
                Err((Status::Conflict, PLAYER_DELETED.to_string()))
            );
            assert!(
                !membership_exists(&pool, &group_id, &joiner).await,
                "a join that read the tombstone before the lock would insert after the release"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invite_takes_the_target_lock_before_it_reads_the_target_tombstone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let admin = fixed_uuid(0x14);
    let target = fixed_uuid(0x14);
    dir.insert(&admin, "GuardAdmin#0001");
    dir.insert(&target, "GuardTarget#0002");
    let lock_key = taken_player_key(&pool, &svc, &target).await;
    let gate_app = racer_app("guard-gate", &target);
    let invite_app = racer_app("guard-invite", &target);

    let apps = vec![gate_app.clone(), invite_app.clone()];
    with_purge_cleanup(&pool, apps, vec![admin.clone(), target.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&admin), "GuardInvite".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            assert_eq!(purge_swept_at(&pool, &target).await, None, "precondition: no tombstone yet");
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            svc.store.lock_player_tx(&mut gate, &target).await.unwrap();

            let invite = tokio::spawn({
                let racer = racer_service(&svc, &dir, &invite_app);
                let (admin, group_id) = (admin.clone(), group_id.clone());
                async move {
                    racer
                        .invite(Identity::player(&admin), group_id, "GuardTarget#0002".into())
                        .await
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked = wait_advisory_parked(&pool, &invite_app, lock_key, &invite).await;
            assert!(parked, "the invite must park on the target's lock before its tombstone read");

            plant(&svc, &pool, &target, "GuardTarget#0002").await;
            gate.rollback().await.unwrap();

            assert_eq!(
                finish("guard-order invite", invite).await,
                Err((Status::NotFound, NO_SUCH_PLAYER.to_string())),
                "a purged target must get exactly the directory-miss answer"
            );
            assert!(
                !membership_exists(&pool, &group_id, &target).await,
                "an invite that read the tombstone before the lock would insert after the release"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_takes_every_player_lock_before_a_group_lock() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let a = fixed_uuid(0x0e);
    let b = fixed_uuid(0x0f);
    assert!(a < b);
    dir.insert(&a, "OrderA#0001");
    dir.insert(&b, "OrderB#0002");
    let b_key = taken_player_key(&pool, &svc, &b).await;
    let gate_app = racer_app("order-gate", &b);
    let sweep_app = racer_app("order-sweep", &b);
    let join_app = racer_app("order-join", &b);

    let apps = vec![gate_app.clone(), sweep_app.clone(), join_app.clone()];
    with_purge_cleanup(&pool, apps, vec![a.clone(), b.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let g1 = svc
                .create(Identity::player(&a), "OrderG1".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let g1_key = taken_group_key(&pool, &svc, &g1).await;

            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            sqlx::query(
                "UPDATE groups.memberships SET role = role \
                  WHERE group_id = $1::uuid AND player_id = $2::uuid",
            )
            .bind(&g1)
            .bind(&a)
            .execute(&mut *gate)
            .await
            .unwrap();
            let gate_xid = current_xid(&mut gate).await;

            plant(&svc, &pool, &a, "OrderA#0001").await;
            plant(&svc, &pool, &b, "OrderB#0002").await;
            assert_claimed_by_the_next_fire(&pool, &b).await;

            let sweep = spawn_sweep(&svc, &sweep_app);
            assert!(
                wait_xact_parked(&pool, &sweep_app, &gate_xid, &sweep).await,
                "the sweep must be held at a's delete in G1, holding G1"
            );

            let join = tokio::spawn({
                let racer = racer_service(&svc, &dir, &join_app);
                let (b, g1) = (b.clone(), g1.clone());
                async move {
                    racer
                        .join(Identity::player(&b), g1)
                        .await
                        .map(|m| m.state)
                        .map_err(|e| (e.status, e.msg))
                }
            });
            let parked_on = parked_on_advisory(&pool, &join_app, &[b_key, g1_key], &join).await;
            assert!(parked_on.is_some(), "the join must park behind the sweep");

            gate.rollback().await.unwrap();
            let outcome = (finish("order sweep", sweep).await, finish("order join", join).await);
            assert_eq!(
                outcome,
                (Ok(()), Err((Status::Conflict, PLAYER_DELETED.to_string()))),
                "neither side may fail with a deadlock (40P01)"
            );
            assert_eq!(parked_on, Some(b_key), "the join must park on b's lock, taken before G1");
            assert!(!group_exists(&pool, &g1).await, "a's purge must dissolve G1");
            assert!(!membership_exists(&pool, &g1, &b).await);
            assert!(swept(&pool, &a).await && swept(&pool, &b).await, "both players must be purged");
        }
    })
    .await;
}

#[tokio::test]
async fn invite_to_a_full_group_answers_no_such_player_for_a_purged_target() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let admin = fixed_uuid(0x10);
    let target = fixed_uuid(0x10);
    let live = fixed_uuid(0x10);
    dir.insert(&admin, "FullAdmin#0001");
    dir.insert(&target, "FullTarget#0002");
    dir.insert(&live, "FullLive#0003");

    with_purge_cleanup(&pool, vec![], vec![admin.clone(), target.clone(), live.clone()], {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&admin), "FullGroup".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            sqlx::query(
                "INSERT INTO groups.memberships (group_id, player_id, state, role) \
                 SELECT $1::uuid, gen_random_uuid(), 'invited', '' FROM generate_series(2, $2)",
            )
            .bind(&group_id)
            .bind(MAX_MEMBERS)
            .execute(&pool)
            .await
            .unwrap();
            plant(&svc, &pool, &target, "FullTarget#0002").await;

            let full = svc
                .invite(Identity::player(&admin), group_id.clone(), "FullLive#0003".into())
                .await
                .unwrap_err();
            assert_eq!(
                (full.status, full.msg.as_str()),
                (Status::Conflict, format!("group is full at {MAX_MEMBERS} members").as_str()),
                "control: the group must be full"
            );

            let e = svc
                .invite(Identity::player(&admin), group_id.clone(), "FullTarget#0002".into())
                .await
                .unwrap_err();
            assert_eq!(
                (e.status, e.msg.as_str()),
                (Status::NotFound, NO_SUCH_PLAYER),
                "a purged target must get the directory-miss answer, not \"full\""
            );
            cleanup_groups(&pool, &[group_id]).await;
        }
    })
    .await;
}

#[tokio::test]
async fn one_sweep_fire_stamps_at_most_64_players() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let mut ids: Vec<String> = (0..65).map(|_| fixed_uuid(0x00)).collect();
    ids.sort();

    with_purge_cleanup(&pool, vec![], ids.clone(), {
        let (pool, svc, ids) = (pool.clone(), svc.clone(), ids.clone());
        async move {
            for (i, id) in ids.iter().enumerate() {
                plant(&svc, &pool, id, &format!("Cap#{i:04}")).await;
            }

            assert_next_fire_claims_only(&pool, &ids, 64).await;
            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                unstamped_tombstones(&pool, &ids).await,
                vec![ids[64].clone()],
                "one fire must stamp exactly the 64 lowest of 65 tombstones"
            );

            assert_next_fire_claims_only(&pool, &ids, 1).await;
            let mut tx2 = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx2).await.unwrap();
            tx2.commit().await.unwrap();
            assert!(
                unstamped_tombstones(&pool, &ids).await.is_empty(),
                "the next fire must resume and stamp the last one"
            );
        }
    })
    .await;
}

async fn skipped_at(pool: &PgPool, player_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT skipped_at::text FROM groups.purged_players WHERE player_id = $1::uuid")
        .bind(player_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn sweep_under_lock_timeout(pool: &PgPool, svc: &Service) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '200ms'").execute(&mut *tx).await.unwrap();
    svc.sweep_purged_players(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_skipped_player_rotates_behind_fresh_tombstones_and_is_purged_later() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let skipped = fixed_uuid(0x00);
    let mut fresh: Vec<String> = (0..GROUPS_PLAYERS_PER_FIRE).map(|_| fixed_uuid(0x15)).collect();
    fresh.sort();
    let late = fixed_uuid(0x15);
    assert!(skipped < fresh[0] && skipped < late);
    let mut ours = fresh.clone();
    ours.extend([skipped.clone(), late.clone()]);
    let gate_app = racer_app("rotation-gate", &skipped);

    with_purge_cleanup(&pool, vec![gate_app.clone()], ours.clone(), {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let group_id = svc
                .create(Identity::player(&skipped), "RotationGroup".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let group_key = taken_group_key(&pool, &svc, &group_id).await;
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(group_key)
                .execute(&mut *gate)
                .await
                .unwrap();

            plant(&svc, &pool, &skipped, "Rotation#0000").await;
            assert_next_fire_claims_only(&pool, &ours, 1).await;
            assert_claimed_by_the_next_fire(&pool, &skipped).await;
            sweep_under_lock_timeout(&pool, &svc).await;
            let first_skip = skipped_at(&pool, &skipped).await;
            assert!(first_skip.is_some(), "a group error must stamp skipped_at");
            assert!(!swept(&pool, &skipped).await, "a skipped player must stay unswept");
            assert!(group_exists(&pool, &group_id).await, "the failed group must be rolled back");

            for (i, id) in fresh.iter().enumerate() {
                plant(&svc, &pool, id, &format!("Rotation#{:04}", i + 1)).await;
            }
            assert_next_fire_claims_only(&pool, &ours, 64).await;
            sweep_under_lock_timeout(&pool, &svc).await;
            assert!(
                unstamped_tombstones(&pool, &fresh).await.is_empty(),
                "the fire must claim every fresh tombstone ahead of the skipped one"
            );
            assert_eq!(
                skipped_at(&pool, &skipped).await,
                first_skip,
                "the skipped player must not be claimed while fresh tombstones fill the fire"
            );
            assert!(!swept(&pool, &skipped).await);

            let late_group = svc
                .create(Identity::player(&late), "RotationLate".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            plant(&svc, &pool, &late, "Rotation#0065").await;
            gate.rollback().await.unwrap();
            assert_next_fire_claims_only(&pool, &ours, 2).await;
            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();
            assert!(swept(&pool, &skipped).await, "the released skipped player must be purged");
            assert!(swept(&pool, &late).await);
            assert!(!group_exists(&pool, &group_id).await && !group_exists(&pool, &late_group).await);
            let purge_order: Vec<String> = sqlx::query_scalar(
                "SELECT payload->>'player_id' FROM asyncevents.events \
                  WHERE payload->>'reason' = $1 AND payload->>'player_id' = ANY($2) \
                  ORDER BY tie_breaker",
            )
            .bind(groupsevents::REASON_ACCOUNT_DELETED)
            .bind(vec![skipped.clone(), late.clone()])
            .fetch_all(&pool)
            .await
            .unwrap();
            assert_eq!(
                purge_order,
                vec![late.clone(), skipped.clone()],
                "the fire must purge in claim order, the fresh tombstone before the skipped one"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn the_oldest_skipped_player_is_reclaimed_first() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool, Arc::new(FakeDirectory::new())).await;
    let older = fixed_uuid(0x16);
    let newer = fixed_uuid(0x16);
    let filler: Vec<String> = (0..GROUPS_PLAYERS_PER_FIRE - 1).map(|_| fixed_uuid(0x17)).collect();
    let fresh: Vec<String> = (0..GROUPS_PLAYERS_PER_FIRE - 1).map(|_| fixed_uuid(0x18)).collect();
    let mut ours = filler.clone();
    ours.extend(fresh.iter().cloned());
    ours.extend([older.clone(), newer.clone()]);
    let gate_app = racer_app("oldest-skipped-gate", &older);

    with_purge_cleanup(&pool, vec![gate_app.clone()], ours.clone(), {
        let (pool, svc) = (pool.clone(), svc.clone());
        async move {
            let older_group = svc
                .create(Identity::player(&older), "OldestSkipped".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let newer_group = svc
                .create(Identity::player(&newer), "NewerSkipped".into(), JOIN_OPEN.into())
                .await
                .unwrap()
                .id;
            let older_key = taken_group_key(&pool, &svc, &older_group).await;
            let newer_key = taken_group_key(&pool, &svc, &newer_group).await;
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            for key in [older_key, newer_key] {
                sqlx::query("SELECT pg_advisory_xact_lock($1)")
                    .bind(key)
                    .execute(&mut *gate)
                    .await
                    .unwrap();
            }

            plant(&svc, &pool, &older, "Oldest#0000").await;
            assert_next_fire_claims_only(&pool, &ours, 1).await;
            sweep_under_lock_timeout(&pool, &svc).await;
            let older_skip = skipped_at(&pool, &older).await;
            assert!(
                older_skip.is_some(),
                "the gated older player must be skipped by the first fire"
            );

            plant(&svc, &pool, &newer, "Oldest#0001").await;
            for (i, id) in filler.iter().enumerate() {
                plant(&svc, &pool, id, &format!("Oldest#{:04}", i + 2)).await;
            }
            assert_next_fire_claims_only(&pool, &ours, 64).await;
            sweep_under_lock_timeout(&pool, &svc).await;
            assert!(unstamped_tombstones(&pool, &filler).await.is_empty());
            let newer_skip = skipped_at(&pool, &newer).await;
            assert!(
                newer_skip.is_some(),
                "the gated newer player must be skipped by the second fire"
            );
            assert_eq!(
                skipped_at(&pool, &older).await,
                older_skip,
                "the second fire is full of fresh tombstones and must not reclaim the older player"
            );
            let strictly_older: bool = sqlx::query_scalar(
                "SELECT o.skipped_at < n.skipped_at \
                   FROM groups.purged_players o, groups.purged_players n \
                  WHERE o.player_id = $1::uuid AND n.player_id = $2::uuid",
            )
            .bind(&older)
            .bind(&newer)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(
                strictly_older,
                "precondition: the older player's skipped_at must sort strictly first"
            );

            for (i, id) in fresh.iter().enumerate() {
                plant(&svc, &pool, id, &format!("Oldest#{:04}", i + 65)).await;
            }
            assert_next_fire_claims_only(&pool, &ours, 64).await;
            sweep_under_lock_timeout(&pool, &svc).await;
            assert!(
                unstamped_tombstones(&pool, &fresh).await.is_empty(),
                "the fire must claim every fresh tombstone first"
            );
            let reskip = skipped_at(&pool, &older).await;
            assert!(
                reskip.is_some() && reskip != older_skip,
                "the last slot must reclaim the oldest skipped player"
            );
            assert_eq!(
                skipped_at(&pool, &newer).await,
                newer_skip,
                "the newer skipped player must not take the last slot"
            );
            assert!(!swept(&pool, &newer).await && !swept(&pool, &older).await);
            gate.rollback().await.unwrap();
        }
    })
    .await;
}

async fn holds_advisory(pool: &PgPool, app: &str, key: i64) -> bool {
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
          WHERE a.application_name = $1 AND l.granted AND l.locktype = 'advisory' \
            AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = $2",
    )
    .bind(app)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap();
    held > 0
}

fn probe_holds_advisory(app: &str, key: i64) -> bool {
    std::thread::spawn({
        let app = app.to_owned();
        move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let probe = PgPool::connect(&testdb::dsn()).await.unwrap();
                    holds_advisory(&probe, &app, key).await
                })
        }
    })
    .join()
    .unwrap()
}

#[tokio::test]
async fn invite_releases_both_locks_before_it_answers_a_purged_target() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let dir = Arc::new(FakeDirectory::new());
    let (_ctx, svc) = wired(&pool, dir.clone()).await;
    let admin = fixed_uuid(0x10);
    let target = fixed_uuid(0x10);
    dir.insert(&admin, "ReleaseAdmin#0001");
    dir.insert(&target, "ReleaseTarget#0002");
    let player_key = taken_player_key(&pool, &svc, &target).await;
    let app = racer_app("release", &target);

    let group_id = svc
        .create(Identity::player(&admin), "ReleaseGroup".into(), JOIN_OPEN.into())
        .await
        .unwrap()
        .id;
    let group_key = taken_group_key(&pool, &svc, &group_id).await;
    plant(&svc, &pool, &target, "ReleaseTarget#0002").await;

    let racer = racer_service(&svc, &dir, &app);
    let mut control = racer.store.pool.begin().await.unwrap();
    racer.store.lock_player_tx(&mut control, &target).await.unwrap();
    racer.store.lock_group_tx(&mut control, &group_id).await.unwrap();
    let seen = (probe_holds_advisory(&app, player_key), probe_holds_advisory(&app, group_key));
    control.rollback().await.unwrap();

    let invited = racer
        .invite(Identity::player(&admin), group_id.clone(), "ReleaseTarget#0002".into())
        .await;
    let held = (probe_holds_advisory(&app, player_key), probe_holds_advisory(&app, group_key));
    drop(racer);
    cleanup_players(&pool, &[admin.clone(), target.clone()]).await;

    assert_eq!(seen, (true, true), "the probe must see the racer's held locks");
    let Err(e) = invited else { panic!("an invite of a purged target must be refused") };
    assert_eq!((e.status, e.msg.as_str()), (Status::NotFound, NO_SUCH_PLAYER));
    assert_eq!(held, (false, false), "a refused invite must not leave its transaction's locks held");
}

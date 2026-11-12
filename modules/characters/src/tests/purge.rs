//! Steps 5-7 of the account-deletion plan for `characters`: the purge tombstone
//! (Step 5), its scheduled sweep (Step 6), and the sweep's write guard (Step 7). A
//! child of `tests` so it reuses that module's live-DB harness
//! (`test_pool`/`wired`/`unique_player`/`cleanup`).

use super::*;
use bus::{AnyTx, Delivery, TxHandler};

/// ONE lock for every test that plants or sweeps `characters.purged_players`.
/// `sweep_purged_players` walks the WHOLE shared table with `FOR UPDATE SKIP LOCKED`, so a
/// sibling's open sweep transaction holds row locks on this test's planted rows and this
/// test's sweep silently skips them, leaving them unswept when it asserts. Serializing the
/// tests keeps exactly one sweeper alive at a time. Process-local: a runner that gives each
/// test its own process (nextest) loses the isolation.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs `body` on its own task and removes `players`' characters and events plus every
/// `tombstones` row even when it panicked, then re-raises the panic (a `Drop` guard cannot
/// `await`). A red run must never leave unswept tombstones in the shared table for the next
/// run's sweeps to trip over.
async fn with_purge_cleanup<Fut>(
    pool: &PgPool,
    players: Vec<String>,
    tombstones: Vec<String>,
    body: Fut,
)
where
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    let refs: Vec<&str> = players.iter().map(String::as_str).collect();
    cleanup(pool, &refs).await;
    for pid in &tombstones {
        cleanup_purge_row(pool, pid).await;
    }
    if let Err(e) = outcome {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        panic!("characters purge test task ended without completing: {e}");
    }
}

async fn purge_row(pool: &PgPool, player_id: &str) -> Option<(String, bool)> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT handle, swept_at::text FROM characters.purged_players WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .unwrap();
    row.map(|(handle, swept_at)| (handle, swept_at.is_some()))
}

async fn stamp_swept(pool: &PgPool, player_id: &str) {
    sqlx::query("UPDATE characters.purged_players SET swept_at = now() WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn cleanup_purge_row(pool: &PgPool, player_id: &str) {
    let _ = sqlx::query("DELETE FROM characters.purged_players WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await;
}

fn deliver_payload(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap()
}

// ============================================================================
// Step 5 — the tombstone: planted once, redelivery is a no-op that never resets
// `swept_at`; a faulted (rolled-back) delivery leaves no row.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn plant_purged_player_is_idempotent_and_never_resets_swept_at() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    with_purge_cleanup(&pool, vec![pid.clone()], vec![pid.clone()], {
        let pool = pool.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();

            let (n,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM characters.purged_players WHERE player_id = $1::uuid",
            )
            .bind(&pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "a redelivered tombstone must not insert a second row");

            stamp_swept(&pool, &pid).await;
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Handle#0001").await.unwrap();
            let (_, swept) = purge_row(&pool, &pid).await.unwrap();
            assert!(swept, "a redelivery after the sweep stamped swept_at must NOT reset it");
        }
    })
    .await;
}

/// A faulted delivery (the containing transaction rolls back) must leave no tombstone row
/// AND — since the plane's checkpoint advance shares the same delivery transaction — no
/// advanced checkpoint either; this test pins the DB-row half directly, the checkpoint half
/// is a plane invariant proven once at the plane level, not re-provable from a module's own
/// unit test.
#[tokio::test(flavor = "multi_thread")]
async fn plant_purged_player_rollback_leaves_no_row() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let pid = unique_player(&pool).await;
    let store = Store { pool: pool.clone() };

    with_purge_cleanup(&pool, vec![pid.clone()], vec![pid.clone()], {
        let pool = pool.clone();
        async move {
            let mut tx = pool.begin().await.unwrap();
            store.plant_purged_player_tx(&mut tx, &pid, "Rollback#0001").await.unwrap();
            tx.rollback().await.unwrap();

            assert!(
                purge_row(&pool, &pid).await.is_none(),
                "a faulted delivery (rolled back) must leave no tombstone row"
            );
        }
    })
    .await;
}

// ============================================================================
// Step 6 — the sweep: the target is fully drained (characters gone, one
// `character.deleted` per row from RETURNING), the survivor is untouched AND emits
// nothing, a redelivery emits nothing more, and the name guard sweeps nothing for a
// foreign schedule (with a positive control on the same row).
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn sweep_deletes_target_emits_once_per_row_survivor_untouched_redelivery_noop() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;

    let target = unique_player(&pool).await;
    let survivor = unique_player(&pool).await;

    let both = vec![target.clone(), survivor.clone()];
    with_purge_cleanup(&pool, both.clone(), both, {
        let pool = pool.clone();
        let svc = svc.clone();
        let target = target.clone();
        let survivor = survivor.clone();
        async move {
            svc.create(Identity::player(&target), "T1".into(), String::new()).await.unwrap();
            svc.create(Identity::player(&target), "T2".into(), String::new()).await.unwrap();
            svc.create(Identity::player(&survivor), "S1".into(), String::new()).await.unwrap();

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &target, "Target#0001").await.unwrap();
            drop(conn);

            // First fire: drains the target, leaves the survivor alone.
            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            assert_eq!(char_count_by_player(&pool, &target).await, 0, "the target's characters must be gone");
            assert_eq!(
                char_count_by_player(&pool, &survivor).await,
                1,
                "the survivor's character must be untouched"
            );
            assert_eq!(
                events_count_by_player(&pool, "character.deleted", &target).await,
                2,
                "one character.deleted per deleted row"
            );
            assert_eq!(
                events_count_by_player(&pool, "character.deleted", &survivor).await,
                0,
                "the survivor must emit NOTHING"
            );
            let (_, swept) = purge_row(&pool, &target).await.unwrap();
            assert!(swept, "the target's tombstone must be stamped swept_at after the sweep");

            // Redelivery: `swept_at IS NULL` excludes the target now, so a second fire emits
            // nothing more — the emits come ONLY from `DELETE ... RETURNING`.
            let mut tx2 = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx2).await.unwrap();
            tx2.commit().await.unwrap();
            assert_eq!(
                events_count_by_player(&pool, "character.deleted", &target).await,
                2,
                "a redelivery must emit NOTHING more"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_handler_ignores_a_foreign_schedule_name_with_positive_control() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let target = unique_player(&pool).await;
    with_purge_cleanup(&pool, vec![target.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let target = target.clone();
        async move {
            svc.create(Identity::player(&target), "Foreign".into(), String::new()).await.unwrap();

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &target, "Foreign#0001").await.unwrap();
            drop(conn);

            let handler = projection::PurgeSweepHandler { svc: svc.clone() };

            // Negative: a foreign schedule name must sweep NOTHING.
            let mut tx = pool.begin().await.unwrap();
            let delivery = Delivery { event_id: "characters:test", tx: AnyTx::new(&mut *tx) };
            handler.call(delivery, deliver_payload("some-other-job")).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                char_count_by_player(&pool, &target).await,
                1,
                "a foreign schedule name must not sweep"
            );

            // Positive control, same row: the real name drains it.
            let mut tx2 = pool.begin().await.unwrap();
            let delivery2 = Delivery { event_id: "characters:test", tx: AnyTx::new(&mut *tx2) };
            handler
                .call(delivery2, deliver_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            tx2.commit().await.unwrap();
            assert_eq!(
                char_count_by_player(&pool, &target).await,
                0,
                "the real schedule name must sweep the same row"
            );
        }
    })
    .await;
}

/// Forces a second `PURGE_BATCH` (256) iteration in ONE fire, plus the reserved nil uuid
/// as the floor's own starting value — proving both that the loop crosses a batch boundary
/// within one delivery (pre-fix: a single, unlooped batch would leave the tail unswept) and
/// that the floor comparison is `>=` (inclusive), not `>` (which would permanently skip the
/// nil uuid — the loop's own starting floor value).
#[tokio::test(flavor = "multi_thread")]
async fn sweep_batch_loop_crosses_the_boundary_and_the_nil_uuid_floor_is_inclusive() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;

    let mut ids: Vec<String> = vec!["00000000-0000-0000-0000-000000000000".to_string()];
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT gen_random_uuid()::text FROM generate_series(1, 299)")
            .fetch_all(&pool)
            .await
            .unwrap();
    ids.extend(rows.into_iter().map(|(id,)| id));

    // A crashed earlier run may have left the nil-uuid tombstone, which the plant below
    // would hit as a primary-key conflict.
    cleanup_purge_row(&pool, &ids[0]).await;

    with_purge_cleanup(&pool, vec![], ids.clone(), {
        let pool = pool.clone();
        let ids = ids.clone();
        async move {
            for id in &ids {
                sqlx::query("INSERT INTO characters.purged_players (player_id, handle) VALUES ($1::uuid, 'x')")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            let (remaining,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM characters.purged_players \
                  WHERE swept_at IS NULL AND player_id = ANY($1::uuid[])",
            )
            .bind(&ids)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(remaining, 0, "one fire must drain past a single 256-row batch boundary");

            let (nil_swept,): (Option<String>,) = sqlx::query_as(
                "SELECT swept_at::text FROM characters.purged_players \
                  WHERE player_id = '00000000-0000-0000-0000-000000000000'::uuid",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(
                nil_swept.is_some(),
                "the nil-uuid floor value itself must be reachable (>=, not >)"
            );
        }
    })
    .await;
}

// ============================================================================
// Step 7 — the write guard: a tombstoned player's `create` is Conflict with no row; a
// DIFFERENT, non-tombstoned player still succeeds despite a tombstone existing (the
// predicate is keyed on identity, not "a tombstone exists somewhere").
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn create_is_blocked_by_tombstone_predicate_not_presence() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;

    let purged = unique_player(&pool).await;
    let other = unique_player(&pool).await;

    let both = vec![purged.clone(), other.clone()];
    with_purge_cleanup(&pool, both.clone(), both, {
        let pool = pool.clone();
        let svc = svc.clone();
        let purged = purged.clone();
        let other = other.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &purged, "Purged#0001").await.unwrap();
            drop(conn);

            let e = svc
                .create(Identity::player(&purged), "Ghost".into(), String::new())
                .await
                .unwrap_err();
            assert_eq!(e.status, Status::Conflict);
            assert_eq!(
                char_count_by_player(&pool, &purged).await,
                0,
                "no character row for a tombstoned player"
            );

            svc.create(Identity::player(&other), "Alive".into(), String::new())
                .await
                .expect("a different, non-tombstoned player must still succeed");
            assert_eq!(char_count_by_player(&pool, &other).await, 1);
        }
    })
    .await;
}

//! Steps 5-7 of the account-deletion plan for `characters`: the purge tombstone
//! (Step 5), its scheduled sweep (Step 6), and the sweep's write guard (Step 7). A
//! child of `tests` so it reuses that module's live-DB harness
//! (`test_pool`/`wired`/`unique_player`/`cleanup`).

use super::*;
use std::time::Duration;

use bus::{AnyTx, Delivery, TxHandler};

/// Serializes the tests that plant or sweep `characters.purged_players`: the whole-table `FOR UPDATE SKIP LOCKED` sweep skips rows a sibling's open sweep has locked.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A red run must not leave unswept tombstones, and a `Drop` guard cannot `await`. On a failure
/// or a hang the backends of `apps` are terminated first: a racer parked server-side keeps its
/// row locks after its future is gone, and the cleanup DELETE would wait on them.
async fn with_purge_cleanup<Fut>(
    pool: &PgPool,
    apps: Vec<String>,
    players: Vec<String>,
    tombstones: Vec<String>,
    body: Fut,
)
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
    let refs: Vec<&str> = players.iter().map(String::as_str).collect();
    cleanup(pool, &refs).await;
    for pid in &tombstones {
        cleanup_purge_row(pool, pid).await;
    }
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Ok(Err(e)) => panic!("characters purge test task ended without completing: {e}"),
        Err(_) => panic!("characters purge test body did not finish within 60s"),
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

/// A fixed player id that sorts below every random tombstone: one fire claims only the
/// `CHARACTERS_PLAYERS_PER_FIRE` lowest unswept ids, so a sweep test's own player must be
/// among them. Each sweep test owns a distinct `n` range.
fn fixed_player(n: u64) -> String {
    format!("00000000-0000-0000-0000-{n:012x}")
}

/// Drops whatever a crashed earlier run left under a fixed id, so the test starts with no
/// characters and an absent tombstone.
async fn clear_fixed_player(pool: &PgPool, player_id: &str) {
    cleanup(pool, &[player_id]).await;
    cleanup_purge_row(pool, player_id).await;
}

/// Precondition of a sweep test: one fire claims only the `CHARACTERS_PLAYERS_PER_FIRE`
/// lowest unswept tombstones, so leftovers sorting below `player_id` would starve it and the
/// test would fail as a purge that never happened.
async fn assert_claimed_by_the_next_fire(pool: &PgPool, player_id: &str) {
    let below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM characters.purged_players \
          WHERE swept_at IS NULL AND player_id < $1::uuid",
    )
    .bind(player_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        below < projection::CHARACTERS_PLAYERS_PER_FIRE,
        "precondition: {below} unswept tombstones sort below {player_id}, so the next fire \
         cannot claim it"
    );
}

// Concurrency is staged, never slept: a racer runs on its OWN pool whose connections carry a
// unique `application_name`, so a test waits on `pg_stat_activity` until the racer is parked
// on the lock the test holds, then releases it.

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

/// A service over `racer_pool(app)` sharing `svc`'s bus, so its `create` runs on a
/// connection the test can find in `pg_stat_activity`.
fn racer_service(svc: &Service, app: &str) -> Arc<Service> {
    let racer = Service {
        store: Store { pool: racer_pool(app) },
        bus: svc.bus.clone(),
        config: OnceLock::new(),
    };
    let _ = racer.config.set(Arc::new(FakeConfig { cap: LIST_HARD_LIMIT }));
    Arc::new(racer)
}

/// A unique `application_name` for one racer of one test run.
async fn racer_app(pool: &PgPool, what: &str) -> String {
    format!("characters-{what}-{}", unique_player(pool).await)
}

/// Awaits a spawned racer with a hang guard, so a deadlocked race fails with a name instead
/// of stalling the run.
async fn finish<T>(what: &str, handle: tokio::task::JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("{what} task did not complete: {e}"),
        Err(_) => panic!("{what} did not finish within 30s"),
    }
}

/// Polls observed database state until a backend of `app` waits on the advisory lock `key`
/// (`true`), or `racer` finished without ever parking (`false`). The second answer is how an
/// unserialized racer shows itself; a racer parked on any OTHER lock reaches the deadline,
/// which is a hang guard, not a timing bound.
async fn wait_advisory_parked<T>(
    pool: &PgPool,
    app: &str,
    key: i64,
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'advisory' \
                AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = $2",
        )
        .bind(app)
        .bind(key)
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
            "racer {app} neither parked on the advisory lock {key} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// FNV-1a over `prefix` + `body`; recomputed here so the test pins the key formula.
fn fnv1a_key(prefix: &[u8], body: &[u8]) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in prefix.iter().chain(body.iter()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h as i64
}

/// The advisory key `Store::lock_player_tx` actually takes for `spelling`, read back from
/// `pg_locks` on the locking connection (a bigint key is split into `classid` high and `objid`
/// low halves, `objsubid = 1`).
async fn taken_player_key(pool: &PgPool, svc: &Service, spelling: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    svc.store.lock_player_tx(&mut tx, spelling).await.unwrap();
    let keys: Vec<i64> = sqlx::query_scalar(
        "SELECT (classid::bigint << 32) | objid::bigint FROM pg_locks \
          WHERE locktype = 'advisory' AND objsubid = 1 AND pid = pg_backend_pid()",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(keys.len(), 1, "lock_player_tx must hold exactly one advisory lock, got {keys:?}");
    keys[0]
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

    with_purge_cleanup(&pool, vec![], vec![pid.clone()], vec![pid.clone()], {
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

    with_purge_cleanup(&pool, vec![], vec![pid.clone()], vec![pid.clone()], {
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

    let target = fixed_player(0x100);
    clear_fixed_player(&pool, &target).await;
    let survivor = unique_player(&pool).await;

    let both = vec![target.clone(), survivor.clone()];
    with_purge_cleanup(&pool, vec![], both.clone(), both, {
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
            assert_claimed_by_the_next_fire(&pool, &target).await;

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
    let target = fixed_player(0x110);
    clear_fixed_player(&pool, &target).await;
    with_purge_cleanup(&pool, vec![], vec![target.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let target = target.clone();
        async move {
            svc.create(Identity::player(&target), "Foreign".into(), String::new()).await.unwrap();

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &target, "Foreign#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

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
    with_purge_cleanup(&pool, vec![], both.clone(), both, {
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

// ============================================================================
// Character creation and the purge sweep serialize on one per-player advisory lock.
// ============================================================================

/// The key is read back from the lock `lock_player_tx` really takes: the pinned
/// `characters.player/` FNV-1a of the 32 lowercase hex digits, the same for every spelling
/// `$1::uuid` treats as one player, and different for a different uuid.
#[tokio::test(flavor = "multi_thread")]
async fn player_lock_key_is_stable_namespaced_and_spelling_invariant() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let canon = "a2b7e8c1-0000-4000-8000-0000000000f1";
    let hex = canon.replace('-', "");

    let key = taken_player_key(&pool, &svc, canon).await;
    assert_eq!(
        key,
        fnv1a_key(b"characters.player/", hex.as_bytes()),
        "the key must be the characters.player/ FNV-1a of the normalized uuid"
    );
    assert_eq!(
        key,
        taken_player_key(&pool, &svc, &canon.to_uppercase()).await,
        "case must not change the key"
    );
    assert_eq!(
        key,
        taken_player_key(&pool, &svc, &format!("{{{canon}}}")).await,
        "braces must not change the key"
    );
    assert_eq!(key, taken_player_key(&pool, &svc, &hex).await, "hyphenation must not change the key");
    assert_ne!(
        key,
        taken_player_key(&pool, &svc, "a2b7e8c1-0000-4000-8000-0000000000f2").await,
        "a genuinely different uuid must take a different key"
    );
}

/// Race 1, create first — the once-wrong branch. A real `create` has passed its tombstone
/// guard and written its INSERT, uncommitted, and is held at its event append by the test's
/// EXCLUSIVE hold on the plane's writer key, when the tombstone commits and a sweep fires.
/// The sweep's DELETE cannot see the uncommitted row, so without the per-player lock it
/// stamped `swept_at` around it and the character outlived the erasure. Now the sweep parks
/// on the player's key until `create` commits, then deletes the character and cascades it.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_create_and_purges_it() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = fixed_player(0x200);
    clear_fixed_player(&pool, &pid).await;
    let lock_key = taken_player_key(&pool, &svc, &pid).await;
    let gate_app = racer_app(&pool, "race1-gate").await;
    let create_app = racer_app(&pool, "race1-create").await;
    let sweep_app = racer_app(&pool, "race1-sweep").await;

    let apps = vec![gate_app.clone(), create_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, vec![pid.clone()], vec![pid.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let pid = pid.clone();
        async move {
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(asyncevents::store::WRITER_LOCK_KEY)
                .execute(&mut *gate)
                .await
                .unwrap();

            let create = tokio::spawn({
                let racer = racer_service(&svc, &create_app);
                let pid = pid.clone();
                async move { racer.create(Identity::player(&pid), "Inflight".into(), String::new()).await }
            });
            assert!(
                wait_advisory_parked(&pool, &create_app, asyncevents::store::WRITER_LOCK_KEY, &create)
                    .await,
                "create must be held at its event append, past its guard and INSERT"
            );

            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Race#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &pid).await;

            let sweep = tokio::spawn({
                let handler = projection::PurgeSweepHandler { svc: svc.clone() };
                let racer = racer_pool(&sweep_app);
                async move {
                    let mut tx = racer.begin().await.unwrap();
                    let delivery = Delivery { event_id: "characters:race1", tx: AnyTx::new(&mut *tx) };
                    handler
                        .call(delivery, deliver_payload(projection::PURGE_SCHEDULE_NAME))
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            });
            let parked = wait_advisory_parked(&pool, &sweep_app, lock_key, &sweep).await;

            gate.commit().await.unwrap();
            let c = finish("race-1 create", create)
                .await
                .expect("the create passed its guard before the tombstone");
            finish("race-1 sweep", sweep).await;

            assert_eq!(
                char_count_by_player(&pool, &pid).await,
                0,
                "the in-flight create's character survived the erasure"
            );
            assert_eq!(
                event_count(&pool, "character.deleted", &c.id).await,
                1,
                "the sweep must cascade the in-flight character with one character.deleted"
            );
            let (_, swept) = purge_row(&pool, &pid).await.unwrap();
            assert!(swept, "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the player's advisory lock");
        }
    })
    .await;
}

/// Race 2, sweep first — a lock-order check: the tombstone is already committed, so even an
/// unlocked `create` would answer `PLAYER_DELETED`; without the per-player lock only the
/// "create parks" assertion fails. An open sweep transaction has locked and swept the player;
/// a racer `create` parks on that lock, then, once the sweep commits, answers 409
/// `PLAYER_DELETED` and writes no row.
#[tokio::test(flavor = "multi_thread")]
async fn create_parks_behind_an_open_sweep_then_answers_player_deleted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = fixed_player(0x300);
    clear_fixed_player(&pool, &pid).await;
    let lock_key = taken_player_key(&pool, &svc, &pid).await;
    let sweep_app = racer_app(&pool, "race2-sweep").await;
    let create_app = racer_app(&pool, "race2-create").await;

    let apps = vec![sweep_app.clone(), create_app.clone()];
    with_purge_cleanup(&pool, apps, vec![pid.clone()], vec![pid.clone()], {
        let pool = pool.clone();
        let svc = svc.clone();
        let pid = pid.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            svc.store.plant_purged_player_tx(&mut conn, &pid, "Race#0002").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &pid).await;

            let sweep_pool = racer_pool(&sweep_app);
            let mut sweep_tx = sweep_pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut sweep_tx).await.unwrap();
            let stamped: bool = sqlx::query_scalar(
                "SELECT swept_at IS NOT NULL FROM characters.purged_players \
                  WHERE player_id = $1::uuid",
            )
            .bind(&pid)
            .fetch_one(&mut *sweep_tx)
            .await
            .unwrap();
            assert!(stamped, "the open sweep must have locked and swept this player");

            let create = tokio::spawn({
                let racer = racer_service(&svc, &create_app);
                let pid = pid.clone();
                async move { racer.create(Identity::player(&pid), "Late".into(), String::new()).await }
            });
            let parked = wait_advisory_parked(&pool, &create_app, lock_key, &create).await;
            assert!(parked, "the create must park on the player's lock while the sweep is open");

            sweep_tx.commit().await.unwrap();
            let e = finish("race-2 create", create).await.unwrap_err();
            assert_eq!(e.status, Status::Conflict);
            assert_eq!(e.msg, crate::service::PLAYER_DELETED);
            assert_eq!(
                char_count_by_player(&pool, &pid).await,
                0,
                "the refused create must write no character"
            );
        }
    })
    .await;
}

/// One fire claims and locks at most 64 players. With 65 of this test's own lowest-sorting
/// tombstones, one fire stamps exactly the 64 lowest and leaves the 65th for the next fire.
/// The counts are literal, not derived from `CHARACTERS_PLAYERS_PER_FIRE`, so raising the
/// claim limit or restoring a batch loop turns this red. The sweep's 5s budget is far above
/// 64 players' few statements each, so it does not stop this fire early.
#[tokio::test(flavor = "multi_thread")]
async fn one_sweep_fire_stamps_at_most_64_players() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    // The LOWEST fixed range, below the race tests' ids: a sibling that failed mid-race and
    // left its tombstone unswept must not sort among these.
    let ids: Vec<String> = (0x01..0x01 + 65).map(fixed_player).collect();
    for id in &ids {
        cleanup_purge_row(&pool, id).await;
    }
    let foreign_below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM characters.purged_players \
          WHERE swept_at IS NULL AND player_id <= $1::uuid",
    )
    .bind(ids.last().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        foreign_below, 0,
        "precondition: a foreign unswept tombstone sorts among this test's ids"
    );

    let app = racer_app(&pool, "fire-bound").await;
    with_purge_cleanup(&pool, vec![app.clone()], vec![], ids.clone(), {
        let (pool, svc, ids) = (racer_pool(&app), svc.clone(), ids.clone());
        async move {
            let mut conn = pool.acquire().await.unwrap();
            for (i, id) in ids.iter().enumerate() {
                svc.store
                    .plant_purged_player_tx(&mut conn, id, &format!("Cap#{i:04}"))
                    .await
                    .unwrap();
            }
            drop(conn);

            let mut tx = pool.begin().await.unwrap();
            svc.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                unstamped_tombstones(&pool, &ids).await,
                vec![ids[64].clone()],
                "one fire must stamp exactly the 64 lowest of 65 tombstones"
            );

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

async fn unstamped_tombstones(pool: &PgPool, ids: &[String]) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT player_id::text FROM characters.purged_players \
          WHERE player_id = ANY($1::uuid[]) AND swept_at IS NULL ORDER BY player_id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap()
}

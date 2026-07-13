//! scheduler tests. The exactly-once concurrency test stands up TWO independent
//! replicas (two pools, two buses, two fake transports) against the same DB and drives
//! `fire` concurrently — the advisory lock + `still_due` re-check must yield exactly ONE
//! durable emit and one `last_fired` bump. A second test proves a schedule re-arms after
//! its interval. Both target the live local Postgres (the test DB) and SKIP cleanly
//! (early return + printed message) when it is unreachable, so `cargo test` never
//! hard-fails on a machine without it. In-crate so they can drive the private `fire` /
//! `due_schedules` / `lock_key`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bus::{AnyTx, Bus, Error as BusError, Transport, TxHandler};

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Tests that drive a global due scan/run loop must not see each other's temporary
/// schedules. Direct per-name `fire` tests do not need this serialization.
static TICK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

/// Connects to the test DB and ensures the schema; `None` (with a printed SKIP) when
/// Postgres is unreachable, so the live tests early-return instead of failing.
async fn test_pool() -> Option<PgPool> {
    match PgPool::connect(&dsn()).await {
        Ok(pool) => {
            sqlx::raw_sql(&SCHEMA_DDL)
                .execute(&pool)
                .await
                .expect("migrate scheduler schema");
            Some(pool)
        }
        Err(e) => {
            eprintln!("SKIP scheduler live test: postgres unreachable: {e}");
            None
        }
    }
}

/// A run-unique schedule name so assertions/cleanup never collide on the shared DB.
async fn unique_name(pool: &PgPool) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT 'test-' || gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    s
}

/// Inserts (or resets) a schedule with `last_fired` at the epoch, so it is immediately
/// due (Go's `seedSchedule`).
async fn seed_schedule(pool: &PgPool, name: &str, interval_seconds: i64) {
    sqlx::query(
        "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) \
         VALUES ($1, $2, to_timestamp(0)) \
         ON CONFLICT (name) DO UPDATE SET interval_seconds = $2, last_fired = to_timestamp(0)",
    )
    .bind(name)
    .bind(interval_seconds)
    .execute(pool)
    .await
    .expect("seed schedule");
}

async fn cleanup(pool: &PgPool, name: &str) {
    let _ = sqlx::query("DELETE FROM scheduler.schedules WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await;
}

/// A minimal in-memory `bus::Transport` standing in for the asyncevents plane: it only
/// RECORDS enqueued durable emits (Go's `fakeTransport`), so `fire`'s `emit_tx` has a
/// transport to write into without a live durable plane (which would need a DB these
/// tests shouldn't need). Durable delivery is the asyncevents plane's own concern.
struct FakeTransport {
    rows: Mutex<Vec<(String, Vec<u8>)>>,
    stop_after: Mutex<Option<(String, watch::Sender<bool>)>>,
}

impl FakeTransport {
    fn new() -> Arc<FakeTransport> {
        Arc::new(FakeTransport {
            rows: Mutex::new(Vec::new()),
            stop_after: Mutex::new(None),
        })
    }

    fn stop_after(&self, name: &str, stop: watch::Sender<bool>) {
        *self.stop_after.lock().unwrap() = Some((name.to_string(), stop));
    }

    /// How many enqueued rows carry the given schedule name — the fake-transport-backed
    /// stand-in for the old Go event-count-by-name query.
    fn count(&self, name: &str) -> usize {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, payload)| {
                serde_json::from_slice::<schedulerevents::Fired>(payload)
                    .map(|f| f.name == name)
                    .unwrap_or(false)
            })
            .count()
    }
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        contract: &bus::EventContract,
        payload: &[u8],
    ) -> Result<(), BusError> {
        self.rows
            .lock()
            .unwrap()
            .push((contract.topic.to_string(), payload.to_vec()));
        let fired = serde_json::from_slice::<schedulerevents::Fired>(payload).ok();
        if let (Some(fired), Some((name, stop))) =
            (fired, self.stop_after.lock().unwrap().as_ref())
        {
            if fired.name == *name {
                let _ = stop.send(true);
            }
        }
        Ok(())
    }

    fn subscribe_tx(
        &self,
        _spec: bus::SubscriptionSpec,
        _topic: &str,
        _version: u32,
        _history: Option<bus::HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
    }
}

/// A bus with a fake transport installed, plus a handle to that transport for assertions.
fn bus_with_fake() -> (Arc<Bus>, Arc<FakeTransport>) {
    let ft = FakeTransport::new();
    let bus = Bus::with_transport(ft.clone() as Arc<dyn Transport>);
    (Arc::new(bus), ft)
}

/// Polls `f` every 50ms until it is true or `max` elapses; returns the final verdict.
async fn wait_until(mut f: impl FnMut() -> bool, max: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < max {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    f()
}

// --- no DB ------------------------------------------------------------------

/// The `"scheduler"` readiness verdict transitions (remediation 4b): a never-started
/// loop is ready, a healthy stamp is ready, the `dead` flag flips it, and the pure
/// staleness predicate honors the never-seeded sentinel, the stall window, and a
/// controlled stop. Direct struct manipulation — no DB, no clock waits.
#[test]
fn liveness_check_transitions() {
    use std::sync::atomic::Ordering;

    let max = Duration::from_secs(30);

    // Never seeded (disabled, or before `start`): ready.
    let l = Liveness::default();
    assert!(l.check(max).is_ok(), "never-started loop must read ready");

    // Freshly stamped: ready.
    l.mark_tick_ok();
    assert!(l.check(max).is_ok(), "healthy stamp must read ready");

    // Loop task died: not ready, named reason.
    l.dead.store(true, Ordering::SeqCst);
    let err = l.check(max).expect_err("dead loop must read unready");
    assert!(err.contains("died"), "unexpected verdict: {err}");

    // The staleness predicate, deterministically (coarse-clock seconds):
    assert!(
        !stalled_from(0, 1_000, false, max),
        "never-seeded clock must never stall"
    );
    assert!(
        !stalled_from(990, 1_000, false, max),
        "10s-old stamp is within the 30s window"
    );
    assert!(
        stalled_from(900, 1_000, false, max),
        "100s-old stamp must read stalled"
    );
    assert!(
        !stalled_from(900, 1_000, true, max),
        "a controlled stop is not a stall"
    );
}

/// `lock_key` is stable per name (so two replicas derive the SAME advisory key and
/// contend) and the FNV-1a wrap matches Go's `int64(fnv64a(name))` for a known input.
#[test]
fn lock_key_is_stable_and_fnv1a() {
    assert_eq!(lock_key("audit-prune"), lock_key("audit-prune"));
    assert_ne!(lock_key("a"), lock_key("b"));
    // FNV-1a 64-bit of "audit-prune" as u64, reinterpreted as i64.
    let expected = {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in "audit-prune".bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h as i64
    };
    assert_eq!(lock_key("audit-prune"), expected);
}

/// Links the seed DDL's literal `'audit-prune'` tuple to the shared contract const —
/// a rename on either side (the seed literal or `schedulerevents::schedule_names::
/// AUDIT_PRUNE`) fails this build/test instead of drifting into a silent no-op prune.
#[test]
fn seeded_schedule_names_are_contract() {
    for name in [
        schedulerevents::schedule_names::AUDIT_PRUNE,
        schedulerevents::schedule_names::SESSIONS_PRUNE,
    ] {
        assert!(
            SCHEMA_DDL.contains(&format!("('{name}',")),
            "seed DDL no longer contains the schedule row for {name}"
        );
    }
}

/// [`SCHEMA_DDL`]'s CHECK, [`DUE_SQL`], and [`FIRE_RECHECK_SQL`] all bound
/// `interval_seconds` by the ONE authoritative [`INTERVAL_SECONDS_CEILING`] const
/// (belt to the DDL's `CHECK` braces — a row surviving on an un-wiped DB from before
/// the CHECK existed, or from before the CHECK's ceiling was added, must still never
/// fire, and must never poison the whole due-scan by making `make_interval` error the
/// entire SELECT). Pins the const's VALUE (the empirically-verified PG18 ceiling) and
/// its presence in all three strings — same style as `seeded_schedule_names_are_contract`.
#[test]
fn due_checks_filter_out_of_bounds_intervals() {
    assert_eq!(
        INTERVAL_SECONDS_CEILING, 9_223_372_036_854,
        "INTERVAL_SECONDS_CEILING drifted from the PG18-verified make_interval ceiling"
    );
    assert!(
        SCHEMA_DDL.contains(&format!(
            "interval_seconds <= {INTERVAL_SECONDS_CEILING}"
        )),
        "SCHEMA_DDL's CHECK no longer bounds interval_seconds to the make_interval ceiling"
    );
    let filter = format!("interval_seconds BETWEEN 1 AND {INTERVAL_SECONDS_CEILING}");
    assert!(
        DUE_SQL.contains(&filter),
        "DUE_SQL no longer bounds interval_seconds to the make_interval ceiling"
    );
    assert!(
        FIRE_RECHECK_SQL.contains(&filter),
        "FIRE_RECHECK_SQL no longer bounds interval_seconds to the make_interval ceiling"
    );
}

#[test]
fn due_rotation_resumes_after_cursor_or_its_insertion_point() {
    let names = vec!["a".to_string(), "c".to_string(), "e".to_string()];
    assert_eq!(rotation_start(&[], None), 0);
    assert_eq!(rotation_start(&names, None), 0);
    assert_eq!(rotation_start(&names, Some("a")), 1);
    assert_eq!(rotation_start(&names, Some("c")), 2);
    assert_eq!(rotation_start(&names, Some("e")), 0);
    assert_eq!(rotation_start(&names, Some("b")), 1);
    assert_eq!(rotation_start(&names, Some("d")), 2);
    assert_eq!(rotation_start(&names, Some("z")), 0);
}

// --- live Postgres ----------------------------------------------------------

/// A schedule with `interval_seconds = 0` (or negative) violates the table's `CHECK
/// (interval_seconds > 0)` — the fresh-DB constraint that backs [`DUE_SQL`]/
/// [`FIRE_RECHECK_SQL`]'s SQL-level filter. Requires the schema to have been created
/// WITH the CHECK in place (`CREATE TABLE IF NOT EXISTS` no-ops on an existing table,
/// so this test is only meaningful right after a schema wipe — stated in the plan's
/// verification step).
#[tokio::test(flavor = "multi_thread")]
async fn zero_interval_insert_violates_check() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;

    let err = sqlx::query(
        "INSERT INTO scheduler.schedules (name, interval_seconds) VALUES ($1, 0)",
    )
    .bind(&name)
    .execute(&pool)
    .await
    .expect_err("interval_seconds = 0 must violate the CHECK constraint");

    let db_err = err.as_database_error().expect("expected a database error");
    assert_eq!(
        db_err.code().as_deref(),
        Some("23514"), // check_violation
        "expected a check-violation SQLSTATE, got: {db_err}"
    );

    cleanup(&pool, &name).await;
}

/// One second above the DDL's `make_interval` ceiling — the empirically-verified point
/// where PG18's `make_interval(secs => …)` starts erroring ("interval out of range")
/// because `interval` stores microseconds in an `int64`. Must violate the named
/// `schedules_interval_bounds` CHECK the same way `interval_seconds = 0` does.
#[tokio::test(flavor = "multi_thread")]
async fn huge_interval_insert_violates_check() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;

    let err = sqlx::query(
        "INSERT INTO scheduler.schedules (name, interval_seconds) VALUES ($1, $2)",
    )
    .bind(&name)
    .bind(INTERVAL_SECONDS_CEILING + 1)
    .execute(&pool)
    .await
    .expect_err("interval_seconds one above the ceiling must violate the CHECK constraint");

    let db_err = err.as_database_error().expect("expected a database error");
    assert_eq!(
        db_err.code().as_deref(),
        Some("23514"), // check_violation
        "expected a check-violation SQLSTATE, got: {db_err}"
    );

    cleanup(&pool, &name).await;
}

/// THE failing-branch test: a legacy row above the `make_interval` ceiling (as could
/// exist on a DB that predates the `schedules_interval_bounds` CHECK) must not poison
/// the whole due-scan for every other schedule. Simulates that legacy state by
/// dropping the named CHECK, inserting the poison row alongside a healthy due
/// schedule, then re-adding the CHECK (mirroring "the DDL now has the guard, but this
/// row survived from before it did") and asserting [`due_schedules`] still succeeds
/// and reports the healthy schedule — proving [`DUE_SQL`]'s SQL-level filter belt, not
/// just the DDL CHECK, is what keeps the scan alive. On OLD code (filter without the
/// upper bound) this test is red: `make_interval` errors the whole SELECT.
#[tokio::test(flavor = "multi_thread")]
async fn due_scan_survives_legacy_huge_interval_row() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let healthy = unique_name(&pool).await;
    let poison = unique_name(&pool).await;

    // Panic residual: async drop guards can't run SQL, so if an assert between the
    // DROP below and the restore at the end panics, the shared dev DB is left with
    // the constraint dropped or NOT VALID (plus the temp rows). `test_pool`'s
    // `CREATE TABLE IF NOT EXISTS` will NOT repair it — a schema wipe (`DROP SCHEMA
    // scheduler CASCADE` + fresh boot) restores the fresh-DDL state.
    sqlx::query("ALTER TABLE scheduler.schedules DROP CONSTRAINT schedules_interval_bounds")
        .execute(&pool)
        .await
        .expect("drop CHECK to simulate a pre-CHECK legacy row");

    seed_schedule(&pool, &healthy, 60).await;
    sqlx::query(
        "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) \
         VALUES ($1, $2, to_timestamp(0)) \
         ON CONFLICT (name) DO UPDATE SET interval_seconds = $2, last_fired = to_timestamp(0)",
    )
    .bind(&poison)
    .bind(INTERVAL_SECONDS_CEILING + 1)
    .execute(&pool)
    .await
    .expect("insert legacy poison row");

    // NOT VALID so the poison row survives under the re-added CHECK — mirroring "the
    // DDL now has the guard, but this row landed before it did".
    sqlx::query(&format!(
        "ALTER TABLE scheduler.schedules ADD CONSTRAINT schedules_interval_bounds \
         CHECK (interval_seconds > 0 AND interval_seconds <= {INTERVAL_SECONDS_CEILING}) NOT VALID",
    ))
    .execute(&pool)
    .await
    .expect("re-add CHECK as NOT VALID (does not re-validate the legacy row)");

    let due = due_schedules(&pool, TICK_DEADLINE)
        .await
        .expect("due scan must survive a legacy out-of-bounds row, not error the whole SELECT");
    assert!(
        due.contains(&healthy),
        "due scan must still report the healthy due schedule: {due:?}"
    );
    assert!(
        !due.contains(&poison),
        "the legacy poison row must never be reported as due: {due:?}"
    );

    cleanup(&pool, &healthy).await;
    cleanup(&pool, &poison).await;
    // Restore the fresh-DDL state: with the poison row gone, validating flips the
    // constraint back to a plain VALID CHECK, so this test leaves no NOT VALID
    // residue on the shared DB (barring the panic residual documented above).
    sqlx::query("ALTER TABLE scheduler.schedules VALIDATE CONSTRAINT schedules_interval_bounds")
        .execute(&pool)
        .await
        .expect("re-validate the CHECK after removing the poison row");
}

/// Two concurrent `fire` attempts (two replicas: two pools, two buses) against one due
/// schedule must yield EXACTLY ONE durable emit and one `last_fired` bump — the advisory
/// lock + `still_due` re-check at work (Go's `TestFireExactlyOnceUnderConcurrency`).
#[tokio::test(flavor = "multi_thread")]
async fn fire_exactly_once_under_concurrency() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool1) = test_pool().await else {
        return;
    };
    let Ok(pool2) = PgPool::connect(&dsn()).await else {
        return;
    };

    let name = unique_name(&pool1).await;
    seed_schedule(&pool1, &name, 3600).await; // due (epoch), won't re-arm within the test

    let (bus1, ft1) = bus_with_fake();
    let (bus2, ft2) = bus_with_fake();

    let (p1, b1, n1) = (pool1.clone(), bus1.clone(), name.clone());
    let (p2, b2, n2) = (pool2.clone(), bus2.clone(), name.clone());
    let h1 = tokio::spawn(async move { fire(&p1, &b1, &n1, TICK_DEADLINE).await });
    let h2 = tokio::spawn(async move { fire(&p2, &b2, &n2, TICK_DEADLINE).await });
    h1.await.unwrap().expect("replica 1 fire");
    h2.await.unwrap().expect("replica 2 fire");

    assert_eq!(
        ft1.count(&name) + ft2.count(&name),
        1,
        "expected exactly 1 durable emit across two concurrent replicas"
    );

    // last_fired moved off the epoch exactly once (now not due).
    let due = due_schedules(&pool1, TICK_DEADLINE).await.unwrap();
    assert!(
        !due.contains(&name),
        "schedule {name:?} still due after firing"
    );

    cleanup(&pool1, &name).await;
}

/// A schedule re-arms: an immediate second fire is a no-op (not due), but after the
/// interval elapses it fires again (Go's `TestFiresAgainAfterInterval`).
#[tokio::test(flavor = "multi_thread")]
async fn fires_again_after_interval() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };

    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 1).await; // 1s interval

    let (bus, ft) = bus_with_fake();

    fire(&pool, &bus, &name, TICK_DEADLINE)
        .await
        .expect("first fire");
    assert_eq!(ft.count(&name), 1, "after first fire want 1 durable emit");

    // Immediately not due — second fire is a no-op.
    fire(&pool, &bus, &name, TICK_DEADLINE)
        .await
        .expect("second (immediate) fire");
    assert_eq!(
        ft.count(&name),
        1,
        "immediate refire should be a no-op durable-emit-wise"
    );

    // After the interval it is due again.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    fire(&pool, &bus, &name, TICK_DEADLINE)
        .await
        .expect("third fire");
    assert_eq!(ft.count(&name), 2, "after interval want 2 durable emits");

    cleanup(&pool, &name).await;
}

/// The 4b hang bound, end to end against the live DB: a competing row lock wedges
/// `fire`'s `UPDATE`, and the session `statement_timeout` must make the fire ERROR
/// (SQLSTATE 57014) instead of stalling forever — with the future never dropped on
/// this path, so the explicit advisory unlock still runs (lock immediately free to a
/// different session, no polling needed), and a subsequent fire from the same pool's
/// connect options works cleanly once unblocked. (`fire` no longer touches a pooled
/// session at all — each fire is a dedicated connection, closed on exit — so the old
/// "RESET before re-pooling" assertion is gone by construction.)
#[tokio::test(flavor = "multi_thread")]
async fn wedged_fire_errors_via_statement_timeout_and_leaks_nothing() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 3600).await; // due (epoch), won't re-arm mid-test

    // The wedge: a competing session holds the schedule row FOR UPDATE in an open tx,
    // so fire's `UPDATE ... SET last_fired` blocks until the statement_timeout cancels it.
    let mut blocker = PgConnection::connect(&dsn()).await.expect("connect blocker");
    let mut btx = blocker.begin().await.expect("open blocker tx");
    sqlx::query("SELECT 1 FROM scheduler.schedules WHERE name = $1 FOR UPDATE")
        .bind(&name)
        .fetch_one(&mut *btx)
        .await
        .expect("take competing row lock");

    let (bus, ft) = bus_with_fake();
    let started = std::time::Instant::now();
    let err = fire(&pool, &bus, &name, Duration::from_millis(500))
        .await
        .expect_err("wedged fire must ERROR via statement_timeout, not stall");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "fire took {:?} — the statement_timeout did not bound the wedge",
        started.elapsed()
    );
    let code = err
        .downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .and_then(|d| d.code());
    assert_eq!(
        code.as_deref(),
        Some("57014"), // query_canceled — "canceling statement due to statement timeout"
        "expected a statement_timeout cancellation, got: {err:#}"
    );
    assert_eq!(ft.count(&name), 0, "a timed-out fire must not emit");

    // The advisory lock was NOT leaked: the error path ran the explicit unlock before
    // `fire` returned, so a DIFFERENT session can take the key immediately.
    let key = lock_key(&name);
    let mut probe = PgConnection::connect(&dsn()).await.expect("connect probe");
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut probe)
        .await
        .expect("probe try-lock");
    assert!(
        free,
        "advisory lock for {name:?} leaked after the timed-out fire"
    );
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut probe)
        .await
        .expect("probe unlock");

    // Release the wedge: a fresh fire now succeeds — nothing about the error path
    // poisoned the schedule or the pool the connect options come from.
    btx.rollback().await.expect("release competing lock");
    fire(&pool, &bus, &name, TICK_DEADLINE)
        .await
        .expect("fire after unblock");
    assert_eq!(ft.count(&name), 1, "after unblock want exactly 1 durable emit");

    cleanup(&pool, &name).await;
}

/// The `"scheduler"` /readyz verdict under a wedge, driving the REAL [`run_loop`]:
/// while a competing lock wedges every tick, the liveness stamp ages past the stall
/// window and [`Liveness::check`] flips to Err — with the loop task still ALIVE (the
/// DB-layer bound errors the tick; nothing is dropped or killed). Releasing the lock
/// lets a healthy tick land, the stamp refreshes, and the verdict recovers to Ok.
#[tokio::test(flavor = "multi_thread")]
async fn wedged_tick_flips_scheduler_readyz_and_recovers() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 1).await; // stays due while the wedge holds

    let mut blocker = PgConnection::connect(&dsn()).await.expect("connect blocker");
    let mut btx = blocker.begin().await.expect("open blocker tx");
    sqlx::query("SELECT 1 FROM scheduler.schedules WHERE name = $1 FOR UPDATE")
        .bind(&name)
        .fetch_one(&mut *btx)
        .await
        .expect("take competing row lock");

    let (bus, ft) = bus_with_fake();
    let liveness = Liveness::default();
    let (stop_tx, stop_rx) = watch::channel(false);
    let cfg = LoopCfg {
        tick_interval: Duration::from_millis(100),
        tick_deadline: Duration::from_millis(300),
    };
    let task = tokio::spawn(run_loop(
        pool.clone(),
        bus.clone(),
        liveness.clone(),
        cfg,
        stop_rx,
    ));

    // Every tick errors (fire's UPDATE hits the 300ms statement_timeout), so the stamp
    // seeded at loop entry ages past the 1s stall window and the check flips.
    let stall_max = Duration::from_secs(1);
    let flipped = wait_until(|| liveness.check(stall_max).is_err(), Duration::from_secs(20)).await;
    assert!(
        flipped,
        "the scheduler readyz check never flipped while ticks were wedged"
    );
    let err = liveness.check(stall_max).unwrap_err();
    assert!(
        err.contains("no healthy scheduler tick"),
        "unexpected verdict: {err}"
    );
    // The loop is still ALIVE — bounded, not dead: the hang became an error, the
    // future was never dropped, and no panic killed the task.
    assert!(!task.is_finished(), "the emission loop died under the wedge");
    assert!(
        liveness.check(Duration::from_secs(3600)).is_ok(),
        "the dead flag flipped — the loop should only be stalled, not dead"
    );

    // Release the wedge: a healthy tick lands (the schedule finally fires), the stamp
    // refreshes, and the verdict recovers.
    btx.rollback().await.expect("release competing lock");
    let recovered = wait_until(|| liveness.check(stall_max).is_ok(), Duration::from_secs(20)).await;
    assert!(recovered, "readyz never recovered after the wedge lifted");
    assert!(
        ft.count(&name) >= 1,
        "the schedule never fired after the wedge lifted"
    );

    stop_tx.send(true).expect("signal stop");
    task.await.expect("loop task join");
    cleanup(&pool, &name).await;
}

/// Probes whether the advisory `key` is currently free from an INDEPENDENT session:
/// try-lock and (if taken) immediately release, so the probe itself never holds the
/// key across iterations.
async fn advisory_key_free(probe: &mut PgConnection, key: i64) -> bool {
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *probe)
        .await
        .expect("probe try-lock");
    if free {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(key)
            .execute(&mut *probe)
            .await
            .expect("probe unlock");
    }
    free
}

/// Aggregate tick budget: with the tick deadline already in the past, every due
/// schedule is SKIPPED — no emit, `last_fired` unmoved (still due next tick) — and the
/// tick reports `Err` so the [`Liveness`] stamp is withheld. Drives `tick` directly
/// with an exhausted deadline (the deadline is a parameter for exactly this test).
#[tokio::test(flavor = "multi_thread")]
async fn exhausted_tick_budget_skips_remaining_schedules() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 3600).await; // due (epoch)

    let (bus, ft) = bus_with_fake();
    let (_stop_tx, stop_rx) = watch::channel(false);
    let mut cursor = None;
    // Already-exhausted aggregate deadline; the due-scan itself keeps a healthy budget.
    let exhausted = Instant::now() - Duration::from_secs(1);
    let err = tick(
        &pool,
        &bus,
        TICK_DEADLINE,
        exhausted,
        &stop_rx,
        &mut cursor,
    )
        .await
        .expect_err("an exhausted tick budget must report the tick as errored");
    assert!(
        err.to_string().contains("fire(s) failed"),
        "unexpected tick error: {err:#}"
    );
    assert_eq!(ft.count(&name), 0, "a budget-skipped schedule must not emit");
    assert_eq!(cursor, None, "a budget-skipped schedule must not advance fairness");

    // `last_fired` never moved — the schedule is still due for the next tick.
    let due = due_schedules(&pool, TICK_DEADLINE).await.unwrap();
    assert!(
        due.contains(&name),
        "budget-skipped schedule {name:?} must remain due for the next tick"
    );

    cleanup(&pool, &name).await;
}

/// A budget-consuming first name cannot starve its lexical successor forever.
/// Tick one wedges on `00-slow` until its statement timeout consumes the budget;
/// tick two resumes after that actual-attempt cursor and emits `01-next` first.
#[tokio::test(flavor = "multi_thread")]
async fn successive_ticks_rotate_past_a_budget_consuming_schedule() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let unique = unique_name(&pool).await;
    let slow = format!("!fair-{unique}-00-slow");
    let next = format!("!fair-{unique}-01-next");
    seed_schedule(&pool, &slow, 3600).await;
    seed_schedule(&pool, &next, 3600).await;

    let mut blocker = PgConnection::connect(&dsn()).await.expect("connect blocker");
    let mut btx = blocker.begin().await.expect("open blocker tx");
    sqlx::query("SELECT 1 FROM scheduler.schedules WHERE name = $1 FOR UPDATE")
        .bind(&slow)
        .fetch_one(&mut *btx)
        .await
        .expect("wedge the slow schedule");

    let (bus, ft) = bus_with_fake();
    let (first_stop_tx, first_stop_rx) = watch::channel(false);
    ft.stop_after(&next, first_stop_tx);
    let mut cursor = None;
    // The due scan retains its normal DB bound; the actual fire budget is short.
    let first = tick(
        &pool,
        &bus,
        TICK_DEADLINE,
        Instant::now() + Duration::from_millis(150),
        &first_stop_rx,
        &mut cursor,
    )
    .await;
    let first_cursor = cursor.clone();
    let next_after_first = ft.count(&next);

    // Stop at the next schedule boundary once `next` commits, preventing this
    // focused test from firing unrelated due rows in the shared test database.
    let (second_stop_tx, second_stop_rx) = watch::channel(false);
    ft.stop_after(&next, second_stop_tx);
    let second = tick(
        &pool,
        &bus,
        TICK_DEADLINE,
        Instant::now() + Duration::from_millis(500),
        &second_stop_rx,
        &mut cursor,
    )
    .await;
    let next_after_second = ft.count(&next);

    btx.rollback().await.expect("release slow schedule wedge");
    cleanup(&pool, &slow).await;
    cleanup(&pool, &next).await;

    assert!(first.is_err(), "the wedged first tick must report failure");
    assert_eq!(first_cursor.as_deref(), Some(slow.as_str()));
    assert_eq!(next_after_first, 0, "tick one must leave the successor unattempted");
    assert!(second.is_ok(), "controlled stop after the successor is healthy: {second:?}");
    assert_eq!(next_after_second, 1, "tick two must emit the lexical successor once");
}

/// Shutdown under a wedged fire: the loop is stuck mid-fire (competing row lock, long
/// tick deadline), so the stop signal cannot be observed at a schedule boundary —
/// [`Scheduler::stop_tasks`] must return within [`STOP_GRACE`] (plus slack) by ABORTING
/// the loop, and the abort must NOT strand the schedule's advisory lock: the dedicated
/// per-fire connection dies with the dropped future, the session closes, and Postgres
/// releases the lock. The release is asynchronous (the server has to notice the
/// disconnect), so the freed-lock assertion POLLS instead of asserting immediately.
#[tokio::test(flavor = "current_thread")]
async fn stop_aborts_wedged_fire_within_grace_and_releases_the_lock() {
    let _tick_guard = TICK_TEST_LOCK.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 1).await; // stays due while the wedge holds

    // The wedge: a competing row lock blocks fire's UPDATE well past STOP_GRACE
    // (the 120s tick deadline guarantees the statement_timeout never fires first).
    let mut blocker = PgConnection::connect(&dsn()).await.expect("connect blocker");
    let mut btx = blocker.begin().await.expect("open blocker tx");
    sqlx::query("SELECT 1 FROM scheduler.schedules WHERE name = $1 FOR UPDATE")
        .bind(&name)
        .fetch_one(&mut *btx)
        .await
        .expect("take competing row lock");

    let (bus, ft) = bus_with_fake();
    let sched = Scheduler::new();
    let (stop_tx, stop_rx) = watch::channel(false);
    let cfg = LoopCfg {
        tick_interval: Duration::from_millis(50),
        tick_deadline: Duration::from_secs(120),
    };
    let task_dropped = Arc::new(AtomicBool::new(false));
    let task_drop_flag = DropFlag(task_dropped.clone());
    let loop_pool = pool.clone();
    let loop_bus = bus.clone();
    let loop_liveness = sched.liveness.clone();
    let task = tokio::spawn(async move {
        let _task_drop_flag = task_drop_flag;
        run_loop(loop_pool, loop_bus, loop_liveness, cfg, stop_rx).await;
    });
    *sched.stop_tx.lock().unwrap() = Some(stop_tx);
    sched.tasks.lock().unwrap().push(task);

    // Wait until the fire actually holds the advisory lock (i.e. it is wedged INSIDE
    // the guarded section) — only then is stop() genuinely racing a stuck fire.
    let key = lock_key(&name);
    let mut probe = PgConnection::connect(&dsn()).await.expect("connect probe");
    let wedged = {
        let started = std::time::Instant::now();
        loop {
            if !advisory_key_free(&mut probe, key).await {
                break true;
            }
            if started.elapsed() > Duration::from_secs(20) {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    assert!(wedged, "the loop never wedged inside a lock-holding fire");

    // stop() must resolve within the grace even though the fire cannot finish
    // (this is the exact hang the round-4 finding named — a bare JoinHandle.await
    // here used to outwait the app-level MODULE_STOP_GRACE_MS and get detached).
    sched.liveness.set_stopping();
    let started = std::time::Instant::now();
    sched.stop_tasks().await;
    let elapsed = started.elapsed();
    assert!(
        task_dropped.load(Ordering::SeqCst),
        "stop_tasks returned before the aborted task future was dropped and reaped"
    );
    assert!(
        elapsed < STOP_GRACE + Duration::from_secs(2),
        "stop_tasks took {elapsed:?} — the abort fallback did not bound shutdown"
    );

    // The abort dropped the fire's dedicated connection, closing its socket. A backend
    // BLOCKED inside a statement does not notice a client disconnect until the
    // statement resolves (in production the fire's own statement_timeout bounds that
    // window; here the competing row lock is the wedge) — so release the wedge first,
    // then POLL: the backend's UPDATE unblocks, it notices the dead client, terminates,
    // rolls back the in-flight tx (no last_fired bump, no emit — exactly-once holds),
    // and releases the session advisory lock.
    btx.rollback().await.expect("release competing lock");
    let freed = {
        let started = std::time::Instant::now();
        loop {
            if advisory_key_free(&mut probe, key).await {
                break true;
            }
            if started.elapsed() > Duration::from_secs(15) {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    assert!(
        freed,
        "advisory lock for {name:?} still held after the aborted fire's session should have closed"
    );
    // The aborted fire's tx died with its session: no durable emit ever landed.
    assert_eq!(ft.count(&name), 0, "an aborted fire must not emit");

    cleanup(&pool, &name).await;
}

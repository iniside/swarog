use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::PgPool;

use super::*;
use crate::app::MODULE_MIGRATE_LOCK_KEY;

/// Records every lifecycle callback into a shared log so a test can assert
/// phase ordering.
struct RecMod {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
    fail_start: bool,
    /// When set, `stop` sleeps 60s — a stand-in for a module that never returns,
    /// used to prove the per-module stop deadline abandons it (records nothing).
    hang_stop: bool,
}

impl RecMod {
    fn boxed(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
            fail_start: false,
            hang_stop: false,
        })
    }

    /// A module whose `start` fails (recording nothing for the failed phase).
    fn boxed_failing_start(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
            fail_start: true,
            hang_stop: false,
        })
    }

    /// A module whose `stop` never returns within the deadline (sleeps 60s).
    fn boxed_hanging_stop(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
            fail_start: false,
            hang_stop: true,
        })
    }

    fn record(&self, phase: &str) {
        self.log.lock().unwrap().push(format!("{phase}:{}", self.name));
    }
}

#[async_trait::async_trait]
impl Module for RecMod {
    fn name(&self) -> &str {
        &self.name
    }

    fn register(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("register");
        Ok(())
    }

    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("init");
        Ok(())
    }

    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.record("migrate");
        Ok(())
    }

    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        if self.fail_start {
            anyhow::bail!("start blew up");
        }
        self.record("start");
        Ok(())
    }

    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if self.hang_stop {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        self.record("stop");
        Ok(())
    }
}

fn app_with(log: &Arc<Mutex<Vec<String>>>, names: &[&str]) -> App {
    let mut app = App::new(Arc::new(Context::new()));
    for n in names {
        app.add(RecMod::boxed(n, log));
    }
    app
}

/// The core guarantee: ALL registers run before ANY init (phase 1 → phase 2),
/// each phase in registration order. That's what lets a module require any
/// service in init without a topological sort.
#[tokio::test]
async fn two_phase_build() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let app = app_with(&log, &["a", "b"]);
    app.build().unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["register:a", "register:b", "init:a", "init:b"]
    );
}

/// Migrate and start run in registration order after build; stop runs in
/// REVERSE registration order.
#[tokio::test]
async fn full_lifecycle_ordering() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let app = app_with(&log, &["a", "b"]);
    app.build().unwrap();
    app.migrate().await.unwrap();
    app.start().await.unwrap();
    app.stop().await;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "register:a",
            "register:b",
            "init:a",
            "init:b",
            "migrate:a",
            "migrate:b",
            "start:a",
            "start:b",
            "stop:b",
            "stop:a",
        ]
    );
}

/// The partial-start unwind: when module B's `start` fails, A (started before B)
/// is stopped, while B itself and C (whose `start` never ran) are NOT — `stop`
/// is only ever invoked after a successful `start`. The original error survives.
#[tokio::test]
async fn start_failure_stops_started_prefix_in_reverse() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Arc::new(Context::new()));
    app.add(RecMod::boxed("a", &log));
    app.add(RecMod::boxed_failing_start("b", &log));
    app.add(RecMod::boxed("c", &log));
    app.build().unwrap();
    log.lock().unwrap().clear();

    let err = app.start().await.unwrap_err();
    assert!(err.to_string().contains("start \"b\""), "{err:#}");
    assert!(format!("{err:#}").contains("start blew up"), "{err:#}");
    // A started, then B failed → only A gets stop; B and C never do.
    assert_eq!(*log.lock().unwrap(), vec!["start:a", "stop:a"]);
}

/// A module hung in `stop` must not stall ordered teardown: with a short
/// `with_stop_grace`, [`App::stop`] abandons the hung module after the deadline
/// (recording nothing for it) and still stops the rest in REVERSE registration
/// order. The whole call returns in well under the hung module's 60s sleep.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_abandons_a_hung_module_and_continues_teardown() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Arc::new(Context::new())).with_stop_grace(Duration::from_millis(100));
    app.add(RecMod::boxed("a", &log));
    app.add(RecMod::boxed_hanging_stop("b", &log));
    app.add(RecMod::boxed("c", &log));

    let started = Instant::now();
    app.stop().await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(30),
        "stop() waited on the hung module ({elapsed:?}) — the deadline did not fire"
    );
    // Reverse order: c stops, b is abandoned (no record), a stops.
    assert_eq!(*log.lock().unwrap(), vec!["stop:c", "stop:a"]);
}

/// The start-unwind path is bounded the same way: when a module's `start` fails,
/// the started prefix is stopped in reverse; a hung `stop` mid-prefix is abandoned
/// after the deadline and the earlier module still gets stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_unwind_abandons_a_hung_stop_and_continues() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Arc::new(Context::new())).with_stop_grace(Duration::from_millis(100));
    app.add(RecMod::boxed("a", &log));
    app.add(RecMod::boxed_hanging_stop("b", &log));
    app.add(RecMod::boxed("c", &log));
    app.add(RecMod::boxed_failing_start("d", &log));
    app.build().unwrap();
    log.lock().unwrap().clear();

    let started = Instant::now();
    let err = app.start().await.unwrap_err();
    let elapsed = started.elapsed();

    assert!(err.to_string().contains("start \"d\""), "{err:#}");
    assert!(
        elapsed < Duration::from_secs(30),
        "start unwind waited on the hung module ({elapsed:?}) — the deadline did not fire"
    );
    // a,b,c started; d failed. Unwind stops the prefix in reverse: c, then b is
    // abandoned (no record), then a — d never started, so it is never stopped.
    assert_eq!(
        *log.lock().unwrap(),
        vec!["start:a", "start:b", "start:c", "stop:c", "stop:a"]
    );
}

/// Fallback DSN when `DATABASE_URL` is unset — the same default `core/app` uses.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres; returns `None` (printing a skip line) when
/// unreachable, so the suite degrades to a no-op where there's no DB — the same
/// convention as `asyncevents`' live tests.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => Some(p),
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — lifecycle DB tests skipped");
            None
        }
    }
}

/// Serializes the tests that take the GLOBAL `MODULE_MIGRATE_LOCK_KEY` advisory
/// lock on the shared DB — the `763f1d9` choreography lesson (asyncevents' two
/// writer-lock tests deadlocked when interleaved: a Rust-await <-> DB-lock cycle
/// Postgres cannot detect). `concurrent_migrate_runs_are_serialized_by_advisory_lock`
/// and `migrate_times_out_when_lock_is_held` both contend for the same session
/// lock, so both take this guard first. An async (tokio) mutex, same remedy as
/// `763f1d9`'s `WRITER_LOCK_CHOREOGRAPHY`: the guard is held across awaits (a
/// std guard trips `clippy::await_holding_lock`), and it cannot poison — a
/// prior panicking holder never wedges later tests.
static LOCK_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A probe module whose `migrate` records the `(enter, exit)` window it was active
/// in — a ~300ms sleep guarantees measurable overlap IF the advisory lock failed
/// to serialize two concurrent `App::migrate` runs.
struct MigrateProbe {
    windows: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

#[async_trait::async_trait]
impl Module for MigrateProbe {
    fn name(&self) -> &str {
        "migrate-probe"
    }
    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }
    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        let enter = Instant::now();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let exit = Instant::now();
        self.windows.lock().unwrap().push((enter, exit));
        Ok(())
    }
}

/// Two concurrent `App::migrate` runs against one shared pool must NOT overlap:
/// [`App::migrate`] holds a session advisory lock around the whole module loop, so
/// the second run blocks on the lock until the first releases it. Asserts the two
/// recorded migrate windows are disjoint (later run enters no earlier than the
/// first run exits). Skips when Postgres is unreachable, like the other live tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_migrate_runs_are_serialized_by_advisory_lock() {
    // See LOCK_TESTS: serializes against migrate_times_out_when_lock_is_held —
    // both hold the GLOBAL module-migrate advisory lock (the `763f1d9` lesson).
    let _choreo = LOCK_TESTS.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let windows = Arc::new(Mutex::new(Vec::new()));

    let build = || {
        let mut app = App::new(Arc::new(Context::with_db(pool.clone())));
        app.add(Box::new(MigrateProbe {
            windows: windows.clone(),
        }));
        app
    };
    let app_a = build();
    let app_b = build();

    let (ra, rb) = tokio::join!(app_a.migrate(), app_b.migrate());
    ra.unwrap();
    rb.unwrap();

    let mut w = windows.lock().unwrap().clone();
    assert_eq!(w.len(), 2, "both migrate runs should have recorded a window");
    w.sort_by_key(|(enter, _)| *enter);
    let (_, first_exit) = w[0];
    let (second_enter, _) = w[1];
    assert!(
        second_enter >= first_exit,
        "migrate windows overlapped — advisory lock did not serialize them \
         (first_exit={first_exit:?}, second_enter={second_enter:?})"
    );
}

/// Holding `MODULE_MIGRATE_LOCK_KEY` on a raw connection must make
/// `migrate_with_lock_timeout` fail loudly, not hang, once its `lock_timeout`
/// expires — and migrate must succeed normally once the holder releases.
/// Uses a test-lowered `"200ms"` timeout instead of the real 60s so the test
/// stays fast. Skips when Postgres is unreachable, like the other live tests.
#[tokio::test]
async fn migrate_times_out_when_lock_is_held() {
    // See LOCK_TESTS: serializes against
    // concurrent_migrate_runs_are_serialized_by_advisory_lock — both hold the
    // GLOBAL module-migrate advisory lock (the `763f1d9` lesson).
    let _choreo = LOCK_TESTS.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };

    let mut holder = pool
        .acquire()
        .await
        .expect("acquire raw connection to hold the lock");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MODULE_MIGRATE_LOCK_KEY)
        .execute(&mut *holder)
        .await
        .expect("hold module-migrate advisory lock");

    let app = App::new(Arc::new(Context::with_db(pool.clone())));
    let err = app
        .migrate_with_lock_timeout("200ms")
        .await
        .expect_err("migrate must fail while the lock is held");
    assert!(
        format!("{err:#}").contains("not acquired"),
        "expected the lock-timeout context, got: {err:#}"
    );

    // Release the holder, then migrate must succeed normally.
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MODULE_MIGRATE_LOCK_KEY)
        .execute(&mut *holder)
        .await
        .expect("release module-migrate advisory lock");
    drop(holder);

    app.migrate_with_lock_timeout("200ms")
        .await
        .expect("migrate must succeed once the lock is free");
}

/// A module whose `migrate` PANICS — the one live panic/cancel point in the boot
/// path between the migrate lock and its unlock. Used to prove the lock is still
/// released when a module blows up mid-migrate.
struct PanicMigrate;

#[async_trait::async_trait]
impl Module for PanicMigrate {
    fn name(&self) -> &str {
        "panic-migrate"
    }
    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }
    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        panic!("module migrate blew up while holding the migrate lock");
    }
}

/// Polls, from throwaway pool connections, until `MODULE_MIGRATE_LOCK_KEY` is
/// free (`pg_try_advisory_lock` succeeds, then immediately releases). FLAKE PIN:
/// the panicking run releases the lock by socket-close→backend-EOF, which is fast
/// but NOT instantaneous — asserting right after the panic would measure "released
/// within N ms of socket close", not "released". Polling for actual release (≈6s
/// cap) removes that latency race.
async fn wait_for_migrate_lock_release(pool: &PgPool) {
    for _ in 0..300 {
        let mut c = pool.acquire().await.expect("probe conn");
        let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MODULE_MIGRATE_LOCK_KEY)
            .fetch_one(&mut *c)
            .await
            .expect("try advisory lock");
        if free {
            sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(MODULE_MIGRATE_LOCK_KEY)
                .execute(&mut *c)
                .await
                .expect("release probe lock");
            return;
        }
        drop(c);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("module-migrate advisory lock was not released within ~6s of the panic");
}

/// The release-on-panic guarantee: when a module's `migrate` PANICS while the
/// session-scoped `MODULE_MIGRATE_LOCK_KEY` is held, the DETACHED lock connection
/// is dropped on unwind — closing its socket, ending the Postgres backend session,
/// and auto-releasing the advisory lock. A SECOND migrate on the SAME pool must
/// then ACQUIRE the lock, proving the panicking run released it. Without the
/// detach+drop guard the panicking run returns its POOLED connection with the lock
/// still held → the second migrate would block its whole `lock_timeout` and fail
/// 55P03 (permanent deadlock in production, where every process shares the DB).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrate_lock_released_when_module_migrate_panics() {
    // See LOCK_TESTS: this holds the GLOBAL module-migrate advisory lock, so it
    // serializes against the other lock tests (the `763f1d9` lesson).
    let _choreo = LOCK_TESTS.lock().await;
    let Some(pool) = test_pool().await else {
        return;
    };

    // First migrate panics mid-loop while holding the lock. Run it on a spawned
    // task so the panic is caught (JoinHandle → Err) and the test process
    // survives; the panic unwinds through `migrate_with_lock_timeout`, dropping
    // the detached lock connection.
    let first = {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut app = App::new(Arc::new(Context::with_db(pool)));
            app.add(Box::new(PanicMigrate));
            app.migrate().await
        })
        .await
    };
    assert!(
        first.is_err() && first.as_ref().unwrap_err().is_panic(),
        "the module-migrate panic should have unwound the first migrate as a task panic, got {first:?}"
    );

    // FLAKE PIN: wait for the lock to actually clear (socket-close latency), then
    // assert. Belt: the asserting migrate also uses a generous 5s lock_timeout —
    // with the fix the lock is already free; without it, it would block the full
    // 5s and fail 55P03.
    wait_for_migrate_lock_release(&pool).await;

    let mut app = App::new(Arc::new(Context::with_db(pool.clone())));
    app.add(RecMod::boxed("after-panic", &Arc::new(Mutex::new(Vec::new()))));
    app.migrate_with_lock_timeout("5s")
        .await
        .expect("second migrate must ACQUIRE the lock the panicking run released");
}

#[tokio::test]
#[should_panic(expected = "registered twice")]
async fn duplicate_name_panics() {
    let log = Arc::new(Mutex::new(Vec::new()));
    app_with(&log, &["dup", "dup"]);
}

/// A module implementing only `name`/`init` (all other phases left at their
/// default no-op impls) still gets every phase called unconditionally — the
/// full build/migrate/start/stop cycle must succeed on a DB-less `Context`
/// without error, proving the defaults are true no-ops.
#[tokio::test]
async fn default_impl_module_survives_full_cycle() {
    struct Plain {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Module for Plain {
        fn name(&self) -> &str {
            "plain"
        }
        fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
            self.log.lock().unwrap().push("init:plain".into());
            Ok(())
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Arc::new(Context::new()));
    app.add(Box::new(Plain { log: log.clone() }));
    app.build().unwrap();
    app.migrate().await.unwrap();
    app.start().await.unwrap();
    app.stop().await;
    assert_eq!(*log.lock().unwrap(), vec!["init:plain"]);
}

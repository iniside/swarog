use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::PgPool;

use super::*;

/// Records every lifecycle callback into a shared log so a test can assert
/// phase ordering.
struct RecMod {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
    fail_start: bool,
}

impl RecMod {
    fn boxed(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
            fail_start: false,
        })
    }

    /// A module whose `start` fails (recording nothing for the failed phase).
    fn boxed_failing_start(name: &str, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Module> {
        Box::new(RecMod {
            name: name.to_string(),
            log: log.clone(),
            fail_start: true,
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

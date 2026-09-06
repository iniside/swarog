use super::*;

/// `DATABASE_URL`/`TESTDB_ALLOW_SKIP` are process-global, so the tests that mutate them
/// run one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A DSN that cannot connect: port 1 refuses immediately, so the strict branch is reached
/// without waiting out `CONNECT_BOUND`.
const DEAD_DSN: &str = "postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable";

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn block_on_test_pool() -> std::thread::Result<Option<PgPool>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(test_pool())
    }));
    std::panic::set_hook(hook);
    outcome
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_default()
}

#[test]
fn an_absent_cluster_fails_the_run_unless_the_opt_out_is_on() {
    assert_eq!(decide(true, false), Verdict::Use);
    assert_eq!(decide(true, true), Verdict::Use);
    assert_eq!(decide(false, true), Verdict::Skip);
    assert_eq!(
        decide(false, false),
        Verdict::Fail,
        "an unreachable cluster with no opt-out must never yield a skip — that is the \
         green-signal-that-never-ran defect"
    );
}

#[test]
fn the_opt_out_is_off_unless_explicitly_truthy() {
    let _serialized = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    {
        let _guard = EnvGuard::unset(SKIP_ENV);
        assert!(!skip_allowed(), "unset must mean strict");
    }
    for off in ["", "0", "false", "off", "no", "yes", "maybe"] {
        let _guard = EnvGuard::set(SKIP_ENV, off);
        assert!(!skip_allowed(), "{off:?} must not enable skipping");
    }
    for on in ["1", "true", "TRUE", "On"] {
        let _guard = EnvGuard::set(SKIP_ENV, on);
        assert!(skip_allowed(), "{on:?} must enable skipping");
    }
}

/// The previously-wrong branch, end to end: an unreachable DSN used to return `None` and
/// let every DB test early-return green. It must now panic, naming the cause.
#[test]
fn an_unreachable_dsn_panics_instead_of_returning_none() {
    let _serialized = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _dsn = EnvGuard::set("DATABASE_URL", DEAD_DSN);
    let _skip = EnvGuard::unset(SKIP_ENV);

    let outcome = block_on_test_pool();
    let message = match outcome {
        Ok(pool) => panic!("an unreachable DSN returned {pool:?} instead of failing the run"),
        Err(payload) => panic_message(payload),
    };
    assert!(message.contains(DEAD_DSN), "{message}");
    assert!(message.contains(SKIP_ENV), "{message}");
}

/// The opt-out's own branch: explicitly on, the same unreachable DSN skips instead.
#[test]
fn the_opt_out_turns_the_same_failure_into_a_skip() {
    let _serialized = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _dsn = EnvGuard::set("DATABASE_URL", DEAD_DSN);
    let _skip = EnvGuard::set(SKIP_ENV, "1");

    match block_on_test_pool() {
        Ok(pool) => assert!(pool.is_none()),
        Err(payload) => panic!("the opt-out did not skip: {}", panic_message(payload)),
    }
}

/// The skip decision must never ride the VIRTUAL clock. A `start_paused` runtime
/// auto-advances every `tokio::time` bound the moment it idles, which would report a
/// HEALTHY cluster as absent — and, now that absence is fatal, would turn a working
/// cluster into a red suite for every crate arming `test-util`. Reads no env: a sibling
/// test mutating `DATABASE_URL` must not be able to decide this one.
#[tokio::test(start_paused = true)]
async fn the_connect_bound_does_not_ride_the_virtual_clock() {
    let started = std::time::Instant::now();
    let pool = connect(DEFAULT_DSN).await;
    assert!(
        pool.is_some() || started.elapsed() >= CONNECT_BOUND,
        "a paused test decided postgres was unreachable in {:?} — the bound is virtual",
        started.elapsed()
    );
}

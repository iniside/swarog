//! Live-Postgres integration + unit tests for the broadcast invalidation plane. The
//! DB-touching tests SKIP cleanly (early return + message) when Postgres is unreachable.
//! In-crate (not `tests/`) so they can drive the private [`RunCtx`]/[`Registration`]
//! fan-out primitives directly.

use super::*;

fn register_noop(invalidation: &Invalidation, channel: &str, name: &str) {
    invalidation.register(channel, name, || async { Ok(()) });
}

#[test]
fn register_rejects_empty_channel() {
    let invalidation = Invalidation::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        register_noop(&invalidation, "", "callback");
    }));
    assert!(result.is_err());
    assert!(invalidation.snapshot().is_empty());
}

#[test]
fn register_rejects_empty_name() {
    let invalidation = Invalidation::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        register_noop(&invalidation, "channel", "");
    }));
    assert!(result.is_err());
    assert!(invalidation.snapshot().is_empty());
}

#[test]
fn register_rejects_duplicate_name_on_same_channel() {
    let invalidation = Invalidation::new();
    register_noop(&invalidation, "channel", "callback");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        register_noop(&invalidation, "channel", "callback");
    }));
    assert!(result.is_err());
    let registrations = invalidation.snapshot();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].channel, "channel");
    assert_eq!(registrations[0].name, "callback");
}

#[test]
fn register_rejects_duplicate_name_across_channels() {
    let invalidation = Invalidation::new();
    register_noop(&invalidation, "first", "callback");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        register_noop(&invalidation, "second", "callback");
    }));
    assert!(result.is_err());
    let registrations = invalidation.snapshot();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].channel, "first");
}

#[test]
fn duplicate_panic_does_not_poison_registry_or_replace_first() {
    let invalidation = Invalidation::new();
    register_noop(&invalidation, "first", "kept");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        register_noop(&invalidation, "other", "kept");
    }));
    assert!(result.is_err());

    register_noop(&invalidation, "other", "unique");
    let registrations = invalidation.snapshot();
    assert_eq!(registrations.len(), 2);
    assert_eq!(registrations[0].channel, "first");
    assert_eq!(registrations[0].name, "kept");
    assert_eq!(registrations[1].channel, "other");
    assert_eq!(registrations[1].name, "unique");
}
use std::sync::atomic::{AtomicUsize, Ordering};

use sqlx::PgPool;

/// Fallback DSN when `DATABASE_URL` is unset — the same default the rest of the workspace uses.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres; returns `None` (printing a skip line) when unreachable.
async fn test_pool() -> Option<(PgPool, String)> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => Some((p, dsn)),
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — invalidation DB tests skipped");
            None
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// A NOTIFY channel unique per run so concurrent runs never cross-trigger.
fn unique_channel() -> String {
    format!("inval_ch_{}_{}", std::process::id(), nanos())
}

/// A callback that bumps a shared counter on every refresh.
fn counting(hits: &Arc<AtomicUsize>) -> impl Fn() -> RefreshFuture + Send + Sync + 'static {
    let hits = hits.clone();
    move || {
        let hits = hits.clone();
        Box::pin(async move {
            hits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

/// A committed NOTIFY on a registered channel re-runs the channel's callback.
#[tokio::test]
async fn notify_triggers_callback() {
    let Some((pool, dsn)) = test_pool().await else {
        return;
    };
    let chan = unique_channel();
    let hits = Arc::new(AtomicUsize::new(0));

    // Long poll so only the NOTIFY path can bump the counter during the test.
    let mut plane =
        new_plane(dsn).unwrap().with_poll_interval(Duration::from_secs(3600));
    plane.handle().register(&chan, "cb", counting(&hits));
    plane.start().await.unwrap();

    // Let the listener connect + run its connect refresh, then baseline.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let baseline = hits.load(Ordering::SeqCst);

    // The listener connects asynchronously; re-send NOTIFY until one is delivered.
    let mut delivered = false;
    for _ in 0..50 {
        sqlx::query("SELECT pg_notify($1, '')")
            .bind(&chan)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if hits.load(Ordering::SeqCst) > baseline {
            delivered = true;
            break;
        }
    }
    plane.stop().await;
    assert!(delivered, "NOTIFY did not trigger the callback");
}

/// A reconnect performs a full refresh of every callback: killing the listener's backend
/// makes `recv` error → reconnect → `refresh_all`, re-running the callback with NO NOTIFY.
#[tokio::test]
async fn reconnect_performs_full_refresh() {
    let Some((pool, dsn)) = test_pool().await else {
        return;
    };
    // A recognizable application_name so we can find and terminate exactly the listener's
    // backend; if sqlx doesn't propagate it (0 backends found) the test SKIPs honestly.
    let app_name = format!("inval_test_{}_{}", std::process::id(), nanos());
    let sep = if dsn.contains('?') { '&' } else { '?' };
    let listen_dsn = format!("{dsn}{sep}application_name={app_name}");
    let chan = unique_channel();
    let hits = Arc::new(AtomicUsize::new(0));

    let mut plane = new_plane(listen_dsn)
        .unwrap()
        .with_poll_interval(Duration::from_secs(3600));
    plane.handle().register(&chan, "cb", counting(&hits));
    plane.start().await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await; // connected + connect refresh done
    let baseline = hits.load(Ordering::SeqCst);

    let killed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (\
             SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1\
         ) t",
    )
    .bind(&app_name)
    .fetch_one(&pool)
    .await
    .unwrap();
    if killed == 0 {
        plane.stop().await;
        eprintln!("SKIP: application_name not propagated to the listener conn — reconnect test skipped");
        return;
    }

    // Reconnect goes through a ~1s backoff, then LISTEN + refresh_all.
    let mut healed = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if hits.load(Ordering::SeqCst) > baseline {
            healed = true;
            break;
        }
    }
    plane.stop().await;
    assert!(healed, "reconnect did not perform a full refresh");
}

/// The poll fallback re-runs callbacks on its interval with NO NOTIFY delivered — the
/// lost-NOTIFY floor (a change whose NOTIFY was dropped while the listener was down is
/// still caught).
#[tokio::test]
async fn poll_fallback_refreshes_without_notify() {
    let Some((_pool, dsn)) = test_pool().await else {
        return;
    };
    let chan = unique_channel();
    let hits = Arc::new(AtomicUsize::new(0));

    let mut plane =
        new_plane(dsn).unwrap().with_poll_interval(Duration::from_millis(300));
    plane.handle().register(&chan, "cb", counting(&hits));
    plane.start().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await; // listener settle
    let baseline = hits.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1000)).await; // ≥3 poll ticks, no NOTIFY sent
    let after = hits.load(Ordering::SeqCst);
    plane.stop().await;

    assert!(
        after > baseline,
        "poll fallback did not re-run the callback (baseline={baseline}, after={after})"
    );
}

/// A failing callback must not prevent a sibling on the same channel from running. Drives
/// the fan-out primitive directly (no DB, no timing).
#[tokio::test]
async fn failing_callback_does_not_block_sibling() {
    let good = Arc::new(AtomicUsize::new(0));
    let bad = Arc::new(AtomicUsize::new(0));

    let inv = Invalidation::new();
    {
        let bad = bad.clone();
        inv.register("c", "bad", move || {
            let bad = bad.clone();
            async move {
                bad.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("boom")
            }
        });
    }
    inv.register("c", "good", counting(&good));

    let regs = inv.snapshot();
    let mut by_channel: HashMap<String, Vec<Registration>> = HashMap::new();
    for reg in &regs {
        by_channel.entry(reg.channel.clone()).or_default().push(reg.clone());
    }
    let ctx = RunCtx {
        all: regs,
        by_channel,
        health: Health::default(),
        gauges: gauges::Gauges::new(),
        callback_timeout: Duration::from_secs(10),
    };
    ctx.run_channel("c").await;

    assert_eq!(bad.load(Ordering::SeqCst), 1, "failing callback did not run");
    assert_eq!(
        good.load(Ordering::SeqCst),
        1,
        "sibling did not run after a failing callback"
    );
}

/// A hung callback (a never-resolving future) must not block a sibling on the same
/// channel: the deadline fires, its run is counted as a failure, and the sibling still
/// runs. Drives the fan-out primitive directly with a short timeout (no DB, no NOTIFY).
#[tokio::test]
async fn hung_callback_does_not_block_sibling() {
    let good = Arc::new(AtomicUsize::new(0));

    let inv = Invalidation::new();
    // A callback whose future never resolves — the deadline is the only thing that can
    // end it, so if the timeout is dropped this fan-out hangs forever.
    inv.register("c", "hung", || async {
        std::future::pending::<()>().await;
        Ok(())
    });
    inv.register("c", "good", counting(&good));

    let regs = inv.snapshot();
    let mut by_channel: HashMap<String, Vec<Registration>> = HashMap::new();
    for reg in &regs {
        by_channel.entry(reg.channel.clone()).or_default().push(reg.clone());
    }
    let ctx = RunCtx {
        all: regs,
        by_channel,
        health: Health::default(),
        gauges: gauges::Gauges::new(),
        callback_timeout: Duration::from_millis(100),
    };

    // If the deadline were absent this would hang forever; a generous overall bound keeps
    // a regression from wedging the suite.
    tokio::time::timeout(Duration::from_secs(5), ctx.run_channel("c"))
        .await
        .expect("hung callback wedged the fan-out despite the deadline");

    assert_eq!(
        good.load(Ordering::SeqCst),
        1,
        "sibling did not run after a hung callback timed out"
    );
    // Only the successful sibling marked its health clock; the timed-out callback never
    // recorded a success (it took the failure path), so the fresh set is exactly "good".
    assert!(
        ctx.health.stale(Duration::from_secs(3600)).is_empty(),
        "no callback should be stale immediately after a fresh success"
    );
}

/// A first-refresh that hangs past the deadline fails `start` loudly — the boot contract
/// (no cache stale-ready) holds even when a callback wedges rather than errors. No DB: the
/// boot refresh runs before any connect, so the DSN ("postgres://unused") is never touched.
#[tokio::test]
async fn first_refresh_timeout_fails_start() {
    let mut plane = new_plane("postgres://unused".to_string())
        .unwrap()
        .with_callback_timeout(Duration::from_millis(100));
    plane.handle().register("c", "hung", || async {
        std::future::pending::<()>().await; // never resolves
        Ok(())
    });

    let res = plane.start().await;
    let err = res.expect_err("start must fail loudly when a first refresh times out");
    assert!(
        format!("{err:#}").contains("timed out"),
        "error should mention the timeout, got: {err:#}"
    );

    plane.stop().await; // no-op: never started
}

/// `stop` returns within its grace bound even when a background task is wedged in a hung
/// refresh mid-flight. Uses a live DB channel (so `start` gets past the first refresh) with
/// a callback that succeeds the FIRST time (the boot refresh) then hangs on every later
/// call; a NOTIFY drives a fan-out into the hang, and `stop` must still complete — the
/// task exceeding its grace is aborted, not awaited forever.
#[tokio::test]
async fn stop_returns_despite_hung_callback() {
    let Some((pool, dsn)) = test_pool().await else {
        return;
    };
    let chan = unique_channel();
    let calls = Arc::new(AtomicUsize::new(0));

    let mut plane = new_plane(dsn)
        .unwrap()
        .with_poll_interval(Duration::from_secs(3600)) // only NOTIFY drives a refresh
        .with_callback_timeout(Duration::from_secs(3600)); // long: the hang must be live at stop
    {
        let calls = calls.clone();
        plane.handle().register(&chan, "cb", move || {
            let calls = calls.clone();
            async move {
                // First call (the boot refresh) succeeds; every later call hangs forever.
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(());
                }
                std::future::pending::<()>().await;
                Ok(())
            }
        });
    }
    plane.start().await.unwrap(); // boot refresh = call #0, succeeds

    tokio::time::sleep(Duration::from_millis(500)).await; // listener connect + connect refresh

    // Drive a NOTIFY fan-out into the hang; re-send until the callback is entered again.
    for _ in 0..50 {
        sqlx::query("SELECT pg_notify($1, '')")
            .bind(&chan)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if calls.load(Ordering::SeqCst) > 1 {
            break;
        }
    }
    assert!(
        calls.load(Ordering::SeqCst) > 1,
        "callback never re-entered — the hang was never triggered"
    );

    // The listener task is now wedged in the hung refresh; stop must still return, well
    // inside 5s grace + a task or two + margin.
    tokio::time::timeout(Duration::from_secs(20), plane.stop())
        .await
        .expect("stop() did not return despite a hung callback — teardown is unbounded");
}

/// A first-refresh failure surfaces loudly as an error from `start` (no DB needed — the
/// boot refresh runs before any connect, so the DSN is never touched).
#[tokio::test]
async fn first_refresh_failure_fails_start() {
    let mut plane = new_plane("postgres://unused".to_string()).unwrap();
    plane
        .handle()
        .register("c", "boom", || async { anyhow::bail!("nope") });

    let res = plane.start().await;
    assert!(res.is_err(), "start must fail loudly when a first refresh fails");

    plane.stop().await; // no-op: never started
}

// ============================================================================
// Step 13 / DEFECT 2 (+ C20): `InvalidationPlane::new` env resolution is the fail-loud
// decision authority for `INVALIDATION_POLL_INTERVAL_MS`/`INVALIDATION_CALLBACK_TIMEOUT_MS`,
// mirroring `asyncevents::retention::Config`'s posture exactly: absent → default,
// malformed → default (garbage tolerated), EXPLICIT zero → fail construction (never a
// silent default). These are boot-level tests: construction does no I/O, so no DB is
// needed to prove a bad knob aborts startup before a connection is ever attempted.
// ============================================================================

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Constructs a plane while holding [`ENV_LOCK`] — the single serialization point
/// against the env-mutating tests below. `INVALIDATION_POLL_INTERVAL_MS`/
/// `INVALIDATION_CALLBACK_TIMEOUT_MS` are read ONLY inside `InvalidationPlane::new`,
/// so routing every construction in this file through this helper is sufficient to
/// stop a parallel test thread from ever observing another test's temporarily
/// mutated env value.
fn new_plane(dsn: impl Into<String>) -> anyhow::Result<InvalidationPlane> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    InvalidationPlane::new(dsn.into())
}

/// Runs `f` with `key` env-set (or cleared) for its duration, holding [`ENV_LOCK`]
/// across mutate→f→restore (including on panic) so no [`new_plane`] call on another
/// thread can race the mutation.
fn with_env(key: &str, val: Option<&str>, f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(key).ok();
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn poll_interval_zero_fails_construction_naming_the_var() {
    with_env("INVALIDATION_POLL_INTERVAL_MS", Some("0"), || {
        let err = InvalidationPlane::new("postgres://unused".to_string())
            .err()
            .expect("an explicit zero poll interval must fail construction");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("INVALIDATION_POLL_INTERVAL_MS"),
            "error should name the var: {msg}"
        );
    });
}

#[test]
fn poll_interval_absent_defaults() {
    with_env("INVALIDATION_POLL_INTERVAL_MS", None, || {
        let plane = InvalidationPlane::new("postgres://unused".to_string())
            .expect("absent poll interval must not fail construction");
        assert_eq!(plane.poll, DEFAULT_POLL);
    });
}

#[test]
fn poll_interval_malformed_falls_back_to_default() {
    // Mirrors `asyncevents::retention::Config`'s posture: malformed (not absent, not
    // zero) tolerates garbage rather than failing — the historical knob convention.
    with_env("INVALIDATION_POLL_INTERVAL_MS", Some("banana"), || {
        let plane = InvalidationPlane::new("postgres://unused".to_string())
            .expect("malformed poll interval must fall back, not fail");
        assert_eq!(plane.poll, DEFAULT_POLL);
    });
}

#[test]
fn callback_timeout_zero_fails_construction_naming_the_var() {
    with_env("INVALIDATION_CALLBACK_TIMEOUT_MS", Some("0"), || {
        let err = InvalidationPlane::new("postgres://unused".to_string())
            .err()
            .expect("an explicit zero callback timeout must fail construction");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("INVALIDATION_CALLBACK_TIMEOUT_MS"),
            "error should name the var: {msg}"
        );
    });
}

#[test]
fn callback_timeout_absent_defaults() {
    with_env("INVALIDATION_CALLBACK_TIMEOUT_MS", None, || {
        let plane = InvalidationPlane::new("postgres://unused".to_string())
            .expect("absent callback timeout must not fail construction");
        assert_eq!(plane.callback_timeout, DEFAULT_CALLBACK_TIMEOUT);
    });
}

#[test]
fn callback_timeout_malformed_falls_back_to_default() {
    with_env("INVALIDATION_CALLBACK_TIMEOUT_MS", Some("nope"), || {
        let plane = InvalidationPlane::new("postgres://unused".to_string())
            .expect("malformed callback timeout must fall back, not fail");
        assert_eq!(plane.callback_timeout, DEFAULT_CALLBACK_TIMEOUT);
    });
}
